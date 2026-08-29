//! Pins the direct-link (P2P) health watchdog's demotion decision — the logic that kicks a
//! dead/one-way direct link back to the server relay so a rejoined peer can be heard again.
//! Thresholds mirror the constants in the P2P manager.

use basis_network_core::p2p::basis_p2p_link_health::{BasisP2PLinkHealth, ConnectedVerdict as V};

const GRACE: i64 = 2500; // ConnectedGracePeriodMs
const STALE: i64 = 1500; // StaleTimeoutMs
const CONFIRM: i64 = 4000; // ConfirmTimeoutMs
const DWELL: i64 = 30000; // HealthyDwellResetMs
const PUNCH_TIMEOUT: i64 = 6000;

fn eval(conn_ms: i64, inbound_ms: i64, confirmed: bool) -> V {
    eval_flap(conn_ms, inbound_ms, confirmed, false)
}

fn eval_flap(conn_ms: i64, inbound_ms: i64, confirmed: bool, flap: bool) -> V {
    BasisP2PLinkHealth::evaluate_connected(conn_ms, inbound_ms, confirmed, flap, GRACE, STALE, CONFIRM, DWELL)
}

// --- The core recover-after-rejoin case ---

#[test]
fn no_inbound_past_stale_demotes_to_server_relay() {
    // Peer rejoined -> its old direct socket is silent -> no inbound P2P. Past the grace window
    // and the stale timeout, the link must be demoted so voice falls back to the server.
    assert_eq!(eval(5000, STALE + 100, true), V::DemoteStale);
}

#[test]
fn fresh_inbound_stays_healthy() {
    assert_eq!(eval(10000, 100, true), V::Healthy);
}

#[test]
fn just_under_stale_stays_healthy() {
    assert_eq!(eval(5000, STALE - 100, true), V::Healthy);
}

// --- Settle window ---

#[test]
fn within_grace_never_demotes_even_with_no_inbound_and_unconfirmed() {
    // The grace short-circuit must win over both the stale and never-confirmed checks.
    assert_eq!(eval(GRACE - 1, 99999, false), V::Healthy);
}

#[test]
fn grace_boundary_is_exclusive() {
    // age < grace is the settle window; age == grace evaluates the liveness checks.
    assert_eq!(eval(GRACE - 1, 99999, true), V::Healthy);
    assert_eq!(eval(GRACE, 99999, true), V::DemoteStale);
}

// --- Never-confirmed offload ---

#[test]
fn connected_but_server_never_confirmed_past_confirm_timeout_demotes() {
    assert_eq!(eval(CONFIRM + 100, 100, false), V::DemoteUnconfirmed);
}

#[test]
fn unconfirmed_but_under_confirm_timeout_stays_healthy() {
    // Past grace, receiving inbound, but the server confirm hasn't had time to arrive yet.
    assert_eq!(eval(CONFIRM - 100, 100, false), V::Healthy);
}

#[test]
fn stale_takes_priority_over_unconfirmed() {
    // Both conditions true -> report the more specific "stale" verdict.
    assert_eq!(eval(CONFIRM + 1000, STALE + 500, false), V::DemoteStale);
}

// --- Flap-counter reset ---

#[test]
fn healthy_long_dwell_with_pending_flap_clears_flap_counter() {
    assert_eq!(eval_flap(DWELL + 1000, 100, true, true), V::ClearFlapCounter);
}

#[test]
fn healthy_long_dwell_no_flap_stays_healthy() {
    assert_eq!(eval_flap(DWELL + 1000, 100, true, false), V::Healthy);
}

#[test]
fn healthy_short_dwell_with_flap_does_not_clear_yet() {
    assert_eq!(eval_flap(DWELL - 1000, 100, true, true), V::Healthy);
}

// --- Punch timeout ---

#[test]
fn punch_stalled_at_boundary() {
    for (age_ms, expected) in [(PUNCH_TIMEOUT - 1, false), (PUNCH_TIMEOUT, false), (PUNCH_TIMEOUT + 1, true)] {
        assert_eq!(BasisP2PLinkHealth::punch_stalled(age_ms, PUNCH_TIMEOUT), expected, "age {age_ms}");
    }
}
