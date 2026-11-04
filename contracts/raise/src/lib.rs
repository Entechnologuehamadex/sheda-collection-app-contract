//! The money side of a raise: escrow, phase releases, refunds and opt-outs.
//!
//! A builder asks many people to fund a building that does not exist yet.
//! Everything here exists because that is an unusually easy thing to go
//! wrong, and because the ways it goes wrong are all variations on one
//! theme: money leaving before it was earned.
//!
//! Four rules carry the contract.
//!
//! **Nothing is released by one party.** The builder says a phase is
//! finished, an independent property manager inspects and attests, and
//! holders then have a week to object. A builder who could mark their own
//! phase complete would make the schedule decorative; a manager who could
//! release without the window would leave holders a vote they never get to
//! use. Neither can act alone, and the contract refuses an attestation from
//! the builder's own account outright.
//!
//! **Money is pulled, never pushed.** Refunds and share claims are called by
//! the person owed. Paying a list of investors in a loop works in a test and
//! stops working at the scale that means the product succeeded, because a
//! transaction has a gas ceiling. No function here iterates over investors.
//!
//! **Escrow is denominated in what arrived.** A phase releases a proportion
//! of the target, but never more than the escrow actually holds. A raise
//! that closed slightly short must not pay out as though it had not.
//!
//! **A failure has to cost the builder something.** The completion guarantee
//! is stake the builder posted, ring-fenced to this project in the registry.
//! If the build stalls, each investor pulls their share of it. A guarantee
//! the builder can walk away from is not a guarantee.

use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::collections::{LookupMap, UnorderedMap};
use near_sdk::json_types::{U128, U64};
use near_sdk::serde::{Deserialize, Serialize};
use near_sdk::{env, near, require, AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault};

pub mod commit;
pub mod ext;
pub mod phases;
pub mod stall;

#[cfg(test)]
mod tests;

/// Basis points denominator.
pub const BPS: u128 = 10_000;

/// How long holders have to object once a phase has been attested.
pub const OBJECTION_WINDOW_NS: u64 = 7 * 24 * 60 * 60 * 1_000_000_000;

/// Share of the raise that must object to hold a payment.
pub const OBJECTION_THRESHOLD_BPS: u32 = 2_500;

/// Ceiling on what Sheda may take of a completed raise.
pub const MAX_PLATFORM_BPS: u32 = 500;

pub const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
pub const GAS_FOR_FT_TRANSFER: Gas = Gas::from_tgas(15);
pub const GAS_FOR_RESOLVE: Gas = Gas::from_tgas(12);
pub const GAS_FOR_REGISTRY: Gas = Gas::from_tgas(25);
pub const GAS_FOR_MINT: Gas = Gas::from_tgas(20);

#[derive(BorshSerialize, BorshStorageKey)]
#[borsh(crate = "near_sdk::borsh")]
enum StorageKey {
    Projects,
    Commitments,
    ByInvestor,
    ByUnit,
    Objected,
    OptedOut,
}

/// Where a raise is in its life.
#[derive(
    BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq,
)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde")]
pub enum RaiseStatus {
    /// Taking money. The only state in which a commitment is accepted.
    Open,
    /// Target met and closed. Phases can now run.
    Funded,
    /// Every phase released.
    Delivered,
    /// Closed without reaching the target. Investors pull their money back.
    Failed,
    /// Declared stalled. Investors pull their share of the builder's cover.
    Stalled,
}

/// What a phase is waiting on.
///
/// The order is the sequence, and `release_phase` is the only thing that
/// advances past `ObjectionWindow`. There is no path from `Held` back to
/// released without governance, because an objection that a builder could
/// clear by waiting would be a formality.
#[derive(
    BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq,
)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde")]
pub enum PhaseState {
    /// The phase before this one has not been released.
    Locked,
    /// Being built. The builder's to finish.
    Building,
    /// The builder says it is done and has asked for an inspection.
    AwaitingInspection,
    /// The manager attested. Holders may object until the window closes.
    ObjectionWindow,
    /// Enough holders objected. The money stays put.
    Held,
    /// Paid to the builder.
    Released,
}

#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone, Debug)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde")]
pub struct Phase {
    pub name: String,
    /// Portion of the target this phase releases, in basis points.
    pub tranche_bps: u32,
    pub state: PhaseState,
    /// When the manager attested, in nanoseconds.
    pub attested_at_ns: u64,
    /// Money committed by the investors who have objected to this phase.
    pub objection_amount: u128,
}

#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone, Debug)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde")]
pub struct Project {
    pub builder: AccountId,
    /// Inspects and attests. Must not be the builder. See R2.
    pub manager: AccountId,
    pub stablecoin: AccountId,
    pub target: u128,
    pub raised: u128,
    /// Paid out to the builder so far.
    pub released: u128,
    /// Sheda's cut, taken once when the raise funds. Held apart from
    /// `released` because it is not the builder's and must not count
    /// against what they are still owed.
    pub fee_taken: u128,
    /// Returned to investors who opted out of a stalled build.
    pub refunded: u128,
    pub deadline_ns: u64,
    pub status: RaiseStatus,
    pub phases: Vec<Phase>,
    /// Sheda's cut of a completed raise. Charged once, on funding.
    pub platform_bps: u32,
    /// Cover the builder advertised, as a proportion of the target. It is
    /// backed by stake allocated to this project in the registry.
    pub guarantee_bps: u32,
}

impl Project {
    /// What the builder actually has to share out: everything that
    /// arrived, less Sheda's cut.
    pub fn distributable(&self) -> u128 {
        self.raised.saturating_sub(self.fee_taken)
    }

    /// What one phase pays.
    ///
    /// A share of the distributable total rather than of the target, so
    /// the platform fee is spread evenly across the schedule. Taking it
    /// off the target instead would pay the early phases in full and
    /// leave the last one absorbing the whole shortfall, which is the
    /// phase a builder can least afford to be short on.
    ///
    /// Capped at what is left regardless. If the two ever diverge, the
    /// alternative is a final transfer that cannot succeed and money
    /// with no state that frees it.
    pub fn tranche_value(&self, tranche_bps: u32) -> u128 {
        let pot = self.distributable();
        let nominal = pot * u128::from(tranche_bps) / BPS;
        nominal.min(pot.saturating_sub(self.released))
    }

    /// Share of the raise that has objected, in basis points.
    ///
    /// Defined once because the view a holder reads before objecting and
    /// the check that settles the phase have to be the same number. Two
    /// copies of the rule that decides whether money moves is one too
    /// many.
    pub fn objection_bps(&self, objection_amount: u128) -> u32 {
        (objection_amount * BPS)
            .checked_div(self.raised)
            .map(|share| share.min(BPS) as u32)
            .unwrap_or(0)
    }

    pub fn is_live(&self) -> bool {
        matches!(self.status, RaiseStatus::Open | RaiseStatus::Funded)
    }
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Raise {
    /// May create projects and declare a stall. The Sheda account.
    pub owner: AccountId,
    /// Where the platform fee goes.
    pub treasury: AccountId,
    /// Builder and manager stake. Empty until the registry is deployed.
    pub stake_registry: AccountId,

    projects: UnorderedMap<String, Project>,
    /// "{project}|{investor}|{unit}" -> amount committed.
    commitments: LookupMap<String, u128>,
    /// "{project}|{investor}" -> amount committed across every unit.
    by_investor: LookupMap<String, u128>,
    /// "{project}|{unit}" -> amount committed to that unit.
    by_unit: LookupMap<String, u128>,
    /// "{project}|{phase}|{investor}" -> has objected.
    objected: LookupMap<String, bool>,
    /// "{project}|{investor}" -> has already taken their money back.
    opted_out: LookupMap<String, bool>,
}

#[near]
impl Raise {
    #[init]
    pub fn new(owner: AccountId, treasury: AccountId, stake_registry: AccountId) -> Self {
        Self {
            owner,
            treasury,
            stake_registry,
            projects: UnorderedMap::new(StorageKey::Projects),
            commitments: LookupMap::new(StorageKey::Commitments),
            by_investor: LookupMap::new(StorageKey::ByInvestor),
            by_unit: LookupMap::new(StorageKey::ByUnit),
            objected: LookupMap::new(StorageKey::Objected),
            opted_out: LookupMap::new(StorageKey::OptedOut),
        }
    }

    // ---- creating a raise ---------------------------------------------

    /// Open a raise.
    ///
    /// Owner only, deliberately. Everything in here is a term investors
    /// will commit money against, and the backend has already checked the
    /// builder's profile, the documents and the manager's independence
    /// before it gets this far. A raise anyone could create on chain would
    /// route around all of it.
    #[allow(clippy::too_many_arguments)]
    pub fn create_project(
        &mut self,
        project_id: String,
        builder: AccountId,
        manager: AccountId,
        stablecoin: AccountId,
        target: U128,
        deadline_ns: U64,
        platform_bps: u32,
        guarantee_bps: u32,
        phases: Vec<(String, u32)>,
    ) {
        self.assert_owner();
        require!(
            self.projects.get(&project_id).is_none(),
            "A raise with that id already exists"
        );
        require!(target.0 > 0, "A raise needs a target above zero");
        require!(
            deadline_ns.0 > env::block_timestamp(),
            "The deadline has already passed"
        );
        require!(
            platform_bps <= MAX_PLATFORM_BPS,
            "The platform fee is above what this contract allows"
        );
        require!(
            guarantee_bps <= BPS as u32,
            "A guarantee cannot exceed the whole raise"
        );
        // R2. The party inspecting a phase decides whether the builder is
        // paid for it, so it cannot be the builder.
        require!(
            manager != builder,
            "A builder cannot be the manager who signs off their own phases"
        );
        require!(!phases.is_empty(), "A raise needs a phase schedule");

        let total: u32 = phases.iter().map(|(_, bps)| *bps).sum();
        require!(
            u128::from(total) == BPS,
            "The phase schedule must release exactly the whole raise"
        );

        let built = phases
            .into_iter()
            .enumerate()
            .map(|(i, (name, tranche_bps))| Phase {
                name,
                tranche_bps,
                // Only the first phase can be worked on. The rest wait on
                // the one before, which is what a release schedule means.
                state: if i == 0 {
                    PhaseState::Building
                } else {
                    PhaseState::Locked
                },
                attested_at_ns: 0,
                objection_amount: 0,
            })
            .collect();

        self.projects.insert(
            &project_id,
            &Project {
                builder,
                manager,
                stablecoin,
                target: target.0,
                raised: 0,
                released: 0,
                fee_taken: 0,
                refunded: 0,
                deadline_ns: deadline_ns.0,
                status: RaiseStatus::Open,
                phases: built,
                platform_bps,
                guarantee_bps,
            },
        );
        env::log_str(&format!("raise_opened {}", project_id));
    }

    // ---- views ---------------------------------------------------------

    pub fn get_project(&self, project_id: String) -> Option<ProjectView> {
        self.projects.get(&project_id).map(|p| ProjectView {
            builder: p.builder.clone(),
            manager: p.manager.clone(),
            stablecoin: p.stablecoin.clone(),
            target: U128(p.target),
            raised: U128(p.raised),
            released: U128(p.released),
            fee_taken: U128(p.fee_taken),
            distributable_view: U128(p.distributable()),
            refunded: U128(p.refunded),
            deadline_ns: U64(p.deadline_ns),
            status: p.status,
            platform_bps: p.platform_bps,
            guarantee_bps: p.guarantee_bps,
            phases: p.phases.clone(),
        })
    }

    /// Every raise, oldest first. Small enough to page in one call for now.
    pub fn list_projects(&self, from: Option<u64>, limit: Option<u64>) -> Vec<String> {
        let from = from.unwrap_or(0) as usize;
        let limit = limit.unwrap_or(50) as usize;
        self.projects.keys().skip(from).take(limit).collect()
    }

    /// What one investor put into one unit.
    pub fn get_commitment(&self, project_id: String, account_id: AccountId, unit: String) -> U128 {
        U128(
            self.commitments
                .get(&Self::unit_key(&project_id, &account_id, &unit))
                .unwrap_or(0),
        )
    }

    /// What one investor put into a raise, across every unit.
    pub fn get_total_commitment(&self, project_id: String, account_id: AccountId) -> U128 {
        U128(self.committed_by(&project_id, &account_id))
    }

    /// What a unit has taken so far.
    pub fn get_unit_raised(&self, project_id: String, unit: String) -> U128 {
        U128(
            self.by_unit
                .get(&Self::unit_total_key(&project_id, &unit))
                .unwrap_or(0),
        )
    }

    /// How much of the raise has objected to a phase, in basis points.
    ///
    /// Computed against money committed rather than headcount. Before the
    /// shares exist, what someone put in is the only measure of their
    /// stake in the decision, and it is the one the threshold is written
    /// against.
    pub fn get_objection_bps(&self, project_id: String, phase: u32) -> u32 {
        let Some(project) = self.projects.get(&project_id) else {
            return 0;
        };
        let Some(p) = project.phases.get(phase as usize) else {
            return 0;
        };
        project.objection_bps(p.objection_amount)
    }

    pub fn has_objected(&self, project_id: String, phase: u32, account_id: AccountId) -> bool {
        self.objected
            .get(&Self::objection_key(&project_id, phase, &account_id))
            .unwrap_or(false)
    }

    /// Whether someone has already taken their money or their cover out.
    pub fn has_opted_out_view(&self, project_id: String, account_id: AccountId) -> bool {
        self.has_opted_out(&project_id, &account_id)
    }

    pub fn get_owner(&self) -> AccountId {
        self.owner.clone()
    }

    pub fn get_stake_registry(&self) -> AccountId {
        self.stake_registry.clone()
    }

    // ---- admin ----------------------------------------------------------

    #[payable]
    pub fn set_stake_registry(&mut self, stake_registry: AccountId) {
        assert_one_yocto();
        self.assert_owner();
        self.stake_registry = stake_registry;
    }

    #[payable]
    pub fn set_treasury(&mut self, treasury: AccountId) {
        assert_one_yocto();
        self.assert_owner();
        self.treasury = treasury;
    }

    // ---- internals -------------------------------------------------------

    pub(crate) fn expect_project(&self, project_id: &str) -> Project {
        self.projects
            .get(&project_id.to_string())
            .unwrap_or_else(|| env::panic_str("No raise with that id"))
    }

    pub(crate) fn save(&mut self, project_id: &str, project: &Project) {
        self.projects.insert(&project_id.to_string(), project);
    }

    pub(crate) fn committed_by(&self, project_id: &str, who: &AccountId) -> u128 {
        self.by_investor
            .get(&Self::investor_key(project_id, who))
            .unwrap_or(0)
    }

    pub(crate) fn credit(&mut self, project_id: &str, who: &AccountId, unit: &str, amount: u128) {
        let unit_key = Self::unit_key(project_id, who, unit);
        let existing = self.commitments.get(&unit_key).unwrap_or(0);
        self.commitments.insert(&unit_key, &(existing + amount));

        let investor_key = Self::investor_key(project_id, who);
        let total = self.by_investor.get(&investor_key).unwrap_or(0);
        self.by_investor.insert(&investor_key, &(total + amount));

        let unit_total_key = Self::unit_total_key(project_id, unit);
        let unit_total = self.by_unit.get(&unit_total_key).unwrap_or(0);
        self.by_unit.insert(&unit_total_key, &(unit_total + amount));
    }

    pub(crate) fn clear_investor(&mut self, project_id: &str, who: &AccountId) {
        self.by_investor
            .remove(&Self::investor_key(project_id, who));
    }

    pub(crate) fn mark_objected(&mut self, project_id: &str, phase: u32, who: &AccountId) {
        self.objected
            .insert(&Self::objection_key(project_id, phase, who), &true);
    }

    pub(crate) fn mark_opted_out(&mut self, project_id: &str, who: &AccountId) {
        self.opted_out
            .insert(&Self::investor_key(project_id, who), &true);
    }

    pub(crate) fn has_opted_out(&self, project_id: &str, who: &AccountId) -> bool {
        self.opted_out
            .get(&Self::investor_key(project_id, who))
            .unwrap_or(false)
    }

    pub(crate) fn unmark_opted_out(&mut self, project_id: &str, who: &AccountId) {
        self.opted_out.remove(&Self::investor_key(project_id, who));
    }

    /// A project, or nothing. Used where a miss is an answer rather than a
    /// fault, such as rejecting a commitment aimed at a raise that is gone.
    pub(crate) fn projects_get(&self, project_id: &str) -> Option<Project> {
        self.projects.get(&project_id.to_string())
    }

    pub(crate) fn commitment_for(&self, project_id: &str, who: &AccountId, unit: &str) -> u128 {
        self.commitments
            .get(&Self::unit_key(project_id, who, unit))
            .unwrap_or(0)
    }

    pub(crate) fn has_objected_internal(
        &self,
        project_id: &str,
        phase: u32,
        who: &AccountId,
    ) -> bool {
        self.objected
            .get(&Self::objection_key(project_id, phase, who))
            .unwrap_or(false)
    }

    pub(crate) fn unit_key(project_id: &str, who: &AccountId, unit: &str) -> String {
        format!("{}|{}|{}", project_id, who, unit)
    }

    pub(crate) fn investor_key(project_id: &str, who: &AccountId) -> String {
        format!("{}|{}", project_id, who)
    }

    pub(crate) fn unit_total_key(project_id: &str, unit: &str) -> String {
        format!("{}|#{}", project_id, unit)
    }

    pub(crate) fn objection_key(project_id: &str, phase: u32, who: &AccountId) -> String {
        format!("{}|{}|{}", project_id, phase, who)
    }

    pub(crate) fn assert_owner(&self) {
        require!(
            env::predecessor_account_id() == self.owner,
            "Only Sheda may do this"
        );
    }
}

pub(crate) fn assert_one_yocto() {
    require!(
        env::attached_deposit().as_yoctonear() == 1,
        "Requires an attached deposit of exactly 1 yoctoNEAR"
    );
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(crate = "near_sdk::serde")]
pub struct ProjectView {
    pub builder: AccountId,
    pub manager: AccountId,
    pub stablecoin: AccountId,
    pub target: U128,
    pub raised: U128,
    pub released: U128,
    pub fee_taken: U128,
    /// Everything that arrived less Sheda's cut, which is what the
    /// phases actually divide between them.
    pub distributable_view: U128,
    pub refunded: U128,
    pub deadline_ns: U64,
    pub status: RaiseStatus,
    pub platform_bps: u32,
    pub guarantee_bps: u32,
    pub phases: Vec<Phase>,
}
