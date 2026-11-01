//! Letting the unit, through the marketplace that already exists.
//!
//! This is the whole of the integration, and it is deliberately thin. The
//! marketplace lets an owner list, accept a bid and release documents. The
//! vault is the owner. So the manager calls the vault and the vault makes the
//! same call under its own name. No marketplace code changes, and a property
//! held by three hundred people behaves exactly like one held by a landlord.
//!
//! The manager never holds the NFT and cannot move it. Everything here is a
//! forwarded instruction, not custody.

use crate::ext::{ext_market, GAS_FOR_MARKET_CALL};
use crate::{Custody, Vault, VaultExt};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, NearToken, Promise};

const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);

#[near]
impl Vault {
    fn assert_holding(&self) {
        require!(
            matches!(self.custody, Custody::Held { .. }),
            "This vault is not holding a unit"
        );
    }

    /// Put the unit on the market, or change its terms.
    #[payable]
    pub fn list_for_rent(&mut self, price: U128, lease_duration_days: u32) -> Promise {
        Self::assert_one_yocto();
        self.assert_manager();
        self.assert_holding();
        require!(lease_duration_days > 0, "A lease needs a length");

        env::log_str(&format!(
            "listing unit {} for rent at {} for {} days",
            self.property_id, price.0, lease_duration_days
        ));
        ext_market::ext(self.marketplace.clone())
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(GAS_FOR_MARKET_CALL)
            .update_listing(
                self.property_id,
                price.0.to_string(),
                false,
                Some(lease_duration_days),
            )
    }

    /// Take the unit off the market.
    #[payable]
    pub fn delist(&mut self) -> Promise {
        Self::assert_one_yocto();
        self.assert_manager();
        self.assert_holding();
        ext_market::ext(self.marketplace.clone())
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(GAS_FOR_MARKET_CALL)
            .delist_property(self.property_id)
    }

    /// Accept a tenant's bid.
    ///
    /// Rent lands in this vault as a result, silently, because the
    /// marketplace pays with plain `ft_transfer`. Call `sync_income`
    /// afterwards — or let anyone else do it, which is the point of leaving
    /// that method open.
    #[payable]
    pub fn accept_bid(&mut self, bid_id: U128) -> Promise {
        Self::assert_one_yocto();
        self.assert_manager();
        self.assert_holding();
        ext_market::ext(self.marketplace.clone())
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(GAS_FOR_MARKET_CALL)
            .accept_bid(bid_id.0 as u64, self.property_id)
    }

    #[payable]
    pub fn reject_bid(&mut self, bid_id: U128) -> Promise {
        Self::assert_one_yocto();
        self.assert_manager();
        self.assert_holding();
        ext_market::ext(self.marketplace.clone())
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(GAS_FOR_MARKET_CALL)
            .reject_bid(bid_id.0 as u64, self.property_id)
    }

    /// Hand over the tenancy agreement once the viewing has happened.
    #[payable]
    pub fn release_documents(
        &mut self,
        bid_id: U128,
        document_image_uri: String,
        document_description: String,
    ) -> Promise {
        Self::assert_one_yocto();
        self.assert_manager();
        self.assert_holding();
        ext_market::ext(self.marketplace.clone())
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(GAS_FOR_MARKET_CALL)
            .release_documents(
                bid_id.0 as u64,
                self.property_id,
                document_image_uri,
                document_description,
            )
    }
}
