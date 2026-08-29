//! Exercises the REAL server serialization — `pre_serialize_keyframe` and `pre_serialize_delta`
//! (not a replica) — and parses the emitted frames the way the client does, confirming the
//! keyframe payload and the delta reconstruction match for byte/ushort ids, every quality, and
//! with/without additional data.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use basis_network_core::BasisNetworkCommons;
use basis_network_core::SerializableBasis::{AdditionalAvatarData, LocalAvatarSyncMessage};
use basis_network_core::compression::{BasisAvatarDeltaCompression, BitQuality};
use basis_network_core::mathematics::Vector3;
use basis_network_server::reduction::{BasisServerReductionSystemEvents, PlayerState};
use basis_server_tests::support::DeltaTestSupport as S;
use basis_server_tests::support::FakePeer;
use basis_server_tests::support::delta_test_support::TestRng;

fn make_state(player_id: u16, small_id: bool, out_seq: u8, kf_seq: u8, has_additional: bool) -> Arc<PlayerState> {
    let peer = FakePeer::new(player_id as i32);
    let state = Arc::new(PlayerState::new(player_id as i32, peer.as_ref(), Vector3::default(), 4));
    state.small_id.store(small_id, Ordering::Relaxed);
    {
        let mut sender = state.sender.lock();
        sender.outbound_sequence = out_seq;
        sender.keyframe_sequence = kf_seq;
        sender.has_additional_data = has_additional;
    }
    state
}

#[test]
fn real_keyframe_then_delta_round_trips() {
    for (q, small_id) in [(BitQuality::VeryLow, true), (BitQuality::Low, true), (BitQuality::Medium, true), (BitQuality::High, true), (BitQuality::High, false), (BitQuality::Low, false)] {
        let qi = q as usize;
        let payload_size = S::payload_size(q);
        let mut rng = TestRng::new((qi * 7 + if small_id { 0 } else { 99 }) as u64);
        let kf_payload = S::make_realistic_payload(q, &mut rng);
        let cur_payload = S::make_realistic_payload(q, &mut rng);
        let player_id: u16 = if small_id { 200 } else { 5000 };
        let id_size = if small_id { 1 } else { 2 };

        let state = make_state(player_id, small_id, 50, 50, false);
        let mut sender = state.sender.lock();

        // --- real keyframe serialization: [id][interval][seq][payload] ---
        sender.avatar_high = LocalAvatarSyncMessage { array: Some(kf_payload.clone()), data_quality_level: qi as u8, ..Default::default() };
        // The keyframe serializer reads the tier's own message; point every tier at this payload.
        sender.avatar_medium = sender.avatar_high.clone();
        sender.avatar_low = sender.avatar_high.clone();
        sender.avatar_very_low = sender.avatar_high.clone();
        BasisServerReductionSystemEvents::test_only_pre_serialize_keyframe(&state, &mut sender, qi, player_id);
        let kbuf = sender.serialized_keyframe[qi].clone();
        let klen = kbuf.len();
        assert!(klen > 0);
        let mut ko = id_size + 1; // skip id + interval
        assert_eq!(kbuf[ko], 50); // sequence
        ko += 1;
        assert_eq!(&kf_payload[..payload_size], &kbuf[ko..ko + payload_size]);
        assert_eq!(klen, id_size + 2 + payload_size);

        // Baseline the state on that keyframe (as pre_serialize_frame would) then emit a delta.
        sender.keyframe_payload[qi] = kf_payload.clone();
        sender.keyframe_payload_length[qi] = payload_size;
        sender.outbound_sequence = 51;

        let cur_msg = LocalAvatarSyncMessage { array: Some(cur_payload.clone()), data_quality_level: qi as u8, ..Default::default() };
        sender.avatar_high = cur_msg.clone();
        sender.avatar_medium = cur_msg.clone();
        sender.avatar_low = cur_msg.clone();
        sender.avatar_very_low = cur_msg;
        BasisServerReductionSystemEvents::test_only_pre_serialize_delta(&state, &mut sender, qi, player_id);
        let dbuf = sender.serialized_delta[qi].clone();
        let dlen = dbuf.len();
        assert!(dlen > 0);

        let mut p = 0;
        let header = dbuf[p];
        p += 1;
        assert_eq!(BasisNetworkCommons::delta_header_quality(header), qi as u8);
        assert!(!BasisNetworkCommons::delta_header_has_additional_data(header));
        assert_eq!(BasisNetworkCommons::delta_header_large_id(header), !small_id);
        let pid = if small_id { dbuf[p] as u16 } else { u16::from_le_bytes([dbuf[p], dbuf[p + 1]]) };
        p += id_size;
        assert_eq!(pid, player_id);
        assert_eq!(dbuf[p], 0); // interval placeholder (patched per receiver at send)
        assert_eq!(dbuf[p + 1], 51); // sequence
        assert_eq!(dbuf[p + 2], 50); // baseSeq == keyframe sequence
        p += 3;

        let body = BasisAvatarDeltaCompression::delta_body_length(&dbuf, p, dlen - p, q).unwrap();
        let mut recon = vec![0u8; payload_size];
        assert!(BasisAvatarDeltaCompression::try_apply_delta(&kf_payload, &dbuf, p, body, q, &mut recon));
        assert_eq!(cur_payload, recon);
        assert_eq!(dlen, p + body); // no trailing bytes without additional data
    }
}

#[test]
fn real_delta_with_additional_data_carries_trailer_and_reconstructs() {
    for (q, small_id) in [(BitQuality::High, true), (BitQuality::Medium, false)] {
        let qi = q as usize;
        let payload_size = S::payload_size(q);
        let mut rng = TestRng::new(qi as u64 + 500);
        let kf_payload = S::make_realistic_payload(q, &mut rng);
        let cur_payload = S::make_realistic_payload(q, &mut rng);
        let player_id: u16 = if small_id { 7 } else { 9000 };
        let id_size = if small_id { 1 } else { 2 };

        let state = make_state(player_id, small_id, 51, 40, true);
        let mut sender = state.sender.lock();
        sender.keyframe_payload[qi] = kf_payload.clone();
        sender.keyframe_payload_length[qi] = payload_size;

        let adds = vec![
            AdditionalAvatarData { array: Some(vec![9, 8, 7]), payload_size: 3, message_index: 2 },
            AdditionalAvatarData { array: Some(vec![1]), payload_size: 1, message_index: 5 },
        ];
        let cur_msg = LocalAvatarSyncMessage { array: Some(cur_payload.clone()), data_quality_level: qi as u8, additional_avatar_datas: Some(adds), additional_avatar_data_size: 2, linked_avatar_index: 3 };
        sender.avatar_high = cur_msg.clone();
        sender.avatar_medium = cur_msg.clone();
        sender.avatar_low = cur_msg.clone();
        sender.avatar_very_low = cur_msg;
        BasisServerReductionSystemEvents::test_only_pre_serialize_delta(&state, &mut sender, qi, player_id);
        let dbuf = sender.serialized_delta[qi].clone();
        let dlen = dbuf.len();

        let mut p = 0;
        let header = dbuf[p];
        p += 1;
        assert!(BasisNetworkCommons::delta_header_has_additional_data(header));
        p += id_size + 3; // id + interval + seq + baseSeq

        let body = BasisAvatarDeltaCompression::delta_body_length(&dbuf, p, dlen - p, q).unwrap();
        let mut recon = vec![0u8; payload_size];
        assert!(BasisAvatarDeltaCompression::try_apply_delta(&kf_payload, &dbuf, p, body, q, &mut recon));
        assert_eq!(cur_payload, recon);

        let add_start = p + body;
        assert!(add_start < dlen, "expected a trailing additional-data section");
        assert_eq!(dbuf[add_start], 2); // count
        assert_eq!(dbuf[add_start + 1], 3); // LinkedAvatarIndex
    }
}
