//! The CPU distance sweep, checked against the scalar math whose results it caches: the cache a
//! full sweep leaves behind against `BasisNetworkCommons` plus the documented tier thresholds, and
//! the vector interval encoding against the protocol encoder over its whole input domain.

use std::sync::Arc;

use basis_network_core::BasisNetworkCommons;
use basis_network_core::mathematics::Vector3;
use basis_network_server::reduction::basis_server_reduction_system_events::MS_TO_TICK;
use basis_network_server::reduction::{BasisServerReductionSystemEvents, PlayerState};
use basis_server_tests::support::FakePeer;
use serial_test::serial;

type Roster = Vec<(i32, Arc<PlayerState>)>;

fn build_roster(players: usize, position: impl Fn(usize) -> Vector3) -> Roster {
    let max_id = (players.saturating_sub(1) * 3) + 1;
    (0..players)
        .map(|i| {
            let id = (i * 3) as i32 + 1;
            let state = Arc::new(PlayerState::new(id, FakePeer::new(id).as_ref(), position(i), max_id + 1));
            (id, state)
        })
        .collect()
}

fn expected_quality(dist_sq: f32) -> u8 {
    if dist_sq <= BasisServerReductionSystemEvents::high_distance_sq() {
        3
    } else if dist_sq <= BasisServerReductionSystemEvents::medium_distance_sq() {
        2
    } else if dist_sq <= BasisServerReductionSystemEvents::low_distance_sq() {
        1
    } else {
        0
    }
}

fn assert_cache_matches_scalar_math(roster: &Roster) {
    let base_interval_ms = BasisServerReductionSystemEvents::bsrs_millisecond_default_interval();
    let base_multiplier = BasisServerReductionSystemEvents::bsr_base_multiplier();
    let increase_rate = BasisServerReductionSystemEvents::bsrs_increase_rate();

    for (id, state) in roster {
        let receiver = state.receiver.lock();
        for (other_id, other) in roster {
            if id == other_id {
                continue;
            }
            let d = state.position() - other.position();
            let dist_sq = d.x * d.x + d.y * d.y + d.z * d.z;

            let raw_interval = (base_interval_ms as f32 * (base_multiplier + dist_sq * increase_rate)) as i32;
            let expected_byte = BasisNetworkCommons::encode_avatar_interval_byte(raw_interval, base_interval_ms);
            let expected_ms = BasisNetworkCommons::decode_avatar_interval_ms(expected_byte, base_interval_ms);

            let cached = &receiver.peer_tracking[*other_id as usize];
            assert_eq!(cached.cached_interval_byte, expected_byte, "{id}->{other_id} interval byte");
            assert_eq!(cached.cached_quality_index, expected_quality(dist_sq), "{id}->{other_id} quality");
            assert_eq!(cached.cached_interval_ticks, (expected_ms as f64 * MS_TO_TICK) as i32, "{id}->{other_id} ticks");
        }
    }
}

#[test]
#[serial(reduction_statics)]
fn sweep_caches_what_the_scalar_math_would_produce() {
    for players in [1usize, 3, 8, 9, 33, 64] {
        let roster = build_roster(players, |i| Vector3::new(((i % 4) as f32 * 15.0) + ((i / 4) as f32 * 0.75), (i % 5) as f32 * 0.5, (i % 7) as f32 * 1.3));
        BasisServerReductionSystemEvents::test_only_run_distance_sweep(&roster);
        assert_cache_matches_scalar_math(&roster);
    }
}

#[test]
#[serial(reduction_statics)]
fn quality_tiers_are_inclusive_at_their_boundaries() {
    assert_eq!(BasisServerReductionSystemEvents::high_distance_sq(), 100.0);
    assert_eq!(BasisServerReductionSystemEvents::medium_distance_sq(), 900.0);
    assert_eq!(BasisServerReductionSystemEvents::low_distance_sq(), 2500.0);

    let on_boundary = [0f32, 10.0, 30.0, 50.0, 60.0];
    let roster = build_roster(16, |i| Vector3::new(if i < on_boundary.len() { on_boundary[i] } else { 100.0 + i as f32 * 40.0 }, 0.0, 0.0));
    BasisServerReductionSystemEvents::test_only_run_distance_sweep(&roster);

    let from_origin = roster[0].1.receiver.lock();
    assert_eq!(from_origin.peer_tracking[roster[1].0 as usize].cached_quality_index, 3);
    assert_eq!(from_origin.peer_tracking[roster[2].0 as usize].cached_quality_index, 2);
    assert_eq!(from_origin.peer_tracking[roster[3].0 as usize].cached_quality_index, 1);
    assert_eq!(from_origin.peer_tracking[roster[4].0 as usize].cached_quality_index, 0);
    drop(from_origin);

    assert_cache_matches_scalar_math(&roster);
}

#[test]
fn vector_interval_encoding_matches_the_protocol() {
    for base_interval_ms in [20, 33, 50, 100] {
        let limit = base_interval_ms + i32::from(BasisNetworkCommons::AVATAR_INTERVAL_EXTENDED_START) + (i32::from(u8::MAX) - i32::from(BasisNetworkCommons::AVATAR_INTERVAL_EXTENDED_START)) * BasisNetworkCommons::AVATAR_INTERVAL_EXTENDED_STEP_MS + BasisNetworkCommons::AVATAR_INTERVAL_EXTENDED_STEP_MS;

        let mut raw = vec![i32::MIN, i32::MIN + 1, -1];
        raw.extend(0..=limit);
        raw.push(i32::MAX - 1);
        raw.push(i32::MAX);

        let (encoded, actual_ms) = BasisServerReductionSystemEvents::test_only_encode_avatar_intervals(&raw, base_interval_ms);
        for i in 0..raw.len() {
            let expected_byte = BasisNetworkCommons::encode_avatar_interval_byte(raw[i], base_interval_ms);
            assert!((0..=255).contains(&encoded[i]));
            assert_eq!(encoded[i] as u8, expected_byte, "raw {}", raw[i]);
            assert_eq!(actual_ms[i], BasisNetworkCommons::decode_avatar_interval_ms(expected_byte, base_interval_ms));
        }
    }
}
