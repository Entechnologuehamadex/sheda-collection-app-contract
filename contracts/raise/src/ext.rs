//! The contracts this one talks to.
//!
//! Every signature here was written against the other contract's source
//! rather than from memory. Two bindings in the vault were wrong in exactly
//! this way once: one method had gained an argument and another had been
//! renamed, and both compiled perfectly and would have failed at runtime,
//! because a cross-contract call is a string and a JSON blob that nothing
//! type checks until it arrives.

use near_sdk::json_types::U128;
use near_sdk::{ext_contract, AccountId, PromiseError};

#[ext_contract(ext_ft)]
pub trait FungibleToken {
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>);
}

/// The stake registry. See `contracts/stake-registry/src/lib.rs`.
#[ext_contract(ext_registry)]
pub trait StakeRegistry {
    /// Reserve cover for a project. Refused unless the free balance carries it.
    fn allocate(&mut self, project_id: String, who: AccountId, amount: U128);
    /// Give the allocation back when a project finishes or is wound up.
    fn release(&mut self, project_id: String, who: AccountId) -> U128;
    /// Take stake for a failure, and send it where the controller says.
    fn slash(&mut self, project_id: String, who: AccountId, amount: U128, to: AccountId);
}

/// One unit's vault. See `contracts/vault/src/shares.rs`.
#[ext_contract(ext_vault)]
pub trait Vault {
    fn mint_shares(&mut self, to: AccountId, amount: U128);
    fn freeze_supply(&mut self);
}

#[ext_contract(ext_self)]
pub trait RaiseCallbacks {
    fn on_tranche_paid(
        &mut self,
        project_id: String,
        phase: u32,
        amount: U128,
        #[callback_result] result: Result<(), PromiseError>,
    ) -> bool;

    fn on_refunded(
        &mut self,
        project_id: String,
        who: AccountId,
        amount: U128,
        #[callback_result] result: Result<(), PromiseError>,
    ) -> bool;

    fn on_fee_paid(
        &mut self,
        project_id: String,
        amount: U128,
        #[callback_result] result: Result<(), PromiseError>,
    ) -> bool;
}
