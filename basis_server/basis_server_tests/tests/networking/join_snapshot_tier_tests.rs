//! The join fill used to hand every joiner a High payload for every player in the instance,
//! however far away. These pin that it now picks the same tier the steady-state send loop would —
//! and, just as important, that it falls back to High rather than sending an empty payload
//! whenever the decision cannot be made safely.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use basis_network_core::SerializableBasis::LocalAvatarSyncMessage;
use basis_network_core::compression::{BasisAvatarBitPacking, BitQuality};
use basis_network_core::mathematics::Vector3;
use basis_network_server::reduction::{BasisServerReductionSystemEvents, PlayerState};
use basis_server_tests::support::FakePeer;
use serial_test::serial;

fn payload(q: BitQuality, fill: u8) -> Vec<u8> {
    vec![fill; BasisAvatarBitPacking::convert_to_size(q)]
}

fn tier(q: BitQuality, fill: u8) -> LocalAvatarSyncMessage {
    LocalAvatarSyncMessage { data_quality_level: q as u8, array: Some(payload(q, fill)), ..Default::default() }
}

/// Registers a subject at a position with all four tiers built, as the tick loop would.
fn subject(id: i32, position: Vector3, with_lower_tiers: bool, bypass: bool) -> Arc<PlayerState> {
    let state = Arc::new(PlayerState::new(id, FakePeer::new(id).as_ref(), position, 1));
    state.bypass_reduction.store(bypass, Ordering::Relaxed);
    {
        let mut sender = state.sender.lock();
        sender.avatar_high = tier(BitQuality::High, 0xAA);
        if with_lower_tiers {
            sender.avatar_medium = tier(BitQuality::Medium, 0xBB);
            sender.avatar_low = tier(BitQuality::Low, 0xCC);
            sender.avatar_very_low = tier(BitQuality::VeryLow, 0xDD);
        }
    }
    BasisServerReductionSystemEvents::test_only_insert_player_state(id, state.clone());
    state
}

struct Remove(i32);

impl Drop for Remove {
    fn drop(&mut self) {
        BasisServerReductionSystemEvents::remove_player(self.0);
    }
}

// 5m -> High(<=10), 20m -> Medium(<=30), 40m -> Low(<=50), 500m -> VeryLow
#[test]
#[serial(reduction_statics)]
fn tier_matches_the_steady_state_distance_thresholds() {
    for (metres, expected) in [(5.0f32, BitQuality::High), (20.0, BitQuality::Medium), (40.0, BitQuality::Low), (500.0, BitQuality::VeryLow)] {
        const ID: i32 = 61001;
        let _r = Remove(ID);
        subject(ID, Vector3::new(metres, 0.0, 0.0), true, false);

        let snapshot = BasisServerReductionSystemEvents::try_get_join_snapshot(Vector3::default(), ID).expect("snapshot");
        assert_eq!(snapshot.data_quality_level, expected as u8, "{metres}m");
        assert_eq!(snapshot.array.map(|a| a.len()), Some(BasisAvatarBitPacking::convert_to_size(expected)), "{metres}m");
    }
}

/// The whole point: at crowd scale nearly everyone is past the VeryLow threshold, so the join
/// payload should be less than half what it used to be for those players.
#[test]
#[serial(reduction_statics)]
fn distant_player_costs_far_less_than_the_old_high_payload() {
    const ID: i32 = 61002;
    let _r = Remove(ID);
    subject(ID, Vector3::new(400.0, 0.0, 0.0), true, false);

    let snapshot = BasisServerReductionSystemEvents::try_get_join_snapshot(Vector3::default(), ID).expect("snapshot");
    let high = BasisAvatarBitPacking::convert_to_size(BitQuality::High);
    let len = snapshot.array.map(|a| a.len()).unwrap_or(0);
    assert!(len * 2 < high, "expected well under half of {high}B, got {len}B");
}

/// Distance is measured between the two players, not from the origin — a joiner standing next to
/// someone far from spawn must still get High for them.
#[test]
#[serial(reduction_statics)]
fn tier_is_relative_to_the_viewer_not_the_origin() {
    const ID: i32 = 61003;
    let _r = Remove(ID);
    subject(ID, Vector3::new(500.0, 0.0, 0.0), true, false);

    let beside = Vector3::new(502.0, 0.0, 0.0);
    let snapshot = BasisServerReductionSystemEvents::try_get_join_snapshot(beside, ID).expect("snapshot");
    assert_eq!(snapshot.data_quality_level, BitQuality::High as u8);
}

/// The repacker leaves the lower arrays empty when a repack fails, and the tick may not have built
/// them yet for a player who just connected. Sending that as-is would be an empty avatar.
#[test]
#[serial(reduction_statics)]
fn missing_lower_tier_falls_back_to_high_rather_than_an_empty_payload() {
    const ID: i32 = 61004;
    let _r = Remove(ID);
    subject(ID, Vector3::new(500.0, 0.0, 0.0), false, false);

    let snapshot = BasisServerReductionSystemEvents::try_get_join_snapshot(Vector3::default(), ID).expect("snapshot");
    assert_eq!(snapshot.data_quality_level, BitQuality::High as u8);
    assert!(snapshot.array.is_some());
}

#[test]
#[serial(reduction_statics)]
fn bypass_reduction_always_gets_high() {
    const ID: i32 = 61005;
    let _r = Remove(ID);
    subject(ID, Vector3::new(5000.0, 0.0, 0.0), true, true);

    let snapshot = BasisServerReductionSystemEvents::try_get_join_snapshot(Vector3::default(), ID).expect("snapshot");
    assert_eq!(snapshot.data_quality_level, BitQuality::High as u8);
}

#[test]
#[serial(reduction_statics)]
fn unknown_subject_reports_no_snapshot() {
    assert!(BasisServerReductionSystemEvents::try_get_join_snapshot(Vector3::default(), 61999).is_none());
}

#[test]
#[serial(reduction_statics)]
fn subject_with_no_pose_yet_reports_no_snapshot() {
    const ID: i32 = 61006;
    let _r = Remove(ID);
    let state = Arc::new(PlayerState::new(ID, FakePeer::new(ID).as_ref(), Vector3::default(), 1));
    BasisServerReductionSystemEvents::test_only_insert_player_state(ID, state);

    assert!(BasisServerReductionSystemEvents::try_get_join_snapshot(Vector3::default(), ID).is_none());
}
