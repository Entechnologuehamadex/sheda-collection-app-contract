//! What happens when a building does not get built.
//!
//! This is the part of the product that is actually a guarantee rather than
//! a promise. A builder advertising eighteen percent cover has allocated
//! that much of their own stake to this project in the registry, and if the
//! build stalls each investor pulls their share of it directly.
//!
//! Two decisions here are worth stating because both could reasonably have
//! gone the other way.
//!
//! **Cover is a proportion of what you put in, not of what is left.** An
//! investor who committed a tenth of the raise recovers a tenth of the
//! cover. Dividing the pot among whoever asks first would reward speed, and
//! the people slowest to hear a project has failed are rarely the ones best
//! able to absorb it.
//!
//! **Declaring a stall is the owner's call and not the builder's.** It has
//! to be someone, and it cannot be the party it costs. The honest limit of
//! this design is that it makes Sheda a referee, which is why the state is
//! public, the slash is recorded against the builder's profile permanently,
//! and the escrow left over is still refundable on top.

use crate::ext::{ext_registry, ext_self};
use crate::{
    assert_one_yocto, Raise, RaiseExt, RaiseStatus, BPS, GAS_FOR_REGISTRY, GAS_FOR_RESOLVE,
};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, Promise};

#[near]
impl Raise {
    /// Declare that a funded raise has stopped making progress.
    ///
    /// Only reachable from `Funded`, which means money was taken and the
    /// building was not finished. A raise that never funded fails instead
    /// and refunds in full, with no need to touch anyone's stake.
    #[payable]
    pub fn declare_stalled(&mut self, project_id: String) {
        assert_one_yocto();
        self.assert_owner();
        let mut project = self.expect_project(&project_id);
        require!(
            project.status == RaiseStatus::Funded,
            "Only a funded raise can stall"
        );

        project.status = RaiseStatus::Stalled;
        self.save(&project_id, &project);
        env::log_str(&format!(
            "raise_stalled {} with {} of {} released",
            project_id, project.released, project.raised
        ));
    }

    /// What one investor would recover if they opted out now.
    ///
    /// A view, so somebody can see the number before signing anything. It
    /// is their share of the cover, which is deliberately not all of their
    /// money: the guarantee is a proportion of the raise, and the screens
    /// say so wherever the figure appears.
    pub fn opt_out_value(&self, project_id: String, account_id: near_sdk::AccountId) -> U128 {
        let Some(project) = self.projects_get(&project_id) else {
            return U128(0);
        };
        if project.raised == 0 || self.has_opted_out(&project_id, &account_id) {
            return U128(0);
        }
        let committed = self.committed_by(&project_id, &account_id);
        U128(committed * u128::from(project.guarantee_bps) / BPS)
    }

    /// Take your share of the builder's cover out of a stalled build.
    ///
    /// Paid from the builder's stake, not from the escrow. The two are
    /// separate on purpose: whatever is still sitting in escrow belongs to
    /// the investors regardless, and paying cover out of it would be
    /// handing people their own money back and calling it a guarantee.
    #[payable]
    pub fn opt_out(&mut self, project_id: String) -> Promise {
        assert_one_yocto();
        let project = self.expect_project(&project_id);
        require!(
            project.status == RaiseStatus::Stalled,
            "Opting out is only open on a build that has been declared stalled"
        );
        require!(
            project.guarantee_bps > 0,
            "This raise was advertised with no cover behind it"
        );

        let who = env::predecessor_account_id();
        require!(
            !self.has_opted_out(&project_id, &who),
            "You have already opted out of this raise"
        );

        let committed = self.committed_by(&project_id, &who);
        require!(committed > 0, "You did not commit to this raise");

        let amount = committed * u128::from(project.guarantee_bps) / BPS;
        require!(amount > 0, "Your share of the cover rounds to nothing");

        // Marked before the call, restored by the callback if the registry
        // refuses. The other order leaves a window in which a second call
        // can be made while the first is still in flight.
        self.mark_opted_out(&project_id, &who);

        ext_registry::ext(self.stake_registry.clone())
            .with_static_gas(GAS_FOR_REGISTRY)
            .slash(
                project_id.clone(),
                project.builder.clone(),
                U128(amount),
                who.clone(),
            )
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_RESOLVE)
                    .on_refunded(project_id, who, U128(amount)),
            )
    }

    /// Reserve the builder's cover against this raise.
    ///
    /// Called by Sheda when a raise opens, before anyone is shown a cover
    /// figure. The registry refuses it unless the builder's free balance
    /// carries it, which is what stops the same stake backing three raises
    /// and three sets of investors each being shown a guarantee that only
    /// holds if the other two succeed.
    #[payable]
    pub fn allocate_cover(&mut self, project_id: String) -> Promise {
        assert_one_yocto();
        self.assert_owner();
        let project = self.expect_project(&project_id);
        require!(project.guarantee_bps > 0, "This raise advertises no cover");

        let amount = project.target * u128::from(project.guarantee_bps) / BPS;
        ext_registry::ext(self.stake_registry.clone())
            .with_static_gas(GAS_FOR_REGISTRY)
            .allocate(project_id, project.builder.clone(), U128(amount))
    }

    /// Give the builder their stake back once a raise is delivered.
    #[payable]
    pub fn release_cover(&mut self, project_id: String) -> Promise {
        assert_one_yocto();
        self.assert_owner();
        let project = self.expect_project(&project_id);
        require!(
            project.status == RaiseStatus::Delivered,
            "Cover is only released once the building is delivered"
        );

        ext_registry::ext(self.stake_registry.clone())
            .with_static_gas(GAS_FOR_REGISTRY)
            .release(project_id, project.builder.clone())
    }
}
