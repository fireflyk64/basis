//! Covers the one property the send pool's width controller cannot hold on its own: that adding a
//! worker has to make the pool faster to keep it. Everything here is the verdict that stops a pool
//! climbing to its ceiling while throughput falls, and the conditions under which the verdict is
//! allowed to expire.

use basis_network_server::reduction::BasisServerReductionSystemEvents as R;
use serial_test::serial;

const STEADY_PLAYERS: i32 = 1000;

struct Reset;

impl Drop for Reset {
    fn drop(&mut self) {
        R::test_only_reset_pool_tuning(4);
    }
}

#[test]
#[serial(reduction_statics)]
fn widening_that_loses_throughput_is_given_back() {
    let _r = Reset;
    R::test_only_reset_pool_tuning(8);
    let width = R::test_only_resolve_widen_trial(8, 16, 1000.0, 900.0, STEADY_PLAYERS);
    assert_eq!(width, 8);
    assert_eq!(R::test_only_send_workers(), 8);
    assert_eq!(R::test_only_learned_width_ceiling(), 8);
}

#[test]
#[serial(reduction_statics)]
fn widening_that_pays_is_kept() {
    let _r = Reset;
    R::test_only_reset_pool_tuning(8);
    let width = R::test_only_resolve_widen_trial(8, 16, 1000.0, 1400.0, STEADY_PLAYERS);
    assert_eq!(width, 16);
    assert_eq!(R::test_only_send_workers(), 16);
    assert_eq!(R::test_only_learned_width_ceiling(), 0);
}

/// Flat is a loss, not a draw: the extra worker is a core spent for nothing.
#[test]
#[serial(reduction_statics)]
fn widening_that_changes_nothing_is_given_back() {
    let _r = Reset;
    R::test_only_reset_pool_tuning(8);
    let width = R::test_only_resolve_widen_trial(8, 16, 1000.0, 1010.0, STEADY_PLAYERS);
    assert_eq!(width, 8);
    assert_eq!(R::test_only_learned_width_ceiling(), 8);
}

/// Nothing timed at the old width is not evidence against the new one.
#[test]
#[serial(reduction_statics)]
fn widening_with_nothing_to_compare_against_stands() {
    let _r = Reset;
    R::test_only_reset_pool_tuning(8);
    let width = R::test_only_resolve_widen_trial(8, 16, 0.0, 1200.0, STEADY_PLAYERS);
    assert_eq!(width, 16);
    assert_eq!(R::test_only_learned_width_ceiling(), 0);
}

#[test]
#[serial(reduction_statics)]
fn learned_ceiling_holds_while_the_population_does() {
    let _r = Reset;
    R::test_only_reset_pool_tuning(8);
    R::test_only_resolve_widen_trial(8, 16, 1000.0, 900.0, STEADY_PLAYERS);
    R::test_only_expire_learned_ceiling(STEADY_PLAYERS + 50);
    assert_eq!(R::test_only_learned_width_ceiling(), 8);
}

/// The verdict was about one load level. A population a quarter larger is a different question.
#[test]
#[serial(reduction_statics)]
fn learned_ceiling_expires_when_the_population_moves() {
    for players in [1400, 600] {
        let _r = Reset;
        R::test_only_reset_pool_tuning(8);
        R::test_only_resolve_widen_trial(8, 16, 1000.0, 900.0, STEADY_PLAYERS);
        R::test_only_expire_learned_ceiling(players);
        assert_eq!(R::test_only_learned_width_ceiling(), 0, "players {players}");
    }
}

#[test]
#[serial(reduction_statics)]
fn learned_ceiling_expires_once_it_is_old_enough() {
    let _r = Reset;
    R::test_only_reset_pool_tuning(8);
    R::test_only_resolve_widen_trial(8, 16, 1000.0, 900.0, STEADY_PLAYERS);
    R::test_only_age_learned_ceiling();
    R::test_only_expire_learned_ceiling(STEADY_PLAYERS);
    assert_eq!(R::test_only_learned_width_ceiling(), 0);
}
