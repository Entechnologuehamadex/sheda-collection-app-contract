//! Taking the unit in, and letting it out again.
//!
//! The way out matters as much as the way in. Someone who acquires every
//! share can redeem: the shares burn, the NFT leaves, and they become an
//! ordinary landlord the marketplace already knows how to deal with. That
//! door is what keeps fractional ownership from being a trap.

use crate::ext::{ext_market, ext_nft, ext_self, PropertyView, GAS_FOR_NFT_TRANSFER, GAS_FOR_VIEW};
use crate::{Custody, Vault, VaultExt};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas, NearToken, Promise, PromiseError};

const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
const GAS_FOR_REDEEM_CB: Gas = Gas::from_tgas(40);
const GAS_FOR_SENT_CB: Gas = Gas::from_tgas(10);

#[near]
impl Vault {
    /// Take custody of the unit.
    ///
    /// NEP-171 calls this on the receiver when an NFT arrives by
    /// `nft_transfer_call`, so the vault is told — unlike money, which
    /// arrives silently. Returning "0" keeps the token; anything else hands
    /// it back, which is what every rejection below does.
    pub fn nft_on_transfer(
        &mut self,
        sender_id: AccountId,
        previous_owner_id: AccountId,
        token_id: String,
        msg: String,
    ) -> near_sdk::PromiseOrValue<bool> {
        require!(
            env::predecessor_account_id() == self.marketplace,
            "This vault only accepts units from its own marketplace"
        );
        require!(
            matches!(self.custody, Custody::AwaitingUnit),
            "This vault already holds a unit"
        );
        // The property id is fixed when the vault is created, so a unit for a
        // different flat cannot be deposited into the wrong vault's holders.
        require!(
            token_id == self.property_id.to_string(),
            "That unit does not belong to this vault"
        );

        self.custody = Custody::Held {
            token_id: token_id.clone(),
        };
        env::log_str(&format!(
            "unit {} received from {} (previously {}) msg: {}",
            token_id, sender_id, previous_owner_id, msg
        ));

        // false == we are keeping it.
        near_sdk::PromiseOrValue::Value(false)
    }

    /// Withdraw the unit, having acquired all of it.
    ///
    /// Refused while the flat is let. A redemption mid-tenancy would hand the
    /// NFT to someone who was not party to the lease and leave the tenant's
    /// deposit sitting against a vault that no longer owns anything.
    #[payable]
    pub fn redeem(&mut self) -> Promise {
        Self::assert_one_yocto();
        let who = env::predecessor_account_id();

        let Custody::Held { token_id } = self.custody.clone() else {
            env::panic_str("This vault is not holding a unit");
        };
        require!(!self.busy, "Another operation is in flight");

        let held = self.balance_of_internal(&who);
        require!(self.total_supply > 0, "No shares exist");
        require!(
            held == self.total_supply,
            "Redeeming takes every share of this unit"
        );

        self.busy = true;
        ext_market::ext(self.marketplace.clone())
            .with_static_gas(GAS_FOR_VIEW)
            .get_property_by_id(self.property_id)
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_REDEEM_CB)
                    .on_redeem_checked(who, token_id),
            )
    }

    #[private]
    pub fn on_redeem_checked(
        &mut self,
        to: AccountId,
        token_id: String,
        #[callback_result] property: Result<Option<PropertyView>, PromiseError>,
    ) -> Option<Promise> {
        self.busy = false;

        let Ok(property) = property else {
            env::log_str("redeem: could not read the property, nothing changed");
            return None;
        };
        // No record of the property is not a reason to release it. Treat an
        // unreadable state as a reason to stop, not to proceed.
        let Some(property) = property else {
            env::log_str("redeem: the marketplace has no such property, nothing changed");
            return None;
        };
        if property.active_lease.as_ref().is_some_and(|l| l.active) {
            env::log_str("redeem: refused, the unit is currently let");
            return None;
        }

        // Burn first. If the transfer then fails, `on_unit_sent` puts the
        // shares back; the other order would leave a window where both the
        // shares and the unit existed.
        let shares = self.balance_of_internal(&to);
        self.internal_burn(&to, shares);

        Some(
            ext_nft::ext(self.marketplace.clone())
                .with_attached_deposit(ONE_YOCTO)
                .with_static_gas(GAS_FOR_NFT_TRANSFER)
                .nft_transfer(
                    to.clone(),
                    token_id.clone(),
                    None,
                    Some("vault redemption".to_string()),
                )
                .then(
                    ext_self::ext(env::current_account_id())
                        .with_static_gas(GAS_FOR_SENT_CB)
                        .on_unit_sent(to, token_id, U128(shares)),
                ),
        )
    }

    #[private]
    pub fn on_unit_sent(
        &mut self,
        to: AccountId,
        token_id: String,
        shares: U128,
        #[callback_result] result: Result<(), PromiseError>,
    ) -> bool {
        if result.is_ok() {
            self.custody = Custody::Redeemed {
                token_id,
                to: to.clone(),
            };
            env::log_str(&format!("unit redeemed to {}", to));
            return true;
        }
        // The unit did not move, so the holder must get their shares back.
        self.internal_mint(&to, shares.0);
        env::log_str("redeem: transfer failed, shares restored");
        false
    }
}
