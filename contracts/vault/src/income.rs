//! Rent arriving, and rent leaving.
//!
//! Nobody is paid when rent arrives. The obvious implementation, looping
//! over the holders and paying each, stops working past a few dozen of them,
//! because a transaction has a gas ceiling and the loop eventually does not
//! fit inside it. It would pass every test and fail once the product
//! succeeded.
//!
//! So one number moves instead. `acc_per_share` records rent per share over
//! all time, and a holder works out their own entitlement from it when they
//! come to collect. The cost of crediting income is the same for ten holders
//! and ten thousand.
//!
//! The second thing this file handles is that the marketplace does not tell
//! us when it pays. Checked against the deployed bytecode of
//! market.shedauser.testnet: payouts are plain `ft_transfer`, there is no
//! standalone `ft_transfer_call`, and NEP-141 `ft_transfer` notifies nobody.
//! Money lands here silently, so `sync_income` goes and looks.

use crate::ext::{ext_ft, ext_self, GAS_FOR_FT_TRANSFER, GAS_FOR_RESOLVE};
use crate::{Vault, VaultExt, BPS, PRECISION};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas, NearToken, Promise, PromiseError};

const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
const GAS_FOR_BALANCE: Gas = Gas::from_tgas(10);
const GAS_FOR_SYNC_CB: Gas = Gas::from_tgas(15);

#[near]
impl Vault {
    /// Look at what the vault actually holds and credit anything new.
    ///
    /// Open to anyone on purpose. If this needed the manager, a manager who
    /// stopped calling it would freeze every holder out of their own rent;
    /// a holder can always come and do it themselves. A call with nothing to
    /// credit is harmless and costs only gas.
    pub fn sync_income(&mut self) -> Promise {
        require!(!self.busy, "A sync is already in flight");
        self.busy = true;

        // The figure the callback compares against is captured now. It cannot
        // read `last_accounted` later, because a claim settling in between
        // would have moved it, and the delta would then include money that
        // had already left.
        let snapshot = self.last_accounted;

        ext_ft::ext(self.stablecoin.clone())
            .with_static_gas(GAS_FOR_BALANCE)
            .ft_balance_of(env::current_account_id())
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_SYNC_CB)
                    .on_sync(U128(snapshot)),
            )
    }

    #[private]
    pub fn on_sync(
        &mut self,
        snapshot: U128,
        #[callback_result] balance: Result<U128, PromiseError>,
    ) -> U128 {
        self.busy = false;

        let Ok(balance) = balance else {
            env::log_str("sync_income: could not read the token balance");
            return U128(0);
        };

        let balance = balance.0;
        if balance <= snapshot.0 {
            return U128(0);
        }
        let received = balance - snapshot.0;

        // Everything that came in is accounted for, whether or not it reaches
        // the accumulator, so the next sync does not count it twice.
        self.last_accounted += received;

        let to_holders = self.split_fees(received);
        self.credit_holders(to_holders);

        env::log_str(&format!(
            "sync_income: received {}, {} to holders",
            received, to_holders
        ));
        U128(received)
    }

    /// Take the manager's cut, the reserve and the platform fee off the top.
    /// Returns what is left for holders.
    pub(crate) fn split_fees(&mut self, amount: u128) -> u128 {
        let manager = amount * (self.manager_bps as u128) / (BPS as u128);
        let reserve = amount * (self.reserve_bps as u128) / (BPS as u128);
        let platform = amount * (self.platform_bps as u128) / (BPS as u128);

        self.owed_manager += manager;
        self.reserve += reserve;
        self.owed_treasury += platform;

        // Subtraction rather than a fourth multiplication, so the parts
        // always add back to the whole however the rounding fell.
        amount - manager - reserve - platform
    }

    /// Move income into the accumulator.
    ///
    /// The remainder of the division is kept and added to the next credit,
    /// so repeated small payments do not quietly shave value off holders.
    pub(crate) fn credit_holders(&mut self, amount: u128) {
        if amount == 0 {
            return;
        }
        if self.total_supply == 0 {
            // No shares yet. Hold it rather than divide by zero; it joins the
            // first credit after the raise closes.
            self.dust += amount * PRECISION;
            return;
        }
        let scaled = amount * PRECISION + self.dust;
        self.acc_per_share += scaled / self.total_supply;
        self.dust = scaled % self.total_supply;
    }

    /// Withdraw everything owed to the caller.
    #[payable]
    pub fn claim(&mut self) -> Promise {
        Self::assert_one_yocto();
        let who = env::predecessor_account_id();

        self.settle(&who);
        let amount = self.unclaimed_of(&who);
        require!(amount > 0, "Nothing to claim");

        self.unclaimed.insert(&who, &0);
        self.last_accounted -= amount;

        self.pay(who, amount, PayKind::Holder)
    }

    /// The manager withdraws their accrued fee.
    #[payable]
    pub fn withdraw_manager_fee(&mut self) -> Promise {
        Self::assert_one_yocto();
        self.assert_manager();
        let amount = self.owed_manager;
        require!(amount > 0, "Nothing owed");
        self.owed_manager = 0;
        self.last_accounted -= amount;
        self.pay(self.manager.clone(), amount, PayKind::Manager)
    }

    /// The platform fee. Anyone may push it, since it only ever goes to the
    /// treasury and a fee nobody can collect is just a growing liability.
    pub fn withdraw_treasury_fee(&mut self) -> Promise {
        let amount = self.owed_treasury;
        require!(amount > 0, "Nothing owed");
        self.owed_treasury = 0;
        self.last_accounted -= amount;
        self.pay(self.treasury.clone(), amount, PayKind::Treasury)
    }

    /// Spend from the maintenance reserve.
    ///
    /// Capped per call so that ordinary repairs do not need a vote. Without a
    /// cap every blocked drain becomes a governance event, and nobody uses a
    /// system like that twice.
    #[payable]
    pub fn draw_reserve(&mut self, amount: U128, to: AccountId, reason: String) -> Promise {
        Self::assert_one_yocto();
        self.assert_manager();
        require!(!reason.trim().is_empty(), "Say what the money is for");
        let amount = amount.0;
        require!(amount > 0, "Draw must be greater than zero");
        require!(amount <= self.reserve, "More than the reserve holds");

        self.reserve -= amount;
        self.last_accounted -= amount;
        env::log_str(&format!(
            "draw_reserve: {} to {} for {}",
            amount, to, reason
        ));
        self.pay(to, amount, PayKind::Reserve)
    }

    fn pay(&mut self, to: AccountId, amount: u128, kind: PayKind) -> Promise {
        ext_ft::ext(self.stablecoin.clone())
            .with_attached_deposit(ONE_YOCTO)
            .with_static_gas(GAS_FOR_FT_TRANSFER)
            .ft_transfer(to.clone(), U128(amount), None)
            .then(
                ext_self::ext(env::current_account_id())
                    .with_static_gas(GAS_FOR_RESOLVE)
                    .on_paid(to, U128(amount), kind.as_str().to_string()),
            )
    }

    /// Put the money back if the transfer failed.
    ///
    /// Without this a failed payout leaves the holder's entitlement erased
    /// and the tokens still sitting in the vault. That is the one outcome
    /// worse than the payout simply not happening.
    #[private]
    pub fn on_paid(
        &mut self,
        to: AccountId,
        amount: U128,
        kind: String,
        #[callback_result] result: Result<(), PromiseError>,
    ) -> bool {
        if result.is_ok() {
            return true;
        }
        let amount = amount.0;
        self.last_accounted += amount;
        match kind.as_str() {
            "manager" => self.owed_manager += amount,
            "treasury" => self.owed_treasury += amount,
            "reserve" => self.reserve += amount,
            _ => {
                let restored = self.unclaimed_of(&to) + amount;
                self.unclaimed.insert(&to, &restored);
            }
        }
        env::log_str(&format!(
            "payout of {} to {} failed and was restored",
            amount, to
        ));
        false
    }
}

pub(crate) enum PayKind {
    Holder,
    Manager,
    Treasury,
    Reserve,
}

impl PayKind {
    fn as_str(&self) -> &'static str {
        match self {
            PayKind::Holder => "holder",
            PayKind::Manager => "manager",
            PayKind::Treasury => "treasury",
            PayKind::Reserve => "reserve",
        }
    }
}
