//! Pins the population/memory scaling that replaced the fixed ceilings, and the migration that
//! retires the old values from files already on disc. These check the properties that matter: a
//! configured value always wins, the result stays inside its declared bounds, a bigger box gets a
//! bigger allowance, and a bigger crowd gets a smaller per-peer share.

use basis_network_core::configuration::{BasisPopulationScale, LNLTransportConfig};
use serial_test::serial;

const GB: i64 = 1024 * 1024 * 1024;

/// The detected memory figure is process-global; every test puts it back.
struct MemoryGuard;

impl Drop for MemoryGuard {
    fn drop(&mut self) {
        BasisPopulationScale::override_available_memory_for_tests(0);
    }
}

#[test]
#[serial(population_scale)]
fn configured_value_always_wins() {
    let _g = MemoryGuard;
    BasisPopulationScale::override_available_memory_for_tests(64 * GB);
    for configured in [1, 512, 99999] {
        // An operator who pinned a number gets that number, even an absurd one.
        assert_eq!(BasisPopulationScale::unreliable_queue_per_peer(configured, 2000), configured);
        assert_eq!(BasisPopulationScale::slice_cap(configured, 2000), configured);
    }
}

#[test]
#[serial(population_scale)]
fn unreliable_queue_stays_within_declared_bounds() {
    let _g = MemoryGuard;
    BasisPopulationScale::override_available_memory_for_tests(64 * GB);
    for peers in [1, 50, 500, 2000, 4000, 8000, 65535] {
        let depth = BasisPopulationScale::unreliable_queue_per_peer(0, peers);
        assert!((BasisPopulationScale::MIN_UNRELIABLE_QUEUE_PER_PEER..=BasisPopulationScale::MAX_UNRELIABLE_QUEUE_PER_PEER).contains(&depth), "peers {peers}: {depth}");
    }
}

#[test]
#[serial(population_scale)]
fn unreliable_queue_shrinks_as_the_crowd_grows() {
    let _g = MemoryGuard;
    BasisPopulationScale::override_available_memory_for_tests(64 * GB);
    let at2000 = BasisPopulationScale::unreliable_queue_per_peer(0, 2000);
    let at8000 = BasisPopulationScale::unreliable_queue_per_peer(0, 8000);
    assert!(at8000 < at2000, "expected 8000 peers to get less than 2000; got {at8000} vs {at2000}");
}

#[test]
#[serial(population_scale)]
fn unreliable_queue_bigger_box_gets_more_headroom() {
    let _g = MemoryGuard;
    BasisPopulationScale::override_available_memory_for_tests(8 * GB);
    let small = BasisPopulationScale::unreliable_queue_per_peer(0, 2000);
    BasisPopulationScale::override_available_memory_for_tests(64 * GB);
    let large = BasisPopulationScale::unreliable_queue_per_peer(0, 2000);
    assert!(large > small, "expected a 64 GB box to allow more than an 8 GB box; got {large} vs {small}");
}

/// The pool has to be able to take back what the queues can let go of.
#[test]
#[serial(population_scale)]
fn packet_pool_can_absorb_everything_the_queues_hold() {
    let _g = MemoryGuard;
    BasisPopulationScale::override_available_memory_for_tests(64 * GB);
    const PEERS: i32 = 1000;
    let queue_capacity = PEERS as i64 * (BasisPopulationScale::unreliable_queue_per_peer(0, PEERS) as i64 + BasisPopulationScale::priority_queue_per_peer(0, PEERS) as i64);
    let pool = BasisPopulationScale::packet_pool_max(0, PEERS, 48);
    assert!(pool as i64 >= queue_capacity, "pool cap {pool} cannot absorb {queue_capacity} packets of queue capacity");
}

#[test]
#[serial(population_scale)]
fn unreliable_queue_measured_working_point_is_reachable() {
    let _g = MemoryGuard;
    BasisPopulationScale::override_available_memory_for_tests(64 * GB);
    let depth = BasisPopulationScale::unreliable_queue_per_peer(0, 2000);
    assert!((2048..=BasisPopulationScale::MAX_UNRELIABLE_QUEUE_PER_PEER).contains(&depth), "{depth}");
}

#[test]
#[serial(population_scale)]
fn unreliable_queue_tiny_box_still_gets_a_usable_floor() {
    let _g = MemoryGuard;
    BasisPopulationScale::override_available_memory_for_tests(GB);
    assert_eq!(BasisPopulationScale::unreliable_queue_per_peer(0, 8000), BasisPopulationScale::MIN_UNRELIABLE_QUEUE_PER_PEER);
}

#[test]
#[serial(population_scale)]
fn packet_pool_max_covers_per_peer_demand_at_eight_thousand() {
    let _g = MemoryGuard;
    BasisPopulationScale::override_available_memory_for_tests(64 * GB);
    let cap = BasisPopulationScale::packet_pool_max(0, 8000, 48);
    assert!(cap >= 8000 * 48, "pool cap {cap} is below the per-peer demand of {}", 8000 * 48);
}

#[test]
#[serial(population_scale)]
fn slice_cap_rises_with_population_and_stays_bounded() {
    assert_eq!(BasisPopulationScale::slice_cap(0, 2000), 32); // unchanged at the old design point
    assert!(BasisPopulationScale::slice_cap(0, 8000) > 32); // more room to degrade at 8k
    assert!((32..=256).contains(&BasisPopulationScale::slice_cap(0, 1_000_000)));
}

#[test]
#[serial(population_scale)]
fn available_memory_is_detected_not_assumed() {
    let _g = MemoryGuard;
    BasisPopulationScale::override_available_memory_for_tests(0);
    let detected = BasisPopulationScale::available_memory_bytes();
    assert!(detected > 0);
    assert_ne!(detected, 4 * GB);
}

#[test]
fn migration_retires_the_old_defaults_but_keeps_deliberate_values() {
    let mut legacy = LNLTransportConfig { max_unreliable_queue_per_peer: 256, packet_pool_size_max: 262144, ..Default::default() };
    legacy.migrate_from(7);
    assert_eq!(legacy.max_unreliable_queue_per_peer, 0);
    assert_eq!(legacy.packet_pool_size_max, 0);

    // Someone who pinned a value meant it. Only the exact shipped defaults are retired.
    let mut deliberate = LNLTransportConfig { max_unreliable_queue_per_peer: 1024, packet_pool_size_max: 100000, ..Default::default() };
    deliberate.migrate_from(7);
    assert_eq!(deliberate.max_unreliable_queue_per_peer, 1024);
    assert_eq!(deliberate.packet_pool_size_max, 100000);
}

#[test]
fn migration_does_not_re_run_on_current_files() {
    let mut current = LNLTransportConfig { max_unreliable_queue_per_peer: 256, ..Default::default() };
    current.migrate_from(LNLTransportConfig::CURRENT_CONFIG_VERSION);
    assert_eq!(current.max_unreliable_queue_per_peer, 256);
}
