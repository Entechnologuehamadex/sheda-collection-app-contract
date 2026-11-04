//! The rules that decide whether money moves.
//!
//! Everything here is reachable without a network, which is the whole
//! reason it is worth having: the parts that need a sandbox are the
//! transfers, and the parts that decide whether a transfer should happen
//! at all are these.

use crate::{Phase, PhaseState, Raise, RaiseStatus, OBJECTION_THRESHOLD_BPS, OBJECTION_WINDOW_NS};
use near_sdk::json_types::{U128, U64};
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, AccountId, NearToken};

const DAY: u64 = 24 * 60 * 60 * 1_000_000_000;

fn acc(name: &str) -> AccountId {
    name.parse().unwrap()
}

fn ctx(predecessor: &str, deposit: u128, now: u64) {
    let mut b = VMContextBuilder::new();
    b.predecessor_account_id(acc(predecessor))
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .block_timestamp(now);
    testing_env!(b.build());
}

fn raise() -> Raise {
    ctx("sheda.testnet", 0, DAY);
    Raise::new(
        acc("sheda.testnet"),
        acc("treasury.testnet"),
        acc("stake.testnet"),
    )
}

/// A raise with four equal phases, open for thirty days.
fn with_project(r: &mut Raise) {
    ctx("sheda.testnet", 0, DAY);
    r.create_project(
        "m120".into(),
        acc("builder.testnet"),
        acc("manager.testnet"),
        acc("usdt.testnet"),
        U128(1_000_000),
        U64(DAY + 30 * DAY),
        200,
        1_800,
        vec![
            ("Foundation".into(), 2_500),
            ("Frame".into(), 2_500),
            ("Roofing".into(), 2_500),
            ("Handover".into(), 2_500),
        ],
    );
}

/// Commit as an investor, the way the stablecoin would.
fn commit(r: &mut Raise, who: &str, unit: &str, amount: u128, now: u64) {
    ctx("usdt.testnet", 0, now);
    let msg = format!(r#"{{"project":"m120","unit":"{}"}}"#, unit);
    let _ = r.ft_on_transfer(acc(who), U128(amount), msg);
}

fn fund(r: &mut Raise) {
    with_project(r);
    commit(r, "ada.testnet", "1a", 600_000, DAY + 1);
    commit(r, "bode.testnet", "1b", 400_000, DAY + 1);
    ctx("anyone.testnet", 0, DAY + 2);
    r.close_raise("m120".into());
}

// ---- creating ---------------------------------------------------------

#[test]
#[should_panic(expected = "exactly the whole raise")]
fn a_schedule_that_does_not_release_everything_is_refused() {
    // Under 100% strands money in escrow with no phase that frees it;
    // over it promises the builder money the raise never collected.
    let mut r = raise();
    ctx("sheda.testnet", 0, DAY);
    r.create_project(
        "short".into(),
        acc("builder.testnet"),
        acc("manager.testnet"),
        acc("usdt.testnet"),
        U128(1_000),
        U64(DAY * 2),
        0,
        0,
        vec![("Foundation".into(), 6_000)],
    );
}

#[test]
#[should_panic(expected = "cannot be the manager who signs off")]
fn a_builder_cannot_inspect_their_own_work() {
    let mut r = raise();
    ctx("sheda.testnet", 0, DAY);
    r.create_project(
        "self".into(),
        acc("builder.testnet"),
        acc("builder.testnet"),
        acc("usdt.testnet"),
        U128(1_000),
        U64(DAY * 2),
        0,
        0,
        vec![("All".into(), 10_000)],
    );
}

#[test]
#[should_panic(expected = "platform fee is above")]
fn the_platform_fee_has_a_ceiling_in_the_contract() {
    // Configuration should not be able to leave investors with a raise
    // where most of the money went to the platform.
    let mut r = raise();
    ctx("sheda.testnet", 0, DAY);
    r.create_project(
        "greedy".into(),
        acc("builder.testnet"),
        acc("manager.testnet"),
        acc("usdt.testnet"),
        U128(1_000),
        U64(DAY * 2),
        9_000,
        0,
        vec![("All".into(), 10_000)],
    );
}

#[test]
fn only_the_first_phase_starts_unlocked() {
    let mut r = raise();
    with_project(&mut r);
    let p = r.get_project("m120".into()).unwrap();
    assert_eq!(p.phases[0].state, PhaseState::Building);
    assert_eq!(p.phases[1].state, PhaseState::Locked);
    assert_eq!(p.phases[3].state, PhaseState::Locked);
}

// ---- committing --------------------------------------------------------

#[test]
fn a_commitment_is_recorded_against_the_investor_and_the_unit() {
    let mut r = raise();
    with_project(&mut r);
    commit(&mut r, "ada.testnet", "1a", 250_000, DAY + 1);

    assert_eq!(
        r.get_commitment("m120".into(), acc("ada.testnet"), "1a".into())
            .0,
        250_000
    );
    assert_eq!(
        r.get_total_commitment("m120".into(), acc("ada.testnet")).0,
        250_000
    );
    assert_eq!(r.get_unit_raised("m120".into(), "1a".into()).0, 250_000);
    assert_eq!(r.get_project("m120".into()).unwrap().raised.0, 250_000);
}

#[test]
fn money_from_the_wrong_token_is_sent_straight_back() {
    // Returning the amount is how a NEP-141 receiver refuses. Panicking
    // would also return it but lose the reason.
    let mut r = raise();
    with_project(&mut r);
    ctx("fake-usdt.testnet", 0, DAY + 1);
    let refunded = r.ft_on_transfer(
        acc("ada.testnet"),
        U128(1_000),
        r#"{"project":"m120","unit":"1a"}"#.into(),
    );
    match refunded {
        near_sdk::PromiseOrValue::Value(v) => assert_eq!(v.0, 1_000),
        _ => panic!("expected the whole amount to be refused"),
    }
    assert_eq!(r.get_project("m120".into()).unwrap().raised.0, 0);
}

#[test]
fn an_overshoot_is_taken_only_up_to_the_target() {
    // Accepting more would leave the extra in escrow with no phase that
    // releases it and no failure that refunds it.
    let mut r = raise();
    with_project(&mut r);
    commit(&mut r, "ada.testnet", "1a", 900_000, DAY + 1);

    ctx("usdt.testnet", 0, DAY + 1);
    let change = r.ft_on_transfer(
        acc("bode.testnet"),
        U128(300_000),
        r#"{"project":"m120","unit":"1b"}"#.into(),
    );
    match change {
        near_sdk::PromiseOrValue::Value(v) => assert_eq!(v.0, 200_000),
        _ => panic!("expected the overshoot back"),
    }
    let p = r.get_project("m120".into()).unwrap();
    assert_eq!(p.raised.0, 1_000_000);
    assert_eq!(
        r.get_total_commitment("m120".into(), acc("bode.testnet")).0,
        100_000
    );
}

#[test]
fn money_arriving_after_the_deadline_is_refused() {
    let mut r = raise();
    with_project(&mut r);
    ctx("usdt.testnet", 0, DAY + 40 * DAY);
    let back = r.ft_on_transfer(
        acc("ada.testnet"),
        U128(1_000),
        r#"{"project":"m120","unit":"1a"}"#.into(),
    );
    match back {
        near_sdk::PromiseOrValue::Value(v) => assert_eq!(v.0, 1_000),
        _ => panic!("expected a refusal"),
    }
}

// ---- closing ------------------------------------------------------------

#[test]
fn a_raise_that_reaches_its_target_is_funded() {
    let mut r = raise();
    fund(&mut r);
    assert_eq!(
        r.get_project("m120".into()).unwrap().status,
        RaiseStatus::Funded
    );
}

#[test]
fn a_raise_that_falls_short_by_its_deadline_fails() {
    let mut r = raise();
    with_project(&mut r);
    commit(&mut r, "ada.testnet", "1a", 100_000, DAY + 1);

    ctx("anyone.testnet", 0, DAY + 31 * DAY);
    r.close_raise("m120".into());
    assert_eq!(
        r.get_project("m120".into()).unwrap().status,
        RaiseStatus::Failed
    );
}

#[test]
#[should_panic(expected = "still open and has not reached its target")]
fn a_raise_cannot_be_closed_early_while_it_is_still_short() {
    let mut r = raise();
    with_project(&mut r);
    commit(&mut r, "ada.testnet", "1a", 100_000, DAY + 1);
    ctx("builder.testnet", 0, DAY + 2);
    r.close_raise("m120".into());
}

#[test]
fn the_platform_fee_is_taken_once_and_kept_apart_from_the_builder() {
    // 2% of a million. Recorded separately so it never reads as money
    // the builder has already been paid.
    let mut r = raise();
    fund(&mut r);
    let p = r.get_project("m120".into()).unwrap();
    assert_eq!(p.fee_taken.0, 20_000);
    assert_eq!(p.released.0, 0);
}

#[test]
fn the_fee_is_spread_across_the_schedule_rather_than_falling_on_the_last_phase() {
    // 980,000 to share over four equal phases is 245,000 each. Charging
    // the fee against the target instead would pay the first three
    // 250,000 and leave the last one 20,000 short, which is the phase a
    // builder can least afford to be short on.
    let mut r = raise();
    fund(&mut r);
    let p = r.get_project("m120".into()).unwrap();
    assert_eq!(p.distributable_view.0, 980_000);

    let mut now = DAY + 3;
    let mut paid = Vec::new();
    for phase in 0..4u32 {
        let before = r.get_project("m120".into()).unwrap().released.0;
        ctx("builder.testnet", 1, now);
        r.request_inspection("m120".into(), phase);
        ctx("manager.testnet", 1, now);
        r.attest("m120".into(), phase);
        now += OBJECTION_WINDOW_NS + 1;
        ctx("anyone.testnet", 0, now);
        let _ = r.release_phase("m120".into(), phase);
        paid.push(r.get_project("m120".into()).unwrap().released.0 - before);
    }

    assert_eq!(paid, vec![245_000, 245_000, 245_000, 245_000]);
    assert_eq!(paid.iter().sum::<u128>(), 980_000);
}

// ---- phases --------------------------------------------------------------

fn attest_first_phase(r: &mut Raise, now: u64) {
    ctx("builder.testnet", 1, now);
    r.request_inspection("m120".into(), 0);
    ctx("manager.testnet", 1, now);
    r.attest("m120".into(), 0);
}

#[test]
#[should_panic(expected = "Only the builder may ask for an inspection")]
fn nobody_else_can_submit_a_phase_for_inspection() {
    let mut r = raise();
    fund(&mut r);
    ctx("ada.testnet", 1, DAY + 3);
    r.request_inspection("m120".into(), 0);
}

#[test]
#[should_panic(expected = "Only the property manager")]
fn the_builder_cannot_attest_even_their_own_submitted_phase() {
    let mut r = raise();
    fund(&mut r);
    ctx("builder.testnet", 1, DAY + 3);
    r.request_inspection("m120".into(), 0);
    ctx("builder.testnet", 1, DAY + 3);
    r.attest("m120".into(), 0);
}

#[test]
#[should_panic(expected = "not one you are building right now")]
fn a_later_phase_cannot_be_submitted_before_the_one_in_front_of_it() {
    let mut r = raise();
    fund(&mut r);
    ctx("builder.testnet", 1, DAY + 3);
    r.request_inspection("m120".into(), 2);
}

#[test]
#[should_panic(expected = "Holders still have time to object")]
fn a_phase_cannot_be_settled_while_the_window_is_open() {
    let mut r = raise();
    fund(&mut r);
    attest_first_phase(&mut r, DAY + 3);
    ctx("builder.testnet", 0, DAY + 4);
    let _ = r.release_phase("m120".into(), 0);
}

#[test]
fn a_phase_nobody_objected_to_releases_and_unlocks_the_next() {
    let mut r = raise();
    fund(&mut r);
    attest_first_phase(&mut r, DAY + 3);

    ctx("anyone.testnet", 0, DAY + 3 + OBJECTION_WINDOW_NS + 1);
    let _ = r.release_phase("m120".into(), 0);

    let p = r.get_project("m120".into()).unwrap();
    assert_eq!(p.phases[0].state, PhaseState::Released);
    assert_eq!(
        p.phases[1].state,
        PhaseState::Building,
        "the next phase becomes the one being built"
    );
    // A quarter of the 980,000 left after Sheda's 2%.
    assert_eq!(p.released.0, 245_000);
}

#[test]
fn enough_objections_hold_the_payment() {
    // Ada put in 600,000 of a 1,000,000 raise, which is 60% and well past
    // the 25% that holds a release.
    let mut r = raise();
    fund(&mut r);
    attest_first_phase(&mut r, DAY + 3);

    ctx("ada.testnet", 1, DAY + 4);
    r.object("m120".into(), 0);
    assert_eq!(r.get_objection_bps("m120".into(), 0), 6_000);

    ctx("anyone.testnet", 0, DAY + 3 + OBJECTION_WINDOW_NS + 1);
    let _ = r.release_phase("m120".into(), 0);

    let p = r.get_project("m120".into()).unwrap();
    assert_eq!(p.phases[0].state, PhaseState::Held);
    assert_eq!(p.released.0, 0, "nothing was paid to the builder");
    assert_eq!(p.phases[1].state, PhaseState::Locked);
}

#[test]
fn an_objection_below_the_threshold_does_not_hold_the_payment() {
    // Bode has 400,000 of 1,000,000, which is 40%. Make it someone small
    // instead: a 10% objection should not stop a release.
    let mut r = raise();
    with_project(&mut r);
    commit(&mut r, "ada.testnet", "1a", 900_000, DAY + 1);
    commit(&mut r, "small.testnet", "1b", 100_000, DAY + 1);
    ctx("anyone.testnet", 0, DAY + 2);
    r.close_raise("m120".into());

    attest_first_phase(&mut r, DAY + 3);
    ctx("small.testnet", 1, DAY + 4);
    r.object("m120".into(), 0);
    assert_eq!(r.get_objection_bps("m120".into(), 0), 1_000);
    assert!(r.get_objection_bps("m120".into(), 0) < OBJECTION_THRESHOLD_BPS);

    ctx("anyone.testnet", 0, DAY + 3 + OBJECTION_WINDOW_NS + 1);
    let _ = r.release_phase("m120".into(), 0);
    assert_eq!(
        r.get_project("m120".into()).unwrap().phases[0].state,
        PhaseState::Released
    );
}

#[test]
#[should_panic(expected = "already objected")]
fn an_investor_cannot_object_twice_to_the_same_phase() {
    // Without this one account objects in a loop until it clears the
    // threshold on its own.
    let mut r = raise();
    fund(&mut r);
    attest_first_phase(&mut r, DAY + 3);
    ctx("ada.testnet", 1, DAY + 4);
    r.object("m120".into(), 0);
    ctx("ada.testnet", 1, DAY + 5);
    r.object("m120".into(), 0);
}

#[test]
#[should_panic(expected = "Only someone who committed")]
fn a_stranger_cannot_object() {
    let mut r = raise();
    fund(&mut r);
    attest_first_phase(&mut r, DAY + 3);
    ctx("passerby.testnet", 1, DAY + 4);
    r.object("m120".into(), 0);
}

#[test]
#[should_panic(expected = "window for objecting")]
fn an_objection_after_the_window_closes_is_refused() {
    let mut r = raise();
    fund(&mut r);
    attest_first_phase(&mut r, DAY + 3);
    ctx("ada.testnet", 1, DAY + 3 + OBJECTION_WINDOW_NS + 1);
    r.object("m120".into(), 0);
}

#[test]
fn the_last_phase_releasing_marks_the_raise_delivered() {
    let mut r = raise();
    fund(&mut r);

    let mut now = DAY + 3;
    for phase in 0..4u32 {
        ctx("builder.testnet", 1, now);
        r.request_inspection("m120".into(), phase);
        ctx("manager.testnet", 1, now);
        r.attest("m120".into(), phase);
        now += OBJECTION_WINDOW_NS + 1;
        ctx("anyone.testnet", 0, now);
        let _ = r.release_phase("m120".into(), phase);
    }

    let p = r.get_project("m120".into()).unwrap();
    assert_eq!(p.status, RaiseStatus::Delivered);
    assert!(p.phases.iter().all(|x| x.state == PhaseState::Released));
}

#[test]
fn the_escrow_is_never_paid_out_beyond_what_arrived() {
    // The tranches are a proportion of the target, so if the two ever
    // diverge the cap is what stops the last phase failing to transfer
    // and the money sitting with no state that frees it.
    let mut r = raise();
    fund(&mut r);

    let mut now = DAY + 3;
    for phase in 0..4u32 {
        ctx("builder.testnet", 1, now);
        r.request_inspection("m120".into(), phase);
        ctx("manager.testnet", 1, now);
        r.attest("m120".into(), phase);
        now += OBJECTION_WINDOW_NS + 1;
        ctx("anyone.testnet", 0, now);
        let _ = r.release_phase("m120".into(), phase);
    }

    let p = r.get_project("m120".into()).unwrap();
    assert!(
        p.released.0 + p.fee_taken.0 <= p.raised.0,
        "paid out {} against the {} that arrived",
        p.released.0 + p.fee_taken.0,
        p.raised.0
    );
}

// ---- failure and stalling ------------------------------------------------

#[test]
#[should_panic(expected = "only open on a raise that failed")]
fn there_is_no_refund_from_a_funded_raise() {
    let mut r = raise();
    fund(&mut r);
    ctx("ada.testnet", 1, DAY + 3);
    r.refund("m120".into());
}

#[test]
fn a_failed_raise_lets_an_investor_pull_exactly_what_they_put_in() {
    let mut r = raise();
    with_project(&mut r);
    commit(&mut r, "ada.testnet", "1a", 100_000, DAY + 1);
    ctx("anyone.testnet", 0, DAY + 31 * DAY);
    r.close_raise("m120".into());

    assert_eq!(
        r.get_total_commitment("m120".into(), acc("ada.testnet")).0,
        100_000
    );
    ctx("ada.testnet", 1, DAY + 31 * DAY);
    let _ = r.refund("m120".into());
    // The claim is marked before the transfer so a second call cannot
    // slip in while the first is still in flight.
    assert!(r.has_opted_out_view("m120".into(), acc("ada.testnet")));
}

#[test]
#[should_panic(expected = "Only a funded raise can stall")]
fn a_raise_that_never_funded_cannot_be_declared_stalled() {
    // It fails instead and refunds in full, with nobody's stake touched.
    let mut r = raise();
    with_project(&mut r);
    ctx("sheda.testnet", 1, DAY + 2);
    r.declare_stalled("m120".into());
}

#[test]
#[should_panic(expected = "Only Sheda")]
fn a_builder_cannot_declare_their_own_build_stalled() {
    let mut r = raise();
    fund(&mut r);
    ctx("builder.testnet", 1, DAY + 3);
    r.declare_stalled("m120".into());
}

#[test]
fn opting_out_pays_a_share_of_the_cover_not_of_the_escrow() {
    // Ada committed 600,000 against 18% cover, so 108,000. The number is
    // deliberately not her whole stake: the guarantee is a proportion,
    // and every screen that shows it says so.
    let mut r = raise();
    fund(&mut r);
    ctx("sheda.testnet", 1, DAY + 10 * DAY);
    r.declare_stalled("m120".into());

    assert_eq!(
        r.opt_out_value("m120".into(), acc("ada.testnet")).0,
        108_000
    );
    assert_eq!(
        r.opt_out_value("m120".into(), acc("bode.testnet")).0,
        72_000
    );
}

#[test]
fn cover_is_shared_in_proportion_rather_than_first_come() {
    // The two shares add up to the whole cover, so nobody's recovery
    // depends on how quickly they heard the project had failed.
    let mut r = raise();
    fund(&mut r);
    ctx("sheda.testnet", 1, DAY + 10 * DAY);
    r.declare_stalled("m120".into());

    let ada = r.opt_out_value("m120".into(), acc("ada.testnet")).0;
    let bode = r.opt_out_value("m120".into(), acc("bode.testnet")).0;
    let whole = 1_000_000u128 * 1_800 / 10_000;
    assert_eq!(ada + bode, whole);
}

#[test]
#[should_panic(expected = "declared stalled")]
fn opting_out_of_a_healthy_raise_is_refused() {
    let mut r = raise();
    fund(&mut r);
    ctx("ada.testnet", 1, DAY + 3);
    let _ = r.opt_out("m120".into());
}

#[test]
fn an_investor_who_has_opted_out_is_offered_nothing_further() {
    let mut r = raise();
    fund(&mut r);
    ctx("sheda.testnet", 1, DAY + 10 * DAY);
    r.declare_stalled("m120".into());

    ctx("ada.testnet", 1, DAY + 11 * DAY);
    let _ = r.opt_out("m120".into());
    assert_eq!(r.opt_out_value("m120".into(), acc("ada.testnet")).0, 0);
}

// ---- views ----------------------------------------------------------------

#[test]
fn a_missing_raise_reads_as_absent_rather_than_panicking() {
    let r = raise();
    assert!(r.get_project("nothing".into()).is_none());
    assert_eq!(r.get_objection_bps("nothing".into(), 0), 0);
    assert_eq!(r.opt_out_value("nothing".into(), acc("ada.testnet")).0, 0);
}

#[test]
fn phases_are_reported_in_the_order_they_run() {
    let mut r = raise();
    with_project(&mut r);
    let names: Vec<String> = r
        .get_project("m120".into())
        .unwrap()
        .phases
        .iter()
        .map(|p: &Phase| p.name.clone())
        .collect();
    assert_eq!(names, vec!["Foundation", "Frame", "Roofing", "Handover"]);
}
