//! R9 and R10 of the risk register, which are the reason this contract
//! exists separately at all: one stake must not back three raises, and one
//! project's failure must not reach another project's cover.

use crate::StakeRegistry;
use near_sdk::json_types::U128;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, AccountId, NearToken};

fn acc(name: &str) -> AccountId {
    name.parse().unwrap()
}

fn ctx(predecessor: &str, deposit: u128) {
    let mut b = VMContextBuilder::new();
    b.predecessor_account_id(acc(predecessor))
        .attached_deposit(NearToken::from_yoctonear(deposit));
    testing_env!(b.build());
}

fn registry() -> StakeRegistry {
    ctx("owner.testnet", 0);
    StakeRegistry::new(acc("usdt.testnet"), acc("projects.testnet"), acc("owner.testnet"))
}

/// Stake arrives through the stablecoin, so deposits are made as that account.
fn deposit(reg: &mut StakeRegistry, who: &str, amount: u128) {
    ctx("usdt.testnet", 0);
    reg.ft_on_transfer(acc(who), U128(amount), who.to_string());
}

#[test]
fn a_deposit_is_entirely_free_until_something_claims_it() {
    let mut reg = registry();
    deposit(&mut reg, "builder.testnet", 10_000_000);

    let p = reg.get_profile(acc("builder.testnet"));
    assert_eq!(p.total.0, 10_000_000);
    assert_eq!(p.allocated.0, 0);
    assert_eq!(p.free.0, 10_000_000);
}

#[test]
fn allocating_moves_stake_out_of_reach() {
    let mut reg = registry();
    deposit(&mut reg, "builder.testnet", 10_000_000);

    ctx("projects.testnet", 0);
    reg.allocate("m120".into(), acc("builder.testnet"), U128(6_000_000));

    let p = reg.get_profile(acc("builder.testnet"));
    assert_eq!(p.allocated.0, 6_000_000);
    assert_eq!(p.free.0, 4_000_000, "only what is left may back the next raise");
}

#[test]
#[should_panic(expected = "already promised to another project")]
fn one_stake_cannot_back_two_raises_beyond_what_it_holds() {
    // R9. Without this every investor on the second raise sees a cover
    // figure that is only true if the first project succeeds.
    let mut reg = registry();
    deposit(&mut reg, "builder.testnet", 10_000_000);

    ctx("projects.testnet", 0);
    reg.allocate("m120".into(), acc("builder.testnet"), U128(7_000_000));
    reg.allocate("yaba".into(), acc("builder.testnet"), U128(7_000_000));
}

#[test]
fn a_second_raise_fits_inside_what_is_left() {
    let mut reg = registry();
    deposit(&mut reg, "builder.testnet", 10_000_000);

    ctx("projects.testnet", 0);
    reg.allocate("m120".into(), acc("builder.testnet"), U128(6_000_000));
    reg.allocate("yaba".into(), acc("builder.testnet"), U128(4_000_000));

    let p = reg.get_profile(acc("builder.testnet"));
    assert_eq!(p.free.0, 0);
    assert_eq!(reg.get_allocation("m120".into(), acc("builder.testnet")).0, 6_000_000);
    assert_eq!(reg.get_allocation("yaba".into(), acc("builder.testnet")).0, 4_000_000);
}

#[test]
fn slashing_one_project_leaves_the_other_untouched() {
    // R10. The cap on a slash is not a safety margin — it is the whole
    // mechanism stopping one failure cascading into every open raise.
    let mut reg = registry();
    deposit(&mut reg, "builder.testnet", 10_000_000);

    ctx("projects.testnet", 0);
    reg.allocate("m120".into(), acc("builder.testnet"), U128(6_000_000));
    reg.allocate("yaba".into(), acc("builder.testnet"), U128(4_000_000));

    reg.slash("m120".into(), acc("builder.testnet"), U128(6_000_000), acc("holders.testnet"));

    assert_eq!(reg.get_allocation("m120".into(), acc("builder.testnet")).0, 0);
    assert_eq!(
        reg.get_allocation("yaba".into(), acc("builder.testnet")).0,
        4_000_000,
        "investors in yaba did nothing wrong and keep their cover"
    );

    let p = reg.get_profile(acc("builder.testnet"));
    assert_eq!(p.total.0, 4_000_000);
    assert_eq!(p.slashed_total.0, 6_000_000);
    assert_eq!(p.slash_count, 1);
}

#[test]
#[should_panic(expected = "only slash the stake allocated to it")]
fn a_project_cannot_slash_more_than_it_was_given() {
    let mut reg = registry();
    deposit(&mut reg, "builder.testnet", 10_000_000);
    ctx("projects.testnet", 0);
    reg.allocate("m120".into(), acc("builder.testnet"), U128(6_000_000));
    reg.allocate("yaba".into(), acc("builder.testnet"), U128(4_000_000));

    reg.slash("m120".into(), acc("builder.testnet"), U128(6_000_001), acc("holders.testnet"));
}

#[test]
#[should_panic(expected = "allocated to a live project")]
fn allocated_stake_cannot_be_withdrawn() {
    // R14. A guarantee the builder can take back once the money is raised
    // is not a guarantee.
    let mut reg = registry();
    deposit(&mut reg, "builder.testnet", 10_000_000);
    ctx("projects.testnet", 0);
    reg.allocate("m120".into(), acc("builder.testnet"), U128(6_000_000));

    ctx("builder.testnet", 1);
    reg.withdraw(U128(5_000_000));
}

#[test]
fn free_stake_can_be_withdrawn() {
    let mut reg = registry();
    deposit(&mut reg, "builder.testnet", 10_000_000);
    ctx("projects.testnet", 0);
    reg.allocate("m120".into(), acc("builder.testnet"), U128(6_000_000));

    ctx("builder.testnet", 1);
    reg.withdraw(U128(4_000_000));

    // The balance drops when the withdrawal is sent; the callback restores
    // it if the transfer fails.
    assert_eq!(reg.get_profile(acc("builder.testnet")).total.0, 6_000_000);
}

#[test]
fn releasing_returns_the_allocation_to_free() {
    let mut reg = registry();
    deposit(&mut reg, "builder.testnet", 10_000_000);
    ctx("projects.testnet", 0);
    reg.allocate("m120".into(), acc("builder.testnet"), U128(6_000_000));

    let returned = reg.release("m120".into(), acc("builder.testnet"));
    assert_eq!(returned.0, 6_000_000);
    assert_eq!(reg.get_profile(acc("builder.testnet")).free.0, 10_000_000);
}

#[test]
fn cover_is_reported_as_the_ratio_an_investor_is_shown() {
    // The card says "18% of your investment". That number comes from here,
    // not from the app, so what is read is what would be paid.
    let mut reg = registry();
    deposit(&mut reg, "builder.testnet", 20_000_000);
    ctx("projects.testnet", 0);
    reg.allocate("m120".into(), acc("builder.testnet"), U128(16_200_000));

    let bps = reg.cover_bps("m120".into(), acc("builder.testnet"), U128(90_000_000));
    assert_eq!(bps, 1_800, "16.2m against a 90m target is 18%");
}

#[test]
fn cover_never_reads_above_full() {
    let mut reg = registry();
    deposit(&mut reg, "builder.testnet", 20_000_000);
    ctx("projects.testnet", 0);
    reg.allocate("small".into(), acc("builder.testnet"), U128(20_000_000));
    assert_eq!(reg.cover_bps("small".into(), acc("builder.testnet"), U128(1_000_000)), 10_000);
}

#[test]
#[should_panic(expected = "Only the projects contract")]
fn nobody_else_can_allocate() {
    let mut reg = registry();
    deposit(&mut reg, "builder.testnet", 10_000_000);
    ctx("builder.testnet", 0);
    reg.allocate("m120".into(), acc("builder.testnet"), U128(1_000_000));
}

#[test]
#[should_panic(expected = "only takes its own stablecoin")]
fn stake_cannot_arrive_in_some_other_token() {
    let mut reg = registry();
    ctx("fake-usdt.testnet", 0);
    reg.ft_on_transfer(acc("builder.testnet"), U128(1_000_000), String::new());
}
