//! Stake, held against a profile rather than against a deal.
//!
//! A builder who abandons one project should find it harder to raise for the
//! next. A deposit tied to a single raise cannot do that; stake attached to
//! an identity can, which is what turns collateral into reputation with a
//! number behind it. Managers stake here too, against their mandates.
//!
//! One accounting rule carries most of the weight. A builder with ten
//! million staked who opens three raises, each advertising eighteen percent
//! cover, has promised the same ten million three times, and every investor
//! sees a figure that is only true if the other two projects succeed. So the
//! balance is split three ways: what is held, what is already spoken for,
//! and what is left. A raise cannot advertise cover the free balance does
//! not support.
//!
//! The matching rule is that an allocation belongs to its project and to no
//! other. Slashing a failure on one touches that allocation only. Without
//! it, one bad project drains the cover out from under investors in a
//! different one who did nothing wrong, and a single failure cascades across
//! every raise a builder has open.

use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::collections::{LookupMap, UnorderedMap};
use near_sdk::json_types::U128;
use near_sdk::serde::{Deserialize, Serialize};
use near_sdk::{
    env, ext_contract, near, require, AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault,
    Promise, PromiseError,
};

const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
const GAS_FOR_FT_TRANSFER: Gas = Gas::from_tgas(15);
const GAS_FOR_RESOLVE: Gas = Gas::from_tgas(10);

#[ext_contract(ext_ft)]
pub trait FungibleToken {
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>);
}

#[ext_contract(ext_self)]
pub trait RegistryCallbacks {
    fn on_withdrawn(
        &mut self,
        who: AccountId,
        amount: U128,
        #[callback_result] result: Result<(), PromiseError>,
    );
}

#[derive(BorshSerialize, BorshStorageKey)]
#[borsh(crate = "near_sdk::borsh")]
enum StorageKey {
    Profiles,
    Allocations,
}

/// What one account has staked, and how much of it is spoken for.
#[derive(BorshSerialize, BorshDeserialize, Serialize, Deserialize, Clone, Debug, Default)]
#[borsh(crate = "near_sdk::borsh")]
#[serde(crate = "near_sdk::serde")]
pub struct Profile {
    /// Everything deposited and not yet withdrawn or slashed.
    pub total: u128,
    /// Reserved by live projects. Never withdrawable.
    pub allocated: u128,
    /// Taken for failures, kept as history. A profile does not get to forget.
    pub slashed_total: u128,
    pub slash_count: u32,
}

impl Profile {
    /// What a new raise may advertise against.
    pub fn free(&self) -> u128 {
        self.total.saturating_sub(self.allocated)
    }
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct StakeRegistry {
    /// The coin stake is held in. The same one the raise is denominated in,
    /// so cover and promise never drift apart on an exchange rate.
    pub stablecoin: AccountId,
    /// Allowed to allocate, release and slash. The projects contract.
    pub controller: AccountId,
    /// May replace the controller. Nothing else.
    pub owner: AccountId,

    profiles: UnorderedMap<AccountId, Profile>,
    /// (project, account) → amount, so a slash can only reach one project's
    /// share of a profile's stake.
    allocations: LookupMap<String, u128>,

    pub total_held: u128,
}

#[near]
impl StakeRegistry {
    #[init]
    pub fn new(stablecoin: AccountId, controller: AccountId, owner: AccountId) -> Self {
        Self {
            stablecoin,
            controller,
            owner,
            profiles: UnorderedMap::new(StorageKey::Profiles),
            allocations: LookupMap::new(StorageKey::Allocations),
            total_held: 0,
        }
    }

    // ---- deposits -------------------------------------------------------

    /// Stake arrives by `ft_transfer_call` on the stablecoin, so the deposit
    /// and the credit happen in one transaction and cannot come apart.
    ///
    /// `msg` is the account to credit, or empty for the sender. Crediting
    /// someone else is allowed: a company often funds a director's profile.
    pub fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> near_sdk::PromiseOrValue<U128> {
        require!(
            env::predecessor_account_id() == self.stablecoin,
            "This registry only takes its own stablecoin"
        );
        let who: AccountId = if msg.trim().is_empty() {
            sender_id
        } else {
            msg.trim()
                .parse()
                .unwrap_or_else(|_| env::panic_str("That is not an account id"))
        };

        let mut profile = self.profile_of(&who);
        profile.total += amount.0;
        self.profiles.insert(&who, &profile);
        self.total_held += amount.0;

        env::log_str(&format!("stake_deposited {} by {}", amount.0, who));
        // Nothing refunded.
        near_sdk::PromiseOrValue::Value(U128(0))
    }

    /// Take back stake that nothing is relying on.
    #[payable]
    pub fn withdraw(&mut self, amount: U128) -> Promise {
        assert_one_yocto();
        let who = env::predecessor_account_id();
        let mut profile = self.profile_of(&who);
        let amount = amount.0;

        require!(amount > 0, "Withdraw must be greater than zero");
        require!(
            amount <= profile.free(),
            "That stake is allocated to a live project and cannot be withdrawn"
        );

        profile.total -= amount;
        self.profiles.insert(&who, &profile);
        self.total_held -= amount;

        ext_ft::ext(self.stablecoin.clone())
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(GAS_FOR_FT_TRANSFER)
            .ft_transfer(who.clone(), U128(amount), None)
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_RESOLVE)
                    .on_withdrawn(who, U128(amount)),
            )
    }

    #[private]
    pub fn on_withdrawn(
        &mut self,
        who: AccountId,
        amount: U128,
        #[callback_result] result: Result<(), PromiseError>,
    ) -> bool {
        if result.is_ok() {
            return true;
        }
        // The tokens never left, so the stake must come back.
        let mut profile = self.profile_of(&who);
        profile.total += amount.0;
        self.profiles.insert(&who, &profile);
        self.total_held += amount.0;
        env::log_str(&format!(
            "withdrawal of {} by {} failed and was restored",
            amount.0, who
        ));
        false
    }

    // ---- allocation -----------------------------------------------------

    /// Reserve cover for a project. Refused unless the free balance carries
    /// it, which is what stops the same stake backing three raises at once.
    pub fn allocate(&mut self, project_id: String, who: AccountId, amount: U128) {
        self.assert_controller();
        let amount = amount.0;
        require!(amount > 0, "Allocation must be greater than zero");

        let mut profile = self.profile_of(&who);
        require!(
            amount <= profile.free(),
            "Not enough free stake. It is already promised to another project."
        );

        let key = Self::key(&project_id, &who);
        require!(
            self.allocations.get(&key).is_none(),
            "This project already holds an allocation from this profile"
        );

        profile.allocated += amount;
        self.profiles.insert(&who, &profile);
        self.allocations.insert(&key, &amount);
        env::log_str(&format!(
            "stake_allocated {} by {} to {}",
            amount, who, project_id
        ));
    }

    /// Give the allocation back when a project finishes or is wound up.
    pub fn release(&mut self, project_id: String, who: AccountId) -> U128 {
        self.assert_controller();
        let key = Self::key(&project_id, &who);
        let Some(amount) = self.allocations.get(&key) else {
            return U128(0);
        };

        let mut profile = self.profile_of(&who);
        profile.allocated = profile.allocated.saturating_sub(amount);
        self.profiles.insert(&who, &profile);
        self.allocations.remove(&key);

        env::log_str(&format!(
            "stake_released {} to {} from {}",
            amount, who, project_id
        ));
        U128(amount)
    }

    /// Take stake for a failure, and send it where the controller says.
    ///
    /// Only ever out of this project's own allocation. The cap is not a
    /// safety margin. It is the whole mechanism by which one project's
    /// failure cannot reach cover promised to investors in another.
    pub fn slash(
        &mut self,
        project_id: String,
        who: AccountId,
        amount: U128,
        to: AccountId,
    ) -> Promise {
        self.assert_controller();
        let key = Self::key(&project_id, &who);
        let allocated = self.allocations.get(&key).unwrap_or(0);
        let amount = amount.0;

        require!(amount > 0, "Slash must be greater than zero");
        require!(
            amount <= allocated,
            "A project may only slash the stake allocated to it"
        );

        let mut profile = self.profile_of(&who);
        profile.total = profile.total.saturating_sub(amount);
        profile.allocated = profile.allocated.saturating_sub(amount);
        profile.slashed_total += amount;
        profile.slash_count += 1;
        self.profiles.insert(&who, &profile);

        let left = allocated - amount;
        if left == 0 {
            self.allocations.remove(&key);
        } else {
            self.allocations.insert(&key, &left);
        }
        self.total_held -= amount;

        env::log_str(&format!(
            "stake_slashed {} from {} on {}",
            amount, who, project_id
        ));
        ext_ft::ext(self.stablecoin.clone())
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(GAS_FOR_FT_TRANSFER)
            .ft_transfer(to, U128(amount), Some(format!("slash: {}", project_id)))
    }

    // ---- admin ----------------------------------------------------------

    #[payable]
    pub fn set_controller(&mut self, controller: AccountId) {
        assert_one_yocto();
        require!(env::predecessor_account_id() == self.owner, "Owner only");
        self.controller = controller;
    }

    // ---- views ----------------------------------------------------------

    pub fn get_profile(&self, account_id: AccountId) -> ProfileView {
        let p = self.profile_of(&account_id);
        ProfileView {
            total: U128(p.total),
            allocated: U128(p.allocated),
            free: U128(p.free()),
            slashed_total: U128(p.slashed_total),
            slash_count: p.slash_count,
        }
    }

    pub fn get_allocation(&self, project_id: String, account_id: AccountId) -> U128 {
        U128(
            self.allocations
                .get(&Self::key(&project_id, &account_id))
                .unwrap_or(0),
        )
    }

    /// Cover as a proportion of a target, in basis points.
    ///
    /// This is the figure a project card shows, and it is computed here
    /// rather than in the app so that what an investor reads is what the
    /// chain would pay.
    pub fn cover_bps(&self, project_id: String, account_id: AccountId, target: U128) -> u32 {
        if target.0 == 0 {
            return 0;
        }
        let allocated = self
            .allocations
            .get(&Self::key(&project_id, &account_id))
            .unwrap_or(0);
        ((allocated * 10_000) / target.0).min(10_000) as u32
    }

    pub fn get_total_held(&self) -> U128 {
        U128(self.total_held)
    }

    // ---- internals ------------------------------------------------------

    fn profile_of(&self, who: &AccountId) -> Profile {
        self.profiles.get(who).unwrap_or_default()
    }

    fn key(project_id: &str, who: &AccountId) -> String {
        format!("{}|{}", project_id, who)
    }

    fn assert_controller(&self) {
        require!(
            env::predecessor_account_id() == self.controller,
            "Only the projects contract may do this"
        );
    }
}

fn assert_one_yocto() {
    require!(
        env::attached_deposit().as_yoctonear() == 1,
        "Requires an attached deposit of exactly 1 yoctoNEAR"
    );
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(crate = "near_sdk::serde")]
pub struct ProfileView {
    pub total: U128,
    pub allocated: U128,
    pub free: U128,
    pub slashed_total: U128,
    pub slash_count: u32,
}

#[cfg(test)]
mod tests;
