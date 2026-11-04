//! Releasing the money, one phase at a time.
//!
//! Three parties, in sequence, and no single one of them can move money:
//!
//!   the builder says a phase is finished
//!   the manager inspects it and attests, and cannot be the builder
//!   holders get seven days to object, and enough objections hold the money
//!
//! The last step is the one that is easy to leave out and the one that
//! matters most. Without it an investor's only recourse against a bad
//! inspection is to have chosen a different manager months earlier.

use crate::ext::{ext_ft, ext_self};
use crate::{
    assert_one_yocto, Phase, PhaseState, Raise, RaiseExt, RaiseStatus, BPS, GAS_FOR_FT_TRANSFER,
    GAS_FOR_RESOLVE, OBJECTION_THRESHOLD_BPS, OBJECTION_WINDOW_NS, ONE_YOCTO,
};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, Promise, PromiseError};

#[near]
impl Raise {
    /// The builder says a phase is done and asks for it to be inspected.
    #[payable]
    pub fn request_inspection(&mut self, project_id: String, phase: u32) {
        assert_one_yocto();
        let mut project = self.expect_project(&project_id);
        require!(
            env::predecessor_account_id() == project.builder,
            "Only the builder may ask for an inspection"
        );
        require!(
            project.status == RaiseStatus::Funded,
            "Phases run once the raise is funded"
        );

        let index = phase as usize;
        let current = Self::phase_at(&project.phases, index);
        require!(
            current.state == PhaseState::Building,
            "That phase is not one you are building right now"
        );

        project.phases[index].state = PhaseState::AwaitingInspection;
        self.save(&project_id, &project);
        env::log_str(&format!(
            "inspection_requested {} phase {}",
            project_id, phase
        ));
    }

    /// The manager inspects and signs off, opening the objection window.
    ///
    /// The builder check is repeated here even though `create_project`
    /// already refuses a manager who is the builder. The manager can be
    /// replaced later, and a rule enforced only at creation is a rule that
    /// holds until the first time someone changes something.
    #[payable]
    pub fn attest(&mut self, project_id: String, phase: u32) {
        assert_one_yocto();
        let mut project = self.expect_project(&project_id);
        let who = env::predecessor_account_id();
        require!(
            who == project.manager,
            "Only the property manager on this project may attest"
        );
        require!(
            who != project.builder,
            "Nobody may sign off their own construction"
        );
        require!(
            project.status == RaiseStatus::Funded,
            "Phases run once the raise is funded"
        );

        let index = phase as usize;
        let current = Self::phase_at(&project.phases, index);
        require!(
            current.state == PhaseState::AwaitingInspection,
            "That phase has not been submitted for inspection"
        );

        project.phases[index].state = PhaseState::ObjectionWindow;
        project.phases[index].attested_at_ns = env::block_timestamp();
        self.save(&project_id, &project);
        env::log_str(&format!(
            "phase_attested {} phase {} by {}",
            project_id, phase, who
        ));
    }

    /// An investor objecting to a release.
    ///
    /// Weighted by what they committed rather than by headcount. Before
    /// the shares exist that is the only measure of someone's stake in the
    /// decision, and counting heads would let a raise be held up by
    /// whoever can open the most accounts.
    #[payable]
    pub fn object(&mut self, project_id: String, phase: u32) {
        assert_one_yocto();
        let mut project = self.expect_project(&project_id);
        let who = env::predecessor_account_id();

        let index = phase as usize;
        let current = Self::phase_at(&project.phases, index);
        require!(
            current.state == PhaseState::ObjectionWindow,
            "That phase is not open to objections"
        );
        require!(
            env::block_timestamp() < current.attested_at_ns + OBJECTION_WINDOW_NS,
            "The window for objecting to this phase has closed"
        );

        let stake = self.committed_by(&project_id, &who);
        require!(
            stake > 0,
            "Only someone who committed to this raise may object"
        );
        require!(
            !self.has_objected_internal(&project_id, phase, &who),
            "You have already objected to this phase"
        );

        self.mark_objected(&project_id, phase, &who);
        project.phases[index].objection_amount += stake;
        self.save(&project_id, &project);

        env::log_str(&format!(
            "objection {} phase {} by {} weight {}",
            project_id, phase, who, stake
        ));
    }

    /// Settle a phase once its objection window has closed.
    ///
    /// Open to anyone. The builder is the one who wants the money, so they
    /// will normally call it, but leaving it to them alone would mean a
    /// phase that nobody settles sits in the objection window forever and
    /// the phase after it never unlocks.
    pub fn release_phase(&mut self, project_id: String, phase: u32) -> Option<Promise> {
        let mut project = self.expect_project(&project_id);
        let index = phase as usize;
        let current = Self::phase_at(&project.phases, index);

        require!(
            current.state == PhaseState::ObjectionWindow,
            "That phase is not waiting to be settled"
        );
        require!(
            env::block_timestamp() >= current.attested_at_ns + OBJECTION_WINDOW_NS,
            "Holders still have time to object to this phase"
        );

        let objected_bps = if project.raised == 0 {
            0
        } else {
            ((current.objection_amount * BPS) / project.raised).min(BPS) as u32
        };

        if objected_bps >= OBJECTION_THRESHOLD_BPS {
            project.phases[index].state = PhaseState::Held;
            self.save(&project_id, &project);
            env::log_str(&format!(
                "phase_held {} phase {} with {} bps objecting",
                project_id, phase, objected_bps
            ));
            return None;
        }

        let amount = project.tranche_value(current.tranche_bps);
        project.phases[index].state = PhaseState::Released;
        project.released += amount;

        // The next phase becomes the one being built. Doing this here,
        // rather than letting the builder pick, is what makes the schedule
        // an order rather than a list.
        if let Some(next) = project.phases.get_mut(index + 1) {
            if next.state == PhaseState::Locked {
                next.state = PhaseState::Building;
            }
        } else {
            project.status = RaiseStatus::Delivered;
        }
        self.save(&project_id, &project);

        env::log_str(&format!(
            "phase_released {} phase {} paying {}",
            project_id, phase, amount
        ));

        if amount == 0 {
            return None;
        }
        Some(
            ext_ft::ext(project.stablecoin.clone())
                .with_attached_deposit(ONE_YOCTO)
                .with_static_gas(GAS_FOR_FT_TRANSFER)
                .ft_transfer(
                    project.builder.clone(),
                    U128(amount),
                    Some(format!("{} phase {}", project_id, phase)),
                )
                .then(
                    ext_self::ext(env::current_account_id())
                        .with_static_gas(GAS_FOR_RESOLVE)
                        .on_tranche_paid(project_id, phase, U128(amount)),
                ),
        )
    }

    #[private]
    pub fn on_tranche_paid(
        &mut self,
        project_id: String,
        phase: u32,
        amount: U128,
        #[callback_result] result: Result<(), PromiseError>,
    ) -> bool {
        if result.is_ok() {
            return true;
        }

        // The transfer failed, so the money never left. Put the phase back
        // where it was and un-count it, or the escrow and the ledger
        // disagree for the rest of the raise.
        let mut project = self.expect_project(&project_id);
        let index = phase as usize;
        project.released = project.released.saturating_sub(amount.0);
        if let Some(p) = project.phases.get_mut(index) {
            p.state = PhaseState::ObjectionWindow;
        }
        if project.status == RaiseStatus::Delivered {
            project.status = RaiseStatus::Funded;
        }
        if let Some(next) = project.phases.get_mut(index + 1) {
            if next.state == PhaseState::Building {
                next.state = PhaseState::Locked;
            }
        }
        self.save(&project_id, &project);
        env::log_str(&format!(
            "tranche for {} phase {} failed and was reversed",
            project_id, phase
        ));
        false
    }

    pub(crate) fn phase_at(phases: &[Phase], index: usize) -> Phase {
        phases
            .get(index)
            .cloned()
            .unwrap_or_else(|| env::panic_str("No phase with that number"))
    }
}
