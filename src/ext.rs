//! The contracts this vault talks to.
//!
//! Three of them: the stablecoin rent arrives in, the marketplace the unit is
//! let through, and itself for callbacks. The marketplace interface here is
//! transcribed from the deployed contract and must not drift from it.

use near_sdk::json_types::{U128, U64};
use near_sdk::{ext_contract, AccountId, Gas, PromiseError};

pub const GAS_FOR_FT_TRANSFER: Gas = Gas::from_tgas(15);
pub const GAS_FOR_RESOLVE: Gas = Gas::from_tgas(10);
pub const GAS_FOR_MARKET_CALL: Gas = Gas::from_tgas(80);
pub const GAS_FOR_NFT_TRANSFER: Gas = Gas::from_tgas(20);
pub const GAS_FOR_VIEW: Gas = Gas::from_tgas(15);

#[ext_contract(ext_ft)]
pub trait FungibleToken {
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>);
    fn ft_balance_of(&self, account_id: AccountId) -> U128;
}

#[ext_contract(ext_nft)]
pub trait NonFungibleToken {
    fn nft_transfer(
        &mut self,
        receiver_id: AccountId,
        token_id: String,
        approval_id: Option<u64>,
        memo: Option<String>,
    );
}

/// The Sheda rental marketplace, as deployed.
///
/// The vault owns the property, so it is the account the marketplace expects
/// these calls from. The manager asks the vault; the vault asks the
/// marketplace. Nothing in the marketplace changes to allow it.
///
/// Every signature below is transcribed from the deployed contract's own ABI,
/// read off `market.shedauser.testnet` rather than from memory. That mattered:
/// `update_listing` takes a fifth argument this originally omitted, and the
/// document call is named `confirm_document_release`, not `release_documents`.
/// Both would have compiled perfectly and failed at runtime — the first on
/// deserialisation, the second on a method that does not exist.
///
/// If the marketplace is ever redeployed, re-read the ABI before trusting
/// this file.
#[ext_contract(ext_market)]
pub trait Marketplace {
    fn update_listing(
        &mut self,
        property_id: u64,
        price: String,
        is_for_sale: bool,
        lease_duration_days: Option<u32>,
        damage_deposit_required: Option<String>,
    );
    fn delist_property(&mut self, property_id: u64);
    fn accept_bid(&mut self, bid_id: u64, property_id: u64);
    fn reject_bid(&mut self, bid_id: u64, property_id: u64);
    fn confirm_document_release(
        &mut self,
        bid_id: u64,
        property_id: u64,
        document_image_uri: String,
        document_description: String,
    );
    fn get_property_by_id(&self, property_id: u64) -> Option<PropertyView>;
}

#[ext_contract(ext_self)]
pub trait VaultCallbacks {
    fn on_sync(&mut self, snapshot: U128, #[callback_result] balance: Result<U128, PromiseError>);
    fn on_paid(
        &mut self,
        to: AccountId,
        amount: U128,
        kind: String,
        #[callback_result] result: Result<(), PromiseError>,
    );
    fn on_redeem_checked(
        &mut self,
        to: AccountId,
        token_id: String,
        #[callback_result] property: Result<Option<PropertyView>, PromiseError>,
    );
    fn on_unit_sent(&mut self, to: AccountId, token_id: String, shares: U128);
}

/// Only the parts of the marketplace's property view this vault reads.
///
/// Deliberately partial: deserialising fields we do not use would make the
/// vault fail whenever the marketplace adds one.
#[derive(near_sdk::serde::Serialize, near_sdk::serde::Deserialize, Clone, Debug)]
#[serde(crate = "near_sdk::serde")]
pub struct PropertyView {
    pub id: U64,
    pub owner_id: String,
    pub active_lease: Option<LeaseView>,
}

#[derive(near_sdk::serde::Serialize, near_sdk::serde::Deserialize, Clone, Debug)]
#[serde(crate = "near_sdk::serde")]
pub struct LeaseView {
    pub id: U64,
    pub active: bool,
    pub escrow_held: String,
}
