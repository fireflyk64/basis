//! BasisCpuBudget: the sum of all grants stays inside the machine, leases come and go cleanly,
//! and the discovery seams behave.

use basis_network_core::protocol::BasisCpuBudget;
use serial_test::serial;

fn sum_of_grants() -> i32 {
    BasisCpuBudget::leases().iter().map(|l| l.granted()).sum()
}

#[test]
#[serial]
fn concurrency_width_is_a_bounded_power_of_two() {
    let width = BasisCpuBudget::concurrency_width(2, 16, 1024);
    assert!(u32::try_from(width).is_ok_and(|w| w.is_power_of_two()));
    assert!((16..=1024).contains(&width));
    assert_eq!(BasisCpuBudget::concurrency_width(0, 4, 4), 4);
    assert_eq!(BasisCpuBudget::concurrency_width(1_000_000, 1, 8), 8);
    assert!(BasisCpuBudget::concurrency_width(1, 3, 1024) >= 4);
}

#[test]
#[serial]
fn standing_leases_exist_and_grants_fit_the_machine() {
    let leases = BasisCpuBudget::leases();
    assert!(leases.iter().any(|l| l.name() == "reduction-send"));
    assert!(leases.iter().any(|l| l.name() == "peer-update"));
    for lease in &leases {
        assert!(lease.granted() >= 1, "{lease:?}");
        assert!(lease.granted() <= lease.effective_max().max(lease.min_cores()), "{lease:?}");
    }
    let cores = BasisCpuBudget::total_cores();
    // Every lease gets at least one core even on a tiny host; otherwise the pool is the machine.
    assert!(sum_of_grants() <= cores.max(i32::try_from(leases.len()).unwrap()));
    assert!(BasisCpuBudget::reduction_send_cap() >= 1);
    assert!(BasisCpuBudget::peer_update_cap() >= 1);
    assert!(BasisCpuBudget::describe().contains("cores"));
    assert!(BasisCpuBudget::describe_live().contains("reduction-send"));
}

#[test]
#[serial]
fn register_and_unregister_keep_the_invariant() {
    let before = BasisCpuBudget::leases().len();
    let lease = BasisCpuBudget::register("test-pool", 1, Box::new(|| 2), 1.0);
    assert_eq!(BasisCpuBudget::leases().len(), before + 1);
    assert!(lease.granted() >= 1 && lease.granted() <= 2);
    for _ in 0..20 {
        BasisCpuBudget::rebalance();
        let cores = BasisCpuBudget::total_cores();
        let count = i32::try_from(BasisCpuBudget::leases().len()).unwrap();
        assert!(sum_of_grants() <= cores.max(count), "grants {} exceed {cores} cores", sum_of_grants());
    }
    BasisCpuBudget::unregister(&lease);
    assert_eq!(BasisCpuBudget::leases().len(), before);
    // Unregistering twice is harmless.
    BasisCpuBudget::unregister(&lease);
    assert_eq!(BasisCpuBudget::leases().len(), before);
}

#[test]
#[serial]
fn a_lease_never_exceeds_its_declared_ceiling_or_drops_below_its_floor() {
    let lease = BasisCpuBudget::register("capped", 3, Box::new(|| 1), 5.0);
    // min above max: the floor wins, and the ceiling is floored too.
    assert_eq!(lease.min_cores(), 3);
    assert_eq!(lease.declared_max(), 3);
    assert_eq!(lease.effective_max(), 3);
    lease.report_demand(1.0);
    for _ in 0..50 {
        BasisCpuBudget::rebalance();
    }
    assert!(lease.granted() >= 1 && lease.granted() <= 3);
    BasisCpuBudget::unregister(&lease);
}

#[test]
#[serial]
fn demand_is_clamped_and_pressure_reaches_the_standing_leases() {
    let lease = BasisCpuBudget::register("clamp", 1, Box::new(|| 4), 1.0);
    lease.report_demand(7.0);
    assert_eq!(lease.demand(), 1.0);
    lease.report_demand(-3.0);
    assert_eq!(lease.demand(), 0.0);
    lease.report_demand(f64::NAN);
    assert_eq!(lease.demand(), 0.0);
    BasisCpuBudget::unregister(&lease);

    BasisCpuBudget::report_pressure(0.25, 0.75);
    assert_eq!(BasisCpuBudget::reduction_send_lease().demand(), 0.25);
    assert_eq!(BasisCpuBudget::peer_update_lease().demand(), 0.75);
    BasisCpuBudget::report_pressure(0.0, 0.0);
}

#[test]
#[serial]
fn work_reports_are_ignored_unless_both_halves_are_positive() {
    let lease = BasisCpuBudget::register("work", 1, Box::new(|| 4), 1.0);
    assert!(!lease.reports_work());
    lease.add_work(0, 5.0);
    lease.add_work(5, 0.0);
    lease.add_work(-1, 1.0);
    lease.add_work(5, -1.0);
    lease.add_work(5, f64::NAN);
    assert!(!lease.reports_work());
    assert_eq!(lease.work_total(), 0);
    lease.add_work(10, 2.5);
    assert!(lease.reports_work());
    assert_eq!(lease.work_total(), 10);
    assert_eq!(lease.busy_micros_total(), 2500);
    BasisCpuBudget::unregister(&lease);
}

#[test]
#[serial]
fn send_socket_count_changes_invalidate_the_send_pool_ceiling() {
    let original = BasisCpuBudget::send_socket_count();
    BasisCpuBudget::set_send_socket_count(0);
    assert_eq!(BasisCpuBudget::send_socket_count(), 1);
    BasisCpuBudget::set_send_socket_count(3);
    assert_eq!(BasisCpuBudget::send_socket_count(), 3);
    assert!(!BasisCpuBudget::reduction_send_lease().has_measured_ceiling());
    assert_eq!(
        BasisCpuBudget::max_reduction_send_workers(),
        BasisCpuBudget::total_cores().min(8 * 3)
    );
    BasisCpuBudget::set_send_socket_count(original);
    let auto = BasisCpuBudget::auto_max_send_sockets();
    assert!(auto >= 1 && auto <= BasisCpuBudget::total_cores());
}

#[test]
#[serial]
fn probe_window_seam_and_utilization_sampling() {
    BasisCpuBudget::set_probe_window_ms(50.0);
    assert_eq!(BasisCpuBudget::probe_window_ms(), 50.0);
    BasisCpuBudget::set_probe_window_ms(f64::NAN);
    assert_eq!(BasisCpuBudget::probe_window_ms(), 2000.0);
    BasisCpuBudget::set_probe_window_ms(-1.0);
    assert_eq!(BasisCpuBudget::probe_window_ms(), 2000.0);

    let first = BasisCpuBudget::sample_utilization();
    assert!((0.0..=1.0).contains(&first));
    let mut spin = 0u64;
    for i in 0..2_000_000u64 {
        spin = spin.wrapping_mul(31).wrapping_add(i);
    }
    assert!(spin != 1);
    let second = BasisCpuBudget::sample_utilization();
    assert!((0.0..=1.0).contains(&second));
    assert!((0.0..=1.0).contains(&BasisCpuBudget::utilization()));
}

#[test]
#[serial]
fn invalidate_discovery_resets_the_measured_ceiling() {
    let lease = BasisCpuBudget::register("discover", 1, Box::new(|| 8), 1.0);
    assert!(!lease.has_measured_ceiling());
    assert_eq!(lease.discovered_max(), i32::MAX);
    assert_eq!(lease.forced_grant(), 0);
    assert_eq!(lease.probe_phase(), 0);
    lease.invalidate_discovery();
    assert!(!lease.has_measured_ceiling());
    assert_eq!(lease.probe_cooldown_steps(), 0);
    BasisCpuBudget::unregister(&lease);
}
