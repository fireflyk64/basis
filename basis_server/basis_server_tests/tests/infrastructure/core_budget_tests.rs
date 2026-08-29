//! Covers the two properties the core allocator exists to hold: the sum of what it hands out
//! stays inside the machine, and a lease's ceiling is measured rather than believed. The discovery
//! tests run against the real clock because the probe windows are real durations.
//!
//! `PeerUpdateSizingSurvivesAGrantBelowItsFloor` exercised LiteNetLib's per-peer update pool,
//! which the iroh transport does not have; the send-pool half of that rule is ported.

use std::sync::Arc;
use std::time::{Duration, Instant};

use basis_network_core::protocol::basis_cpu_budget::{BasisCoreLease, BasisCpuBudget};
use basis_network_server::reduction::BasisServerReductionSystemEvents;
use serial_test::serial;

/// Shortened probe window: the controller cares about the ratio between work and busy time.
const WINDOW_MS: f64 = 120.0;

struct ShortWindow(f64);

impl ShortWindow {
    fn new() -> Self {
        let previous = BasisCpuBudget::probe_window_ms();
        BasisCpuBudget::set_probe_window_ms(WINDOW_MS);
        Self(previous)
    }
}

impl Drop for ShortWindow {
    fn drop(&mut self) {
        BasisCpuBudget::set_probe_window_ms(self.0);
    }
}

struct Registered(Vec<Arc<BasisCoreLease>>);

impl Drop for Registered {
    fn drop(&mut self) {
        for lease in &self.0 {
            BasisCpuBudget::unregister(lease);
        }
    }
}

/// Drives the allocator for a stretch of wall time, feeding a lease work at a rate that depends on
/// how many cores it currently holds.
fn run(lease: &BasisCoreLease, ms: f64, rate_per_busy_ms: impl Fn(i32) -> f64) {
    let end = Instant::now() + Duration::from_secs_f64(ms / 1000.0);
    while Instant::now() < end {
        // One notional pass: 1 ms of busy time, delivering whatever this width is worth.
        lease.report_demand(1.0);
        lease.add_work(rate_per_busy_ms(lease.granted()) as i64, 1.0);
        BasisCpuBudget::rebalance();
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
#[serial(cpu_budget)]
fn grants_never_exceed_the_machine() {
    let leases = Registered(vec![
        BasisCpuBudget::register("test-a", 1, Box::new(|| 4096), 1.0),
        BasisCpuBudget::register("test-b", 1, Box::new(|| 4096), 1.0),
        BasisCpuBudget::register("test-c", 1, Box::new(|| 4096), 5.0),
    ]);
    for l in &leases.0 {
        l.report_demand(1.0);
    }
    for _ in 0..200 {
        BasisCpuBudget::rebalance();
    }
    let all = BasisCpuBudget::leases();
    let total: i32 = all.iter().map(|l| l.granted()).sum();
    // Every lease declared an effectively unbounded ceiling and maximum demand. Without the pool
    // invariant each would take the whole box and the sum would be a multiple of it. A box with
    // fewer cores than the leases' floors add up to cannot honour both; there the floors win, and
    // that is the only excess allowed.
    let floors: i32 = all.iter().map(|l| l.min_cores()).sum();
    let ceiling = BasisCpuBudget::total_cores().max(floors);
    if floors > BasisCpuBudget::total_cores() {
        println!("note: {floors} cores of floors on a {}-core box; checking against the floors", BasisCpuBudget::total_cores());
    }
    assert!(total <= ceiling, "handed out {total} of {} cores (floors {floors})", BasisCpuBudget::total_cores());
    for l in &leases.0 {
        assert!(l.granted() >= 1);
    }
}

#[test]
#[serial(cpu_budget)]
fn lease_that_reports_no_work_keeps_its_declared_ceiling() {
    let lease = Registered(vec![BasisCpuBudget::register("test-silent", 1, Box::new(|| 3), 1.0)]);
    lease.0[0].report_demand(1.0);
    for _ in 0..500 {
        BasisCpuBudget::rebalance();
    }
    // Nothing to measure against, so the declared number stands.
    assert!(!lease.0[0].has_measured_ceiling());
    assert_eq!(lease.0[0].effective_max(), 3);
}

#[test]
#[serial(cpu_budget)]
fn ceiling_converges_on_the_real_one() {
    if BasisCpuBudget::total_cores() < 8 {
        println!("skipped: needs room to narrow ({} cores)", BasisCpuBudget::total_cores());
        return;
    }
    // Declares a ceiling of 64 but genuinely saturates at 4: past that, extra workers deliver nothing.
    const REAL_CEILING: i32 = 4;
    let _w = ShortWindow::new();
    let lease = Registered(vec![BasisCpuBudget::register("test-capped", 1, Box::new(|| 64), 8.0)]);
    run(&lease.0[0], WINDOW_MS * 80.0, |granted| granted.min(REAL_CEILING) as f64 * 100.0);
    assert!(lease.0[0].has_measured_ceiling(), "no ceiling was measured");
    let measured = lease.0[0].effective_max();
    assert!((REAL_CEILING..=REAL_CEILING + 2).contains(&measured), "measured {measured}");
}

#[test]
#[serial(cpu_budget)]
fn ceiling_stops_narrowing_when_throughput_falls() {
    if BasisCpuBudget::total_cores() < 8 {
        println!("skipped: needs room to narrow ({} cores)", BasisCpuBudget::total_cores());
        return;
    }
    // Scales perfectly with width: there is no ceiling below the machine and narrowing must be
    // rejected at the first step.
    let _w = ShortWindow::new();
    let lease = Registered(vec![BasisCpuBudget::register("test-scaling", 2, Box::new(|| 64), 8.0)]);
    let width_before = lease.0[0].granted();
    run(&lease.0[0], WINDOW_MS * 80.0, |granted| granted as f64 * 100.0);
    assert!(lease.0[0].effective_max() >= width_before, "narrowed to {} from {width_before} despite throughput scaling with width", lease.0[0].effective_max());
}

#[test]
#[serial(cpu_budget)]
fn idle_lease_is_not_measured() {
    let _w = ShortWindow::new();
    let lease = Registered(vec![BasisCpuBudget::register("test-idle", 1, Box::new(|| 64), 1.0)]);
    // Reports work but is not under pressure: narrowing it would look free and would latch a
    // ceiling describing the load rather than the machine.
    let end = Instant::now() + Duration::from_secs_f64(WINDOW_MS * 2.0 / 1000.0);
    while Instant::now() < end {
        lease.0[0].report_demand(0.1);
        lease.0[0].add_work(100, 1.0);
        BasisCpuBudget::rebalance();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!lease.0[0].has_measured_ceiling());
}

#[test]
#[serial(cpu_budget)]
fn rising_demand_reopens_a_measured_ceiling() {
    if BasisCpuBudget::total_cores() < 8 {
        println!("skipped: needs room to narrow ({} cores)", BasisCpuBudget::total_cores());
        return;
    }
    let _w = ShortWindow::new();
    let lease = Registered(vec![BasisCpuBudget::register("test-surge", 1, Box::new(|| 64), 8.0)]);
    // Settles on a low ceiling under light load.
    let end = Instant::now() + Duration::from_secs_f64(WINDOW_MS * 60.0 / 1000.0);
    while Instant::now() < end {
        lease.0[0].report_demand(0.55);
        lease.0[0].add_work(lease.0[0].granted().min(3) as i64 * 100, 1.0);
        BasisCpuBudget::rebalance();
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(lease.0[0].has_measured_ceiling(), "no ceiling was measured under light load");
    let narrow = lease.0[0].effective_max();

    // Load steps up: a ceiling measured on a half-empty server must not cap a full one.
    lease.0[0].report_demand(1.0);
    BasisCpuBudget::rebalance();
    assert!(lease.0[0].effective_max() > narrow, "ceiling stayed at {narrow} after demand rose");
}

#[test]
#[serial(cpu_budget)]
fn adding_a_send_socket_reopens_the_send_ceiling() {
    let lease = BasisCpuBudget::reduction_send_lease();
    let before = BasisCpuBudget::send_socket_count();
    BasisCpuBudget::set_send_socket_count(before + 1);
    // A new socket means anything measured earlier described a machine that no longer exists.
    assert!(!lease.has_measured_ceiling());
    BasisCpuBudget::set_send_socket_count(before);
}

/// The send pool sizes itself between a floor and the allocator's current grant. On a host too
/// small to satisfy every lease's floor the grant can land below the number the pool would like to
/// start from; when it does, the floor has to yield to the grant.
#[test]
#[serial(cpu_budget)]
fn send_pool_sizing_survives_a_grant_below_its_floor() {
    let count = 8.max(BasisCpuBudget::total_cores() * 2) as usize;
    let squeeze = Registered((0..count).map(|i| BasisCpuBudget::register(&format!("test-squeeze-{i}"), 1, Box::new(|| 4096), 8.0)).collect());
    for l in &squeeze.0 {
        l.report_demand(1.0);
    }

    let mut saw_grant_under_floor = false;
    for _ in 0..200 {
        BasisCpuBudget::rebalance();
        if BasisCpuBudget::reduction_send_cap() < BasisCpuBudget::min_workers_per_pool() {
            saw_grant_under_floor = true;
        }
        for players in [0, 1, 200, 4000] {
            // current is swept past both ends of the legal range: the sizing clamps it before
            // stepping from it.
            for current in [0, 1, 4, BasisCpuBudget::total_cores() * 2] {
                let degree = BasisServerReductionSystemEvents::test_only_degree_for(players, current);
                assert!(degree >= 1, "degree {degree} is not a legal parallelism");
                assert!(degree <= BasisCpuBudget::total_cores(), "degree {degree} exceeds {} cores", BasisCpuBudget::total_cores());
            }
        }
    }
    assert!(saw_grant_under_floor, "the squeeze never drove the grant under the floor of {}, so this never exercised the case it exists for", BasisCpuBudget::min_workers_per_pool());
    drop(squeeze);
    for _ in 0..200 {
        BasisCpuBudget::rebalance();
    }
}
