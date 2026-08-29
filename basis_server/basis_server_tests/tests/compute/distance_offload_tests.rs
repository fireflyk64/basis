//! The compute offload, exercised through the same runtime lookup the server uses. Every test is
//! skippable rather than failing when there is no device, because most machines that run this
//! suite have none and that is the supported configuration.

use basis_network_core::BasisNetworkCommons;
use basis_network_core::compute::{BasisDistanceSolveParameters, BasisDistanceSolveRequest};
use basis_network_server::reduction::basis_server_reduction_system_events::DistanceSweepState;
use basis_network_server::reduction::{BasisComputeBackend, BasisServerReductionSystemEvents};
use basis_server_tests::support::delta_test_support::TestRng;
use serial_test::serial;

const BASE_INTERVAL_MS: i32 = 50;
const HIGH_DISTANCE_SQ: f32 = 100.0;
const MEDIUM_DISTANCE_SQ: f32 = 900.0;
const LOW_DISTANCE_SQ: f32 = 2500.0;
const BASE_MULTIPLIER: f32 = 1.0;
const INCREASE_RATE: f32 = 0.01;

fn parameters() -> BasisDistanceSolveParameters {
    BasisDistanceSolveParameters { high_distance_sq: HIGH_DISTANCE_SQ, medium_distance_sq: MEDIUM_DISTANCE_SQ, low_distance_sq: LOW_DISTANCE_SQ, base_multiplier: BASE_MULTIPLIER, increase_rate: INCREASE_RATE, base_interval_ms: BASE_INTERVAL_MS }
}

/// A crowd spread across the tier boundaries rather than uniformly.
fn build_roster(players: usize) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let mut rng = TestRng::new(9081);
    let mut x = Vec::with_capacity(players);
    let mut y = Vec::with_capacity(players);
    let mut z = Vec::with_capacity(players);
    for _ in 0..players {
        x.push((rng.next_f64() * 120.0 - 60.0) as f32);
        y.push((rng.next_f64() * 4.0) as f32);
        z.push((rng.next_f64() * 120.0 - 60.0) as f32);
    }
    (x, y, z)
}

#[test]
fn backend_loads_or_explains_why_not() {
    let solver = BasisComputeBackend::try_load_distance_solver(BASE_INTERVAL_MS, "");
    let status = BasisComputeBackend::status();
    println!("status: {status}");
    assert!(!status.trim().is_empty());
    if let Some(solver) = solver {
        assert!(!solver.backend().trim().is_empty());
        assert!(!solver.device_name().trim().is_empty());
    }
}

/// The tier a pair is served at must be identical on both backends.
#[test]
fn quality_tiers_match_the_cpu_exactly() {
    let Some(solver) = BasisComputeBackend::try_load_distance_solver(BASE_INTERVAL_MS, "") else {
        println!("skipped: {}", BasisComputeBackend::status());
        return;
    };

    const PLAYERS: usize = 512;
    let (x, y, z) = build_roster(PLAYERS);
    let request = BasisDistanceSolveRequest { pos_x: x.clone(), pos_y: y.clone(), pos_z: z.clone(), player_count: PLAYERS, slice_start: 0, slice_end: PLAYERS, parameters: parameters() };
    let mut interval = vec![0u8; PLAYERS * PLAYERS];
    let mut quality = vec![0u8; PLAYERS * PLAYERS];
    solver.solve(&request, &mut interval, &mut quality);

    let (mut tier_mismatches, mut interval_beyond_one_step, mut interval_differing) = (0i64, 0i64, 0i64);
    for i in 0..PLAYERS {
        for j in 0..PLAYERS {
            if i == j {
                continue;
            }
            let (dx, dy, dz) = (x[i] - x[j], y[i] - y[j], z[i] - z[j]);
            let dist_sq = dx * dx + dy * dy + dz * dz;
            let expected_quality = if dist_sq <= HIGH_DISTANCE_SQ {
                3
            } else if dist_sq <= MEDIUM_DISTANCE_SQ {
                2
            } else if dist_sq <= LOW_DISTANCE_SQ {
                1
            } else {
                0
            };
            let raw = (BASE_INTERVAL_MS as f32 * (BASE_MULTIPLIER + dist_sq * INCREASE_RATE)) as i32;
            let expected_interval = BasisNetworkCommons::encode_avatar_interval_byte(raw, BASE_INTERVAL_MS);

            let o = i * PLAYERS + j;
            if quality[o] != expected_quality {
                tier_mismatches += 1;
            }
            let difference = interval[o] as i32 - expected_interval as i32;
            if difference != 0 {
                interval_differing += 1;
            }
            if !(-1..=1).contains(&difference) {
                interval_beyond_one_step += 1;
            }
        }
    }
    let pairs = PLAYERS * PLAYERS - PLAYERS;
    println!("{} ({}) over {pairs} pairs: tier mismatches {tier_mismatches}, interval differing {interval_differing}, interval beyond one step {interval_beyond_one_step}", solver.backend(), solver.device_name());
    assert_eq!(tier_mismatches, 0);
    assert_eq!(interval_beyond_one_step, 0);
}

/// A slice must produce the same answers as the full sweep for the receivers it covers.
#[test]
fn slice_agrees_with_the_full_sweep() {
    let Some(solver) = BasisComputeBackend::try_load_distance_solver(BASE_INTERVAL_MS, "") else {
        println!("skipped: {}", BasisComputeBackend::status());
        return;
    };

    const PLAYERS: usize = 256;
    const SLICE_START: usize = 64;
    const SLICE_END: usize = 192;
    let (x, y, z) = build_roster(PLAYERS);

    let full = BasisDistanceSolveRequest { pos_x: x.clone(), pos_y: y.clone(), pos_z: z.clone(), player_count: PLAYERS, slice_start: 0, slice_end: PLAYERS, parameters: parameters() };
    let mut full_interval = vec![0u8; PLAYERS * PLAYERS];
    let mut full_quality = vec![0u8; PLAYERS * PLAYERS];
    solver.solve(&full, &mut full_interval, &mut full_quality);

    let slice = BasisDistanceSolveRequest { pos_x: x, pos_y: y, pos_z: z, player_count: PLAYERS, slice_start: SLICE_START, slice_end: SLICE_END, parameters: parameters() };
    let mut slice_interval = vec![0u8; (SLICE_END - SLICE_START) * PLAYERS];
    let mut slice_quality = vec![0u8; (SLICE_END - SLICE_START) * PLAYERS];
    solver.solve(&slice, &mut slice_interval, &mut slice_quality);

    for s in 0..SLICE_END - SLICE_START {
        for j in 0..PLAYERS {
            let from_full = (SLICE_START + s) * PLAYERS + j;
            let from_slice = s * PLAYERS + j;
            assert_eq!(full_interval[from_full], slice_interval[from_slice]);
            assert_eq!(full_quality[from_full], slice_quality[from_slice]);
        }
    }
}

#[test]
fn device_selector_by_index_picks_the_first_device() {
    let Some(auto) = BasisComputeBackend::try_load_distance_solver(BASE_INTERVAL_MS, "") else {
        println!("skipped: {}", BasisComputeBackend::status());
        return;
    };
    let auto_name = auto.device_name().to_string();
    let by_index = BasisComputeBackend::try_load_distance_solver(BASE_INTERVAL_MS, "0").expect("device 0");
    assert_eq!(auto_name, by_index.device_name());
}

/// A selector naming a device this host does not have must refuse, not quietly run somewhere else.
#[test]
fn device_selector_unknown_refuses_rather_than_falling_back() {
    if BasisComputeBackend::try_load_distance_solver(BASE_INTERVAL_MS, "").is_none() {
        println!("skipped: {}", BasisComputeBackend::status());
        return;
    }
    let bogus = BasisComputeBackend::try_load_distance_solver(BASE_INTERVAL_MS, "no-such-device-xyz");
    let status = BasisComputeBackend::status();
    println!("status: {status}");
    assert!(bogus.is_none());
    assert!(status.contains("no-such-device-xyz"), "{status}");
}

#[test]
fn device_selector_out_of_range_index_refuses() {
    if BasisComputeBackend::try_load_distance_solver(BASE_INTERVAL_MS, "").is_none() {
        println!("skipped: {}", BasisComputeBackend::status());
        return;
    }
    let bogus = BasisComputeBackend::try_load_distance_solver(BASE_INTERVAL_MS, "99");
    let status = BasisComputeBackend::status();
    println!("status: {status}");
    assert!(bogus.is_none());
    assert!(status.contains("out of range"), "{status}");
}

/// The faster refresh is only taken while a device is actually carrying the sweep, keyed off the
/// live solver rather than off configuration — so it has to hold at the moment the backend is
/// dropped, not just at startup.
#[test]
#[serial(reduction_statics)]
fn refresh_period_tracks_whether_a_device_is_actually_carrying_the_sweep() {
    let saved_cpu = BasisServerReductionSystemEvents::distance_update_interval_ticks();
    let saved_gpu = BasisServerReductionSystemEvents::compute_distance_update_interval_ticks();
    BasisServerReductionSystemEvents::set_distance_update_interval_ticks(125);
    BasisServerReductionSystemEvents::set_compute_distance_update_interval_ticks(32);

    let mut distance = DistanceSweepState { distance_solver: None, ..Default::default() };
    assert_eq!(BasisServerReductionSystemEvents::test_only_effective_distance_interval_ticks(&distance), 125);

    match BasisComputeBackend::try_load_distance_solver(50, "") {
        None => println!("no device; only the CPU half of this rule is exercised: {}", BasisComputeBackend::status()),
        Some(solver) => {
            distance.distance_solver = Some(solver);
            assert_eq!(BasisServerReductionSystemEvents::test_only_effective_distance_interval_ticks(&distance), 32);
            // Losing the backend must put the period back on the spot.
            distance.distance_solver = None;
            assert_eq!(BasisServerReductionSystemEvents::test_only_effective_distance_interval_ticks(&distance), 125);
        }
    }

    BasisServerReductionSystemEvents::set_distance_update_interval_ticks(saved_cpu);
    BasisServerReductionSystemEvents::set_compute_distance_update_interval_ticks(saved_gpu);
}
