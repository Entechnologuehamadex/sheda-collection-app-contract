//! Shares in the vault: a fungible token whose balance earns rent.
//!
//! The whole of the care in this file is one rule. A balance may not move
//! until both sides have been settled against the accumulator. Miss it and
//! someone who buys shares on Monday collects rent that accrued in March —
//! a silent bug, favouring whoever trades most, found by the person it robbed.

use crate::{Vault, VaultExt, BPS, PRECISION};
use near_sdk::json_types::U128;
use near_sdk::serde::{Deserialize, Serialize};
use near_sdk::{env, near, require, AccountId};

impl Vault {
    pub(crate) fn balance_of_internal(&self, account_id: &AccountId) -> u128 {
        self.balances.get(account_id).unwrap_or(0)
    }

    pub(crate) fn credited_of(&self, account_id: &AccountId) -> u128 {
        self.credited.get(account_id).unwrap_or(0)
    }

    pub(crate) fn unclaimed_of(&self, account_id: &AccountId) -> u128 {
        self.unclaimed.get(account_id).unwrap_or(0)
    }

    /// What this balance has earned since it was last settled.
    ///
    /// Saturating rather than checked: `credited` is always set from the same
    /// expression, so it can only exceed `entitled` through a rounding step,
    /// and the honest answer there is zero rather than a panic that would
    /// freeze a holder out of their own money.
    pub(crate) fn pending_of(&self, account_id: &AccountId) -> u128 {
        let entitled = self.balance_of_internal(account_id) * self.acc_per_share / PRECISION;
        entitled.saturating_sub(self.credited_of(account_id))
    }

    /// Move everything this balance has earned into `unclaimed`, then mark it
    /// settled at the current accumulator. Call before any balance change.
    pub(crate) fn settle(&mut self, account_id: &AccountId) {
        let pending = self.pending_of(account_id);
        if pending > 0 {
            let total = self.unclaimed_of(account_id) + pending;
            self.unclaimed.insert(account_id, &total);
        }
        self.mark_credited(account_id);
    }

    /// Re-baseline an account against the accumulator at its current balance.
    /// Must follow every balance change, or the new balance would be treated
    /// as though it had been held all along.
    pub(crate) fn mark_credited(&mut self, account_id: &AccountId) {
        let entitled = self.balance_of_internal(account_id) * self.acc_per_share / PRECISION;
        self.credited.insert(account_id, &entitled);
    }

    pub(crate) fn internal_transfer(&mut self, from: &AccountId, to: &AccountId, amount: u128) {
        require!(amount > 0, "Transfer amount must be greater than zero");
        require!(from != to, "Cannot transfer to yourself");

        // Both sides first, while the balances are still the ones that earned.
        self.settle(from);
        self.settle(to);

        let from_balance = self.balance_of_internal(from);
        require!(from_balance >= amount, "Not enough shares");
        let to_balance = self.balance_of_internal(to);

        self.balances.insert(from, &(from_balance - amount));
        self.balances.insert(to, &(to_balance + amount));

        // Re-baseline both at their new balances.
        self.mark_credited(from);
        self.mark_credited(to);
    }

    pub(crate) fn internal_mint(&mut self, to: &AccountId, amount: u128) {
        require!(amount > 0, "Mint amount must be greater than zero");
        self.settle(to);
        let balance = self.balance_of_internal(to);
        self.balances.insert(to, &(balance + amount));
        self.total_supply += amount;
        self.mark_credited(to);
    }

    pub(crate) fn internal_burn(&mut self, from: &AccountId, amount: u128) {
        require!(amount > 0, "Burn amount must be greater than zero");
        self.settle(from);
        let balance = self.balance_of_internal(from);
        require!(balance >= amount, "Not enough shares");
        self.balances.insert(from, &(balance - amount));
        self.total_supply -= amount;
        self.mark_credited(from);
    }
}

#[near]
impl Vault {
    /// Issue shares. Only before the supply is frozen, which happens when the
    /// raise that funded this unit closes — after that no share may ever be
    /// created, so nobody can be diluted by the party who sold to them.
    #[payable]
    pub fn mint_shares(&mut self, to: AccountId, amount: U128) {
        Self::assert_one_yocto();
        self.assert_governor();
        require!(!self.supply_frozen, "Share supply is frozen");
        self.internal_mint(&to, amount.0);
    }

    /// Close the supply for good. There is deliberately no way back.
    #[payable]
    pub fn freeze_supply(&mut self) {
        Self::assert_one_yocto();
        self.assert_governor();
        self.supply_frozen = true;
    }

    // ---- NEP-141 core ---------------------------------------------------

    #[payable]
    pub fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        Self::assert_one_yocto();
        let sender = env::predecessor_account_id();
        self.internal_transfer(&sender, &receiver_id, amount.0);
        if let Some(memo) = memo {
            env::log_str(&format!("Transfer memo: {}", memo));
        }
    }

    pub fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        U128(self.balance_of_internal(&account_id))
    }

    pub fn ft_total_supply(&self) -> U128 {
        U128(self.total_supply)
    }

    pub fn ft_metadata(&self) -> ShareMetadata {
        ShareMetadata {
            spec: "ft-1.0.0".to_string(),
            name: self.name.clone(),
            symbol: self.symbol.clone(),
            icon: None,
            reference: None,
            reference_hash: None,
            // Shares are whole things. A tenth of a share is not a smaller
            // claim on the flat, it is a confusing way to write a smaller
            // number of shares, so the unit is indivisible.
            decimals: 0,
        }
    }

    /// What proportion of the unit an account holds, in basis points.
    /// Useful to the app, which otherwise does this division itself and gets
    /// to disagree with the contract about it.
    pub fn ownership_bps(&self, account_id: AccountId) -> u32 {
        if self.total_supply == 0 {
            return 0;
        }
        let bps = self.balance_of_internal(&account_id) * (BPS as u128) / self.total_supply;
        bps as u32
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(crate = "near_sdk::serde")]
pub struct ShareMetadata {
    pub spec: String,
    pub name: String,
    pub symbol: String,
    pub icon: Option<String>,
    pub reference: Option<String>,
    pub reference_hash: Option<String>,
    pub decimals: u8,
}
