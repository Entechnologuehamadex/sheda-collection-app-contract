//! The arithmetic that decides who gets paid what.
//!
//! These are the rows of the risk register that can be proven without a
//! network: R5 (cost does not grow with holders), R6 (a buyer does not
//! collect income earned before they arrived), R23 (rounding loses nothing).

use crate::{Vault, BPS};
use near_sdk::json_types::{U128, U64};
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, AccountId, NearToken};

fn acc(name: &str) -> AccountId {
    name.parse().unwrap()
}

fn ctx(predecessor: AccountId, deposit: u128) {
    let mut b = VMContextBuilder::new();
    b.predecessor_account_id(predecessor)
        .attached_deposit(NearToken::from_yoctonear(deposit));
    testing_env!(b.build());
}

/// A vault with no fees, so the arithmetic under test is only the split.
fn vault() -> Vault {
    ctx(acc("gov.testnet"), 0);
    Vault::new(
        "M120 Unit 1B".to_string(),
        "M120-1B".to_string(),
        acc("market.testnet"),
        U64(13),
        acc("usdt.testnet"),
        acc("manager.testnet"),
        acc("gov.testnet"),
        acc("treasury.testnet"),
        0,
        0,
        0,
    )
}

fn vault_with_fees(manager_bps: u32, reserve_bps: u32, platform_bps: u32) -> Vault {
    ctx(acc("gov.testnet"), 0);
    Vault::new(
        "M120 Unit 1B".to_string(),
        "M120-1B".to_string(),
        acc("market.testnet"),
        U64(13),
        acc("usdt.testnet"),
        acc("manager.testnet"),
        acc("gov.testnet"),
        acc("treasury.testnet"),
        manager_bps,
        reserve_bps,
        platform_bps,
    )
}

#[test]
fn rent_splits_in_proportion_to_shares() {
    let mut v = vault();
    v.internal_mint(&acc("ada.testnet"), 7_000);
    v.internal_mint(&acc("bola.testnet"), 3_000);

    v.credit_holders(1_000_000);

    assert_eq!(v.claimable(acc("ada.testnet")).0, 700_000);
    assert_eq!(v.claimable(acc("bola.testnet")).0, 300_000);
}

#[test]
fn a_buyer_does_not_collect_income_earned_before_they_arrived() {
    // R6. The bug this guards is silent: it favours whoever trades most and
    // is noticed by the person it robbed.
    let mut v = vault();
    v.internal_mint(&acc("ada.testnet"), 10_000);

    // Ada holds alone while the first rent arrives.
    v.credit_holders(500_000);

    // Ada then sells half to Bola.
    v.internal_transfer(&acc("ada.testnet"), &acc("bola.testnet"), 5_000);

    assert_eq!(
        v.claimable(acc("ada.testnet")).0,
        500_000,
        "Ada keeps everything earned while she held it all"
    );
    assert_eq!(
        v.claimable(acc("bola.testnet")).0,
        0,
        "Bola earns nothing for a period before he held anything"
    );

    // The next month they split it evenly.
    v.credit_holders(400_000);
    assert_eq!(v.claimable(acc("ada.testnet")).0, 700_000);
    assert_eq!(v.claimable(acc("bola.testnet")).0, 200_000);
}

#[test]
fn a_seller_keeps_what_they_earned_before_selling_everything() {
    let mut v = vault();
    v.internal_mint(&acc("ada.testnet"), 1_000);
    v.credit_holders(90_000);
    v.internal_transfer(&acc("ada.testnet"), &acc("bola.testnet"), 1_000);

    assert_eq!(v.claimable(acc("ada.testnet")).0, 90_000);
    assert_eq!(v.ft_balance_of(acc("ada.testnet")).0, 0);
}

#[test]
fn rounding_never_pays_out_more_than_came_in() {
    // R23, and the half of it that actually matters. Three holders and an
    // amount that never divides by three: the accumulator carries its
    // remainder forward, but each holder's own division still truncates, and
    // that last fraction stays in the vault.
    //
    // So the invariant is not exact conservation. It is that the vault can
    // never owe more than it received, and that the shortfall stays bounded
    // instead of growing with the number of payments. A naive accumulator
    // that dropped its remainder would lose up to one unit *per payment*;
    // this loses under one unit per holder, once, however long it runs.
    let mut v = vault();
    let holders = ["a.testnet", "b.testnet", "c.testnet"];
    for h in holders {
        v.internal_mint(&acc(h), 1);
    }

    let payments = 100u128;
    let each = 10u128; // 10 / 3 never divides evenly
    for _ in 0..payments {
        v.credit_holders(each);
    }

    let received = payments * each;
    let distributed: u128 = holders.iter().map(|n| v.claimable(acc(n)).0).sum();

    assert!(
        distributed <= received,
        "the vault promised {} having received {}",
        distributed,
        received
    );
    assert!(
        received - distributed < holders.len() as u128,
        "shortfall of {} over {} payments is more than sub-unit truncation",
        received - distributed,
        payments
    );
}

#[test]
fn income_arriving_before_any_shares_exist_is_not_lost() {
    let mut v = vault();
    v.credit_holders(1_000);
    assert_eq!(v.acc_per_share, 0, "nothing to divide by yet");

    v.internal_mint(&acc("ada.testnet"), 100);
    v.credit_holders(0);
    // The held amount joins the first real credit.
    v.credit_holders(1_000);
    assert_eq!(v.claimable(acc("ada.testnet")).0, 2_000);
}

#[test]
fn fees_come_off_the_top_and_always_add_back_to_the_whole() {
    let mut v = vault_with_fees(800, 500, 100); // 8% manager, 5% reserve, 1% platform
    v.internal_mint(&acc("ada.testnet"), 1_000);

    let to_holders = v.split_fees(1_000_000);

    assert_eq!(v.owed_manager, 80_000);
    assert_eq!(v.reserve, 50_000);
    assert_eq!(v.owed_treasury, 10_000);
    assert_eq!(to_holders, 860_000);
    assert_eq!(
        v.owed_manager + v.reserve + v.owed_treasury + to_holders,
        1_000_000,
        "the parts reconstruct the whole whichever way rounding fell"
    );
}

#[test]
#[should_panic(expected = "more than 30% of rent")]
fn fees_cannot_be_set_high_enough_to_leave_holders_nothing() {
    vault_with_fees(2_000, 1_000, 500);
}

#[test]
#[should_panic(expected = "Share supply is frozen")]
fn no_shares_can_be_minted_once_the_raise_closes() {
    // R20. Dilution after people have bought is indistinguishable from theft.
    let mut v = vault();
    ctx(acc("gov.testnet"), 1);
    v.freeze_supply();
    v.mint_shares(acc("ada.testnet"), U128(1));
}

#[test]
fn ownership_is_reported_in_basis_points() {
    let mut v = vault();
    v.internal_mint(&acc("ada.testnet"), 1_250);
    v.internal_mint(&acc("bola.testnet"), 8_750);
    assert_eq!(v.ownership_bps(acc("ada.testnet")), 1_250);
    assert_eq!(v.ownership_bps(acc("bola.testnet")), 8_750);
    assert_eq!(
        v.ownership_bps(acc("ada.testnet")) + v.ownership_bps(acc("bola.testnet")),
        BPS
    );
}

#[test]
fn claiming_is_not_double_counted() {
    let mut v = vault();
    v.internal_mint(&acc("ada.testnet"), 100);
    v.credit_holders(1_000);

    // Settling twice must not pay twice.
    v.settle(&acc("ada.testnet"));
    v.settle(&acc("ada.testnet"));
    assert_eq!(v.claimable(acc("ada.testnet")).0, 1_000);
}

#[test]
fn a_thousand_holders_still_reconcile() {
    // R5. The point is less the assertion than that crediting income does no
    // work per holder at all: this runs in constant time regardless.
    let mut v = vault();
    for i in 0..1_000u32 {
        let who: AccountId = format!("h{}.testnet", i).parse().unwrap();
        v.internal_mint(&who, 10);
    }
    v.credit_holders(10_000_000);

    let total: u128 = (0..1_000u32)
        .map(|i| {
            let who: AccountId = format!("h{}.testnet", i).parse().unwrap();
            v.claimable(who).0
        })
        .sum();
    assert_eq!(total + v.dust / crate::PRECISION, 10_000_000);
}
