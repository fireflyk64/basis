//! End-to-end AdditionalAvatarData (face tracking / behaviour params) transport: sender wire bytes
//! → server ingest → reduction-system pre-serialization → receiver parse. Every hop uses the REAL
//! serializers over actual byte buffers, so any wire-layout drift that would silently drop face
//! data fails here instead of in-game.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use basis_network_core::SerializableBasis::{AdditionalAvatarData, LocalAvatarSyncMessage, ServerSideSyncPlayerMessage};
use basis_network_core::compression::{BasisAvatarBitPacking, BasisAvatarBundleCodec, BasisAvatarDeltaCompression, BitQuality};
use basis_network_core::mathematics::Vector3;
use basis_network_core::{BasisNetworkCommons, NetDataReader, NetDataWriter};
use basis_network_server::reduction::{AvatarQualityRepacker, BasisServerReductionSystemEvents, PendingAvatarSend, PlayerState, ReceiverData};
use basis_server_tests::support::DeltaTestSupport as S;
use basis_server_tests::support::FakePeer;
use basis_server_tests::support::delta_test_support::TestRng;
use serial_test::serial;

const FACE_BYTES: [u8; 8] = [16, 3, 200, 150, 100, 50, 25, 12]; // HVR high-frequency variables shape
const FACE_MESSAGE_INDEX: u8 = 1; // HVR VariableNetworkingCarrier slot
const LINKED_INDEX: u8 = 5;

struct StripGuard(bool);

impl StripGuard {
    fn new(value: bool) -> Self {
        let saved = BasisServerReductionSystemEvents::strip_additional_data_at_low_quality();
        BasisServerReductionSystemEvents::set_strip_additional_data_at_low_quality(value);
        Self(saved)
    }
}

impl Drop for StripGuard {
    fn drop(&mut self) {
        BasisServerReductionSystemEvents::set_strip_additional_data_at_low_quality(self.0);
    }
}

fn entry(message_index: u8, bytes: &[u8]) -> AdditionalAvatarData {
    AdditionalAvatarData { message_index, payload_size: bytes.len() as u8, array: Some(bytes.to_vec()) }
}

fn make_additional() -> Vec<AdditionalAvatarData> {
    vec![entry(FACE_MESSAGE_INDEX, &FACE_BYTES)]
}

fn assert_face_survived(msg: &LocalAvatarSyncMessage) {
    assert!(msg.additional_avatar_data_size > 0, "additional data was dropped");
    let datas = msg.additional_avatar_datas.as_ref().expect("additional entries");
    assert_eq!(msg.additional_avatar_data_size, 1);
    assert_eq!(datas[0].message_index, FACE_MESSAGE_INDEX);
    assert_eq!(datas[0].array.as_deref(), Some(&FACE_BYTES[..]));
    assert_eq!(msg.linked_avatar_index, LINKED_INDEX);
}

// ── Sender-side framing, exactly as the compressor writes it ──

fn write_uplink_keyframe(seq: u8, payload: &[u8], additional: Option<Vec<AdditionalAvatarData>>) -> NetDataWriter {
    let mut lasm = LocalAvatarSyncMessage { array: Some(payload.to_vec()), additional_avatar_datas: additional, linked_avatar_index: LINKED_INDEX, ..Default::default() };
    let mut w = NetDataWriter::new();
    w.put_byte(seq);
    lasm.serialize_for_channel(&mut w, BitQuality::High).unwrap();
    w
}

fn write_uplink_delta(seq: u8, base_seq: u8, baseline: &[u8], current: &[u8], additional: Option<Vec<AdditionalAvatarData>>) -> NetDataWriter {
    let mut scratch = vec![0u8; BasisAvatarDeltaCompression::max_delta_size(BitQuality::High)];
    let delta_len = BasisAvatarDeltaCompression::build_delta(baseline, current, BitQuality::High, &mut scratch, 0).unwrap();
    assert!(delta_len > 0 && delta_len < current.len(), "test expects a genuine delta, not a promotion");

    let has_additional = additional.as_ref().is_some_and(|a| !a.is_empty());
    let mut lasm = LocalAvatarSyncMessage { array: Some(current.to_vec()), additional_avatar_datas: additional, linked_avatar_index: LINKED_INDEX, ..Default::default() };
    let mut w = NetDataWriter::new();
    w.put_byte(BasisNetworkCommons::build_delta_header(3, has_additional, false));
    w.put_byte(seq);
    w.put_byte(base_seq);
    w.put_bytes(&scratch[..delta_len]);
    if has_additional {
        lasm.serialize_additional_only(&mut w).unwrap();
    }
    w
}

// ── Server ingest, mirroring handle_avatar_movement / handle_delta_channel_inbound ──

fn ingest_keyframe(client_wire: &NetDataWriter, channel_says_additional: bool) -> (LocalAvatarSyncMessage, u8) {
    let mut reader = NetDataReader::new(client_wire.copy_data());
    let seq = reader.try_get_byte().expect("seq");
    let mut msg = LocalAvatarSyncMessage::default();
    msg.deserialize_for_channel(&mut reader, 3, channel_says_additional).unwrap();
    assert_eq!(reader.available_bytes(), 0); // whole frame consumed — no trailing garbage
    (msg, seq)
}

fn ingest_delta(client_wire: &NetDataWriter, server_baseline: &[u8], expected_base_seq: u8) -> (LocalAvatarSyncMessage, u8) {
    let mut reader = NetDataReader::new(client_wire.copy_data());
    let header = reader.try_get_byte().expect("header");
    assert!(!BasisNetworkCommons::is_delta_control_header(header));
    assert_eq!(BasisNetworkCommons::delta_header_quality(header), 3);
    let has_additional = BasisNetworkCommons::delta_header_has_additional_data(header);
    let seq = reader.try_get_byte().expect("seq");
    let base_seq = reader.try_get_byte().expect("baseSeq");
    assert_eq!(base_seq, expected_base_seq);

    let body_len = BasisAvatarDeltaCompression::delta_body_length(reader.raw_data(), reader.position(), reader.available_bytes(), BitQuality::High).expect("delta body length probe failed");
    assert!(body_len > 0 && body_len <= reader.available_bytes());

    let mut array = vec![0u8; BasisAvatarDeltaCompression::payload_size(BitQuality::High)];
    assert!(BasisAvatarDeltaCompression::try_apply_delta(server_baseline, reader.raw_data(), reader.position(), body_len, BitQuality::High, &mut array));
    reader.skip_bytes(body_len);

    let mut msg = LocalAvatarSyncMessage { array: Some(array), data_quality_level: 3, additional_avatar_data_size: 0, additional_avatar_datas: None, ..Default::default() };
    if has_additional {
        msg.deserialize_additional_data(&mut reader).unwrap();
    }
    assert_eq!(reader.available_bytes(), 0); // additional section consumed exactly
    (msg, seq)
}

// ── Server state builder, mirroring process_message ──

fn replace_high(state: &PlayerState, inbound: &LocalAvatarSyncMessage, outbound_seq: u8) {
    let expected = BasisAvatarBitPacking::convert_to_size(BitQuality::High);
    let owned = inbound.array.as_deref().unwrap()[..expected].to_vec();
    let high = LocalAvatarSyncMessage {
        data_quality_level: 3,
        additional_avatar_datas: inbound.additional_avatar_datas.clone(),
        additional_avatar_data_size: inbound.additional_avatar_data_size,
        linked_avatar_index: inbound.linked_avatar_index,
        array: Some(owned),
    };
    let mut guard = state.sender.lock();
    let sender: &mut basis_network_server::reduction::SenderWork = &mut guard;
    sender.avatar_high = high.clone();
    sender.high_array_actual_size = expected;
    sender.outbound_sequence = outbound_seq;
    AvatarQualityRepacker::build_all_lower_from_high_into(&high, &mut sender.avatar_medium, &mut sender.avatar_low, &mut sender.avatar_very_low).unwrap();
    BasisServerReductionSystemEvents::test_only_propagate_additional_data(sender);
    sender.has_additional_data = high.additional_avatar_datas.as_ref().is_some_and(|d| !d.is_empty());
}

fn build_state(inbound: &LocalAvatarSyncMessage, player_id: u16, outbound_seq: u8) -> Arc<PlayerState> {
    let peer = FakePeer::new(player_id as i32);
    let state = Arc::new(PlayerState::new(player_id as i32, peer.as_ref(), Vector3::default(), 4));
    state.small_id.store(player_id <= u8::MAX as u16, Ordering::Relaxed);
    state.data_generation.store(1, Ordering::Relaxed);
    replace_high(&state, inbound, outbound_seq);
    state
}

// ── Receiver-side parsing, mirroring the client handlers ──

fn parse_fanout_keyframe(wire: &[u8], channel: u8) -> ServerSideSyncPlayerMessage {
    let mut reader = NetDataReader::from_slice(wire);
    let mut ssm = ServerSideSyncPlayerMessage::default();
    let quality = BasisNetworkCommons::get_quality_from_channel(channel);
    let has_additional = BasisNetworkCommons::channel_has_additional_data(channel);
    let large_id = BasisNetworkCommons::is_large_player_id_channel(channel);
    ssm.deserialize_for_channel_sized(&mut reader, quality, has_additional, large_id).unwrap();
    assert_eq!(reader.available_bytes(), 0);
    ssm
}

fn parse_fanout_delta(wire: &[u8], receiver_baseline: &[u8], expected_base_seq: u8) -> (ServerSideSyncPlayerMessage, u8) {
    let mut reader = NetDataReader::from_slice(wire);
    let header = reader.try_get_byte().expect("header");
    assert!(!BasisNetworkCommons::is_delta_control_header(header));
    let quality = BasisNetworkCommons::delta_header_quality(header);
    let has_additional = BasisNetworkCommons::delta_header_has_additional_data(header);
    let large_id = BasisNetworkCommons::delta_header_large_id(header);

    let player_id = if large_id { reader.get_ushort().unwrap() } else { reader.get_byte().unwrap() as u16 };
    let _interval = reader.try_get_byte().expect("interval");
    let sequence = reader.try_get_byte().expect("sequence");
    let base_seq = reader.try_get_byte().expect("baseSeq");
    assert_eq!(base_seq, expected_base_seq);

    let q = BitQuality::from_byte(quality).unwrap();
    let body_len = BasisAvatarDeltaCompression::delta_body_length(reader.raw_data(), reader.position(), reader.available_bytes(), q).unwrap();
    assert!(body_len > 0 && body_len <= reader.available_bytes());

    let mut recon = vec![0u8; BasisAvatarDeltaCompression::payload_size(q)];
    assert!(BasisAvatarDeltaCompression::try_apply_delta(receiver_baseline, reader.raw_data(), reader.position(), body_len, q, &mut recon));
    reader.skip_bytes(body_len);

    let mut ssm = ServerSideSyncPlayerMessage::default();
    ssm.player_id_message.player_id = player_id;
    ssm.sequence = sequence;
    ssm.avatar_serialization = LocalAvatarSyncMessage { array: Some(recon), data_quality_level: quality, ..Default::default() };
    if has_additional {
        ssm.avatar_serialization.deserialize_additional_data(&mut reader).unwrap();
    }
    assert_eq!(reader.available_bytes(), 0);
    (ssm, quality)
}

/// The published frame's keyframe wire for one quality.
fn frame_keyframe(state: &PlayerState, qi: usize) -> Vec<u8> {
    state.frame.load().serialized_keyframe[qi].as_ref().map(|a| a.to_vec()).unwrap_or_default()
}

fn frame_delta(state: &PlayerState, qi: usize) -> Vec<u8> {
    state.frame.load().serialized_delta[qi].as_ref().map(|a| a.to_vec()).unwrap_or_default()
}

fn frame_has_additional(state: &PlayerState, qi: usize) -> bool {
    state.frame.load().serialized_has_additional[qi]
}

/// `pre_serialize_keyframe` on the state's own High message; returns (wire, has_additional).
fn pre_serialize_keyframe(state: &PlayerState, qi: usize, player_id: u16) -> (Vec<u8>, bool) {
    let mut sender = state.sender.lock();
    BasisServerReductionSystemEvents::test_only_pre_serialize_keyframe(state, &mut sender, qi, player_id);
    (sender.serialized_keyframe[qi].clone(), sender.keyframe_has_additional[qi])
}

// ─────────────────────────────────────────────────────────────────────────────
//  Tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn uplink_keyframe_with_face_data_survives_server_ingest() {
    let mut rng = TestRng::new(1001);
    let payload = S::make_realistic_payload(BitQuality::High, &mut rng);

    let wire = write_uplink_keyframe(7, &payload, Some(make_additional()));
    // Client picks the odd (additional) channel; the server derives hasAdditional from it.
    let channel = BasisNetworkCommons::get_player_avatar_channel_for_quality(3, true);
    assert!(BasisNetworkCommons::channel_has_additional_data(channel));

    let (ingested, seq) = ingest_keyframe(&wire, true);
    assert_eq!(seq, 7);
    assert_eq!(ingested.array.as_deref(), Some(&payload[..]));
    assert_face_survived(&ingested);
}

#[test]
fn ready_snapshot_self_describing_path_round_trips_face_data() {
    let mut rng = TestRng::new(1005);
    let payload_a = S::make_realistic_payload(BitQuality::High, &mut rng);
    let payload_b = S::make_realistic_payload(BitQuality::High, &mut rng);

    let mut with_face = LocalAvatarSyncMessage { array: Some(payload_a.clone()), additional_avatar_datas: Some(make_additional()), linked_avatar_index: LINKED_INDEX, ..Default::default() };
    let mut without_face = LocalAvatarSyncMessage { array: Some(payload_b.clone()), ..Default::default() };

    let mut w = NetDataWriter::new();
    with_face.serialize(&mut w, Some(BitQuality::High)).unwrap();
    without_face.serialize(&mut w, Some(BitQuality::High)).unwrap();

    let mut reader = NetDataReader::new(w.copy_data());
    let mut first = LocalAvatarSyncMessage::default();
    first.deserialize(&mut reader).unwrap();
    assert_eq!(first.array.as_deref(), Some(&payload_a[..]));
    assert_face_survived(&first);

    let mut second = LocalAvatarSyncMessage::default();
    second.deserialize(&mut reader).unwrap();
    assert_eq!(second.array.as_deref(), Some(&payload_b[..]));
    assert_eq!(second.additional_avatar_data_size, 0);
    assert!(second.additional_avatar_datas.is_none());
    assert_eq!(reader.available_bytes(), 0);
}

#[test]
fn uplink_delta_with_face_data_survives_server_ingest() {
    let mut rng = TestRng::new(1002);
    let baseline = S::make_realistic_payload(BitQuality::High, &mut rng);
    let mut current = baseline.clone();
    current[0] ^= 0xFF;
    S::flip_bone(&mut current, BitQuality::High, 12);

    let wire = write_uplink_delta(8, 7, &baseline, &current, Some(make_additional()));
    let (ingested, seq) = ingest_delta(&wire, &baseline, 7);
    assert_eq!(seq, 8);
    assert_eq!(ingested.array.as_deref(), Some(&current[..]));
    assert_face_survived(&ingested);
}

#[test]
fn uplink_delta_without_face_data_header_says_none() {
    let mut rng = TestRng::new(1003);
    let baseline = S::make_realistic_payload(BitQuality::High, &mut rng);
    let mut current = baseline.clone();
    S::flip_bone(&mut current, BitQuality::High, 3);

    let wire = write_uplink_delta(9, 7, &baseline, &current, None);
    let (ingested, _) = ingest_delta(&wire, &baseline, 7);
    assert_eq!(ingested.array.as_deref(), Some(&current[..]));
    assert_eq!(ingested.additional_avatar_data_size, 0);
    assert!(ingested.additional_avatar_datas.is_none());
}

#[test]
#[serial(reduction_statics)]
fn fanout_keyframe_high_carries_face_data_end_to_end() {
    let mut rng = TestRng::new(1004);
    let payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    let (ingested, _) = ingest_keyframe(&write_uplink_keyframe(1, &payload, Some(make_additional())), true);

    let state = build_state(&ingested, 42, 1);
    let (wire, has_additional) = pre_serialize_keyframe(&state, 3, 42);
    assert!(!wire.is_empty(), "keyframe was not serialized");
    assert!(has_additional, "server lost the additional flag at High");
    let channel = BasisNetworkCommons::get_player_avatar_channel_for_quality(3, has_additional);

    let ssm = parse_fanout_keyframe(&wire, channel);
    assert_eq!(ssm.player_id_message.player_id, 42);
    assert_eq!(ssm.avatar_serialization.array.as_deref(), Some(&payload[..]));
    assert_face_survived(&ssm.avatar_serialization);
}

#[test]
#[serial(reduction_statics)]
fn fanout_keyframe_medium_keeps_face_data_low_tiers_strip_it() {
    let _g = StripGuard::new(true);
    let mut rng = TestRng::new(1005);
    let payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    let (ingested, _) = ingest_keyframe(&write_uplink_keyframe(1, &payload, Some(make_additional())), true);
    let state = build_state(&ingested, 42, 1);

    for qi in 0..4 {
        let (wire, has_additional) = pre_serialize_keyframe(&state, qi, 42);
        assert!(!wire.is_empty(), "tier {qi} not serialized");
        let channel = BasisNetworkCommons::get_player_avatar_channel_for_quality(qi as i32, has_additional);
        let ssm = parse_fanout_keyframe(&wire, channel);
        if qi >= 2 {
            assert_face_survived(&ssm.avatar_serialization); // High + Medium keep face data
        } else {
            assert!(!has_additional, "tier {qi} should strip additional");
            assert_eq!(ssm.avatar_serialization.additional_avatar_data_size, 0);
        }
    }
}

#[test]
#[serial(reduction_statics)]
fn fanout_delta_high_carries_face_data_end_to_end() {
    let mut rng = TestRng::new(1006);

    // Generation 1: keyframe (no face this frame) establishes the baseline.
    let kf_payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    let (kf_ingested, _) = ingest_keyframe(&write_uplink_keyframe(1, &kf_payload, None), false);
    let state = build_state(&kf_ingested, 42, 1);

    let payload_size = BasisAvatarBitPacking::convert_to_size(BitQuality::High);
    {
        let mut sender = state.sender.lock();
        sender.keyframe_payload[3] = sender.avatar_high.array.as_deref().unwrap()[..payload_size].to_vec();
        sender.keyframe_payload_length[3] = payload_size;
        sender.keyframe_sequence = sender.outbound_sequence; // = 1
        BasisServerReductionSystemEvents::test_only_pre_serialize_keyframe(&state, &mut sender, 3, 42);
    }

    // The receiver captured that keyframe (sequence byte embedded in the wire = 1).
    let receiver_baseline = kf_payload[..payload_size].to_vec();

    // Generation 2: the avatar moved AND the wearer's face changed — delta with additional.
    let mut cur_payload = kf_payload.clone();
    cur_payload[0] ^= 0xFF;
    S::flip_bone(&mut cur_payload, BitQuality::High, 20);
    let delta_wire = write_uplink_delta(2, 1, &kf_payload, &cur_payload, Some(make_additional()));
    let (delta_ingested, _) = ingest_delta(&delta_wire, &kf_payload, 1);
    assert_face_survived(&delta_ingested); // face made it INTO the server

    replace_high(&state, &delta_ingested, 2);
    let wire = {
        let mut sender = state.sender.lock();
        BasisServerReductionSystemEvents::test_only_pre_serialize_delta(&state, &mut sender, 3, 42);
        sender.serialized_delta[3].clone()
    };
    assert!(!wire.is_empty(), "fan-out delta was not serialized");

    // Receiver reconstructs against the gen-1 keyframe and must recover the face bytes.
    let (ssm, quality) = parse_fanout_delta(&wire, &receiver_baseline, 1);
    assert_eq!(quality, 3);
    assert_eq!(ssm.player_id_message.player_id, 42);
    assert_eq!(ssm.avatar_serialization.array.as_deref(), Some(&cur_payload[..]));
    assert_face_survived(&ssm.avatar_serialization);
}

#[test]
#[serial(reduction_statics)]
fn fanout_via_pre_serialize_frame_delta_generation_keeps_face_data() {
    // Same as above but through the REAL pre_serialize_frame decision path: keyframe gen then delta gen.
    let mut rng = TestRng::new(1007);
    let kf_payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    let (kf_ingested, _) = ingest_keyframe(&write_uplink_keyframe(1, &kf_payload, Some(make_additional())), true);
    let state = build_state(&kf_ingested, 7, 1);

    // Gen 1 = forced keyframe (what process_message does for a new player).
    BasisServerReductionSystemEvents::test_only_pre_serialize_frame(&state, 1, true);
    assert!(state.frame.load().current_is_keyframe);
    assert!(!frame_keyframe(&state, 3).is_empty());
    assert!(frame_has_additional(&state, 3));

    let (receiver_baseline, base_seq) = {
        let sender = state.sender.lock();
        (sender.keyframe_payload[3][..sender.keyframe_payload_length[3]].to_vec(), sender.keyframe_sequence)
    };

    // Gen 2: small pose change + fresh face bytes → delta generation.
    let mut cur_payload = kf_payload.clone();
    S::flip_bone(&mut cur_payload, BitQuality::High, 9);
    let delta_wire = write_uplink_delta(2, 1, &kf_payload, &cur_payload, Some(make_additional()));
    let (delta_ingested, _) = ingest_delta(&delta_wire, &kf_payload, 1);
    replace_high(&state, &delta_ingested, 2);
    BasisServerReductionSystemEvents::test_only_pre_serialize_frame(&state, 2, false);

    assert!(!state.frame.load().current_is_keyframe, "a one-bone delta must not promote to keyframe");
    let wire = frame_delta(&state, 3);
    assert!(!wire.is_empty());

    let (ssm, _) = parse_fanout_delta(&wire, &receiver_baseline, base_seq);
    assert_eq!(ssm.avatar_serialization.array.as_deref(), Some(&cur_payload[..]));
    assert_face_survived(&ssm.avatar_serialization);
}

#[test]
fn p2p_splice_keyframe_carries_face_data() {
    // The P2P keyframe splice: [playerId:1][interval:1][clientWire...]
    let mut rng = TestRng::new(1008);
    let payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    let client_wire = write_uplink_keyframe(3, &payload, Some(make_additional()));
    let channel = BasisNetworkCommons::get_player_avatar_channel_for_quality(3, true);

    let mut spliced = NetDataWriter::new();
    spliced.put_byte(42); // localId (small)
    spliced.put_byte(0); // interval
    spliced.put_bytes(client_wire.as_read_only_span());

    let ssm = parse_fanout_keyframe(spliced.as_read_only_span(), channel);
    assert_eq!(ssm.player_id_message.player_id, 42);
    assert_eq!(ssm.sequence, 3);
    assert_eq!(ssm.avatar_serialization.array.as_deref(), Some(&payload[..]));
    assert_face_survived(&ssm.avatar_serialization);
}

#[test]
fn p2p_splice_delta_carries_face_data() {
    // The P2P delta splice: [hdr(+largeId)][playerId][interval][uplink frame after hdr...]
    let mut rng = TestRng::new(1009);
    let baseline = S::make_realistic_payload(BitQuality::High, &mut rng);
    let mut current = baseline.clone();
    S::flip_bone(&mut current, BitQuality::High, 30);

    let client_wire = write_uplink_delta(4, 3, &baseline, &current, Some(make_additional()));
    let raw = client_wire.copy_data();

    let mut spliced = NetDataWriter::new();
    spliced.put_byte(raw[0]); // header (small id — bit 3 unset)
    spliced.put_byte(42); // localId
    spliced.put_byte(0); // interval
    spliced.put_bytes(&raw[1..]);

    let (ssm, quality) = parse_fanout_delta(spliced.as_read_only_span(), &baseline, 3);
    assert_eq!(quality, 3);
    assert_eq!(ssm.player_id_message.player_id, 42);
    assert_eq!(ssm.avatar_serialization.array.as_deref(), Some(&current[..]));
    assert_face_survived(&ssm.avatar_serialization);
}

#[test]
fn pooled_message_reuse_reuses_entry_buffers_so_a_snapshot_must_clone() {
    // Both the entry list and each entry's payload buffer are retained and overwritten in place
    // when the next packet's sizes match — every frame of a steady face-tracking stream. A
    // consumer that keeps an entry beyond the next deserialize must therefore clone the payload.
    let mut rng = TestRng::new(1011);
    let payload = S::make_realistic_payload(BitQuality::High, &mut rng);

    let mut pooled = LocalAvatarSyncMessage::default();

    let wire1 = write_uplink_keyframe(1, &payload, Some(vec![entry(FACE_MESSAGE_INDEX, &[16, 1, 11, 0, 200])]));
    let mut reader1 = NetDataReader::new(wire1.copy_data());
    assert!(reader1.try_get_byte().is_some());
    pooled.deserialize_for_channel(&mut reader1, 3, true).unwrap();

    // Deep copy, as the receive path takes it.
    let cloned: Vec<AdditionalAvatarData> = pooled.additional_avatar_datas.clone().unwrap();

    // Packet 2 reuses the same pooled message (same entry count and payload size).
    let outer_before = pooled.additional_avatar_datas.as_ref().unwrap().as_ptr();
    let payload_buffer_before = pooled.additional_avatar_datas.as_ref().unwrap()[0].array.as_ref().unwrap().as_ptr();
    let wire2 = write_uplink_keyframe(2, &payload, Some(vec![entry(FACE_MESSAGE_INDEX, &[16, 1, 22, 0, 50])]));
    let mut reader2 = NetDataReader::new(wire2.copy_data());
    assert!(reader2.try_get_byte().is_some());
    pooled.deserialize_for_channel(&mut reader2, 3, true).unwrap();

    assert_eq!(outer_before, pooled.additional_avatar_datas.as_ref().unwrap().as_ptr(), "the entry list is reused in place");
    assert_eq!(payload_buffer_before, pooled.additional_avatar_datas.as_ref().unwrap()[0].array.as_ref().unwrap().as_ptr(), "the payload buffer is reused in place");
    assert_eq!(pooled.additional_avatar_datas.as_ref().unwrap()[0].array.as_deref(), Some(&[16, 1, 22, 0, 50][..]));

    // The cloned copy is what stays usable.
    assert_eq!(cloned[0].array.as_deref(), Some(&[16, 1, 11, 0, 200][..]));
}

#[test]
#[serial(reduction_statics)]
fn fanout_channel_odd_only_when_additional_present() {
    let mut rng = TestRng::new(1010);
    let payload = S::make_realistic_payload(BitQuality::High, &mut rng);

    // With face data → odd channel.
    let (with_face, _) = ingest_keyframe(&write_uplink_keyframe(1, &payload, Some(make_additional())), true);
    let s1 = build_state(&with_face, 42, 1);
    let (_, has1) = pre_serialize_keyframe(&s1, 3, 42);
    assert!(has1);
    assert_eq!(BasisNetworkCommons::get_player_avatar_channel_for_quality(3, has1), BasisNetworkCommons::PLAYER_AVATAR_HIGH_ADDITIONAL_CHANNEL);

    // Without → even channel; a parse expecting additional data must not be attempted.
    let (no_face, _) = ingest_keyframe(&write_uplink_keyframe(2, &payload, None), false);
    let s2 = build_state(&no_face, 42, 2);
    let (_, has2) = pre_serialize_keyframe(&s2, 3, 42);
    assert!(!has2);
    assert_eq!(BasisNetworkCommons::get_player_avatar_channel_for_quality(3, has2), BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL);
}

// ─────────────────────────────────────────────────────────────────────────────
//  Condition matrix
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial(reduction_statics)]
fn large_player_id_keyframe_and_delta_carry_face_data() {
    // Player ids > 255 use the ushort-id channels (41-48) and the largeId delta-header bit.
    let mut rng = TestRng::new(1012);
    let kf_payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    const BIG_ID: u16 = 300;

    let (ingested, _) = ingest_keyframe(&write_uplink_keyframe(1, &kf_payload, Some(make_additional())), true);
    let state = build_state(&ingested, BIG_ID, 1);
    assert!(!state.small_id());

    BasisServerReductionSystemEvents::test_only_pre_serialize_frame(&state, 1, true);
    assert!(frame_has_additional(&state, 3));
    let channel = BasisNetworkCommons::get_player_avatar_large_channel_for_quality(3, true);
    assert!(BasisNetworkCommons::is_large_player_id_channel(channel));

    let kf = parse_fanout_keyframe(&frame_keyframe(&state, 3), channel);
    assert_eq!(kf.player_id_message.player_id, BIG_ID);
    assert_face_survived(&kf.avatar_serialization);

    let (receiver_baseline, keyframe_sequence) = {
        let sender = state.sender.lock();
        (sender.keyframe_payload[3][..sender.keyframe_payload_length[3]].to_vec(), sender.keyframe_sequence)
    };

    let mut cur = kf_payload.clone();
    S::flip_bone(&mut cur, BitQuality::High, 5);
    let (delta_ingested, _) = ingest_delta(&write_uplink_delta(2, 1, &kf_payload, &cur, Some(make_additional())), &kf_payload, 1);
    replace_high(&state, &delta_ingested, 2);
    BasisServerReductionSystemEvents::test_only_pre_serialize_frame(&state, 2, false);
    assert!(!state.frame.load().current_is_keyframe);
    let wire = frame_delta(&state, 3);
    assert!(!wire.is_empty());

    // Header must carry the largeId bit and the parse must recover the ushort id.
    assert!(BasisNetworkCommons::delta_header_large_id(wire[0]));
    let (dssm, _) = parse_fanout_delta(&wire, &receiver_baseline, keyframe_sequence);
    assert_eq!(dssm.player_id_message.player_id, BIG_ID);
    assert_face_survived(&dssm.avatar_serialization);
}

#[test]
#[serial(reduction_statics)]
fn idle_avatar_face_only_change_mask_only_delta_carries_face_data() {
    // "Standing still while the face moves": pose is byte-identical to the baseline, so the uplink
    // and the fan-out deltas are mask-only — the additional tail must still ride along.
    let mut rng = TestRng::new(1013);
    let kf_payload = S::make_realistic_payload(BitQuality::High, &mut rng);

    let (kf_ingested, _) = ingest_keyframe(&write_uplink_keyframe(1, &kf_payload, Some(make_additional())), true);
    let state = build_state(&kf_ingested, 42, 1);
    BasisServerReductionSystemEvents::test_only_pre_serialize_frame(&state, 1, true);
    let (receiver_baseline, keyframe_sequence) = {
        let sender = state.sender.lock();
        (sender.keyframe_payload[3][..sender.keyframe_payload_length[3]].to_vec(), sender.keyframe_sequence)
    };

    // Uplink: identical pose + fresh face bytes → mask-only delta body.
    let mut scratch = vec![0u8; BasisAvatarDeltaCompression::max_delta_size(BitQuality::High)];
    let body_len = BasisAvatarDeltaCompression::build_delta(&kf_payload, &kf_payload.clone(), BitQuality::High, &mut scratch, 0).unwrap();
    assert_eq!(body_len, BasisAvatarDeltaCompression::DIRTY_MASK_BYTES);

    let mut lasm = LocalAvatarSyncMessage { array: Some(kf_payload.clone()), additional_avatar_datas: Some(make_additional()), linked_avatar_index: LINKED_INDEX, ..Default::default() };
    let mut w = NetDataWriter::new();
    w.put_byte(BasisNetworkCommons::build_delta_header(3, true, false));
    w.put_byte(2);
    w.put_byte(1);
    w.put_bytes(&scratch[..body_len]);
    lasm.serialize_additional_only(&mut w).unwrap();

    let (delta_ingested, _) = ingest_delta(&w, &kf_payload, 1);
    assert_eq!(delta_ingested.array.as_deref(), Some(&kf_payload[..]));
    assert_face_survived(&delta_ingested);

    // Fan-out: the delta generation for an unchanged pose must still carry the face bytes.
    replace_high(&state, &delta_ingested, 2);
    BasisServerReductionSystemEvents::test_only_pre_serialize_frame(&state, 2, false);
    assert!(!state.frame.load().current_is_keyframe);
    let (ssm, _) = parse_fanout_delta(&frame_delta(&state, 3), &receiver_baseline, keyframe_sequence);
    assert_eq!(ssm.avatar_serialization.array.as_deref(), Some(&kf_payload[..]));
    assert_face_survived(&ssm.avatar_serialization);
}

#[test]
#[serial(reduction_statics)]
fn delta_promotion_fully_changed_pose_falls_back_to_keyframe_with_face_data() {
    // A delta that would be larger than a keyframe is promoted — face data must follow.
    let mut rng = TestRng::new(1014);
    let kf_payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    let (kf_ingested, _) = ingest_keyframe(&write_uplink_keyframe(1, &kf_payload, None), false);
    let state = build_state(&kf_ingested, 42, 1);
    BasisServerReductionSystemEvents::test_only_pre_serialize_frame(&state, 1, true);

    let wholly_new = S::make_realistic_payload(BitQuality::High, &mut rng); // every field differs
    let (next, _) = ingest_keyframe(&write_uplink_keyframe(2, &wholly_new, Some(make_additional())), true);
    replace_high(&state, &next, 2);
    BasisServerReductionSystemEvents::test_only_pre_serialize_frame(&state, 2, false);

    assert!(state.frame.load().current_is_keyframe, "an everything-changed frame must promote to keyframe");
    assert!(frame_has_additional(&state, 3));
    let channel = BasisNetworkCommons::get_player_avatar_channel_for_quality(3, true);
    let ssm = parse_fanout_keyframe(&frame_keyframe(&state, 3), channel);
    assert_eq!(ssm.avatar_serialization.array.as_deref(), Some(&wholly_new[..]));
    assert_face_survived(&ssm.avatar_serialization);
}

#[test]
#[serial(reduction_statics)]
fn strip_on_real_pre_serialize_frame_all_tiers_channels_and_sections_agree() {
    let _g = StripGuard::new(true);
    let mut rng = TestRng::new(1015);
    let payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    let (ingested, _) = ingest_keyframe(&write_uplink_keyframe(1, &payload, Some(make_additional())), true);
    let state = build_state(&ingested, 42, 1);
    BasisServerReductionSystemEvents::test_only_pre_serialize_frame(&state, 1, true);

    for qi in 0..4 {
        let wire = frame_keyframe(&state, qi);
        assert!(!wire.is_empty(), "tier {qi} not serialized");
        let expect_face = qi >= 2; // High + Medium keep it; Low + VeryLow strip it
        assert_eq!(frame_has_additional(&state, qi), expect_face);

        let channel = BasisNetworkCommons::get_player_avatar_channel_for_quality(qi as i32, frame_has_additional(&state, qi));
        // parse_fanout_keyframe asserts the frame is consumed EXACTLY.
        let ssm = parse_fanout_keyframe(&wire, channel);
        if expect_face {
            assert_face_survived(&ssm.avatar_serialization);
        } else {
            assert_eq!(ssm.avatar_serialization.additional_avatar_data_size, 0);
        }
    }
}

#[test]
#[serial(reduction_statics)]
fn medium_tier_delta_carries_face_data_low_tier_delta_strips_it() {
    let _g = StripGuard::new(true);
    let mut rng = TestRng::new(1016);
    let kf_payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    let (kf_ingested, _) = ingest_keyframe(&write_uplink_keyframe(1, &kf_payload, Some(make_additional())), true);
    let state = build_state(&kf_ingested, 42, 1);
    BasisServerReductionSystemEvents::test_only_pre_serialize_frame(&state, 1, true);
    let (medium_baseline, low_baseline, keyframe_sequence) = {
        let sender = state.sender.lock();
        (sender.keyframe_payload[2][..sender.keyframe_payload_length[2]].to_vec(), sender.keyframe_payload[1][..sender.keyframe_payload_length[1]].to_vec(), sender.keyframe_sequence)
    };

    let mut cur = kf_payload.clone();
    S::flip_bone(&mut cur, BitQuality::High, 8);
    let (delta_ingested, _) = ingest_delta(&write_uplink_delta(2, 1, &kf_payload, &cur, Some(make_additional())), &kf_payload, 1);
    replace_high(&state, &delta_ingested, 2);
    BasisServerReductionSystemEvents::test_only_pre_serialize_frame(&state, 2, false);
    assert!(!state.frame.load().current_is_keyframe);

    // Medium (qi=2): repacked pose delta + face data.
    let med_wire = frame_delta(&state, 2);
    assert!(!med_wire.is_empty(), "medium delta not serialized");
    assert!(BasisNetworkCommons::delta_header_has_additional_data(med_wire[0]));
    let (med, mq) = parse_fanout_delta(&med_wire, &medium_baseline, keyframe_sequence);
    assert_eq!(mq, 2);
    assert_face_survived(&med.avatar_serialization);

    // Low (qi=1): stripped — header bit clear, no trailing section.
    let low_wire = frame_delta(&state, 1);
    assert!(!low_wire.is_empty(), "low delta not serialized");
    assert!(!BasisNetworkCommons::delta_header_has_additional_data(low_wire[0]));
    let (low, lq) = parse_fanout_delta(&low_wire, &low_baseline, keyframe_sequence);
    assert_eq!(lq, 1);
    assert_eq!(low.avatar_serialization.additional_avatar_data_size, 0);
}

#[test]
#[serial(reduction_statics)]
fn multi_entry_edge_payloads_keep_stream_alignment() {
    // Several entries with edge payloads: 0-length, 255-byte max, and an absent array (wire form
    // is a size-0 entry). Entries after each edge case must still parse from the correct offset.
    let mut rng = TestRng::new(1017);
    let payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    let max = rng.bytes(255);

    let entries = vec![
        entry(1, &FACE_BYTES),
        AdditionalAvatarData { message_index: 2, payload_size: 0, array: None }, // size-0 wire form
        entry(3, &[]),                                                             // 0-length but indexed
        entry(4, &max),                                                            // max payload
        entry(5, &[7]),
    ];

    let (ingested, _) = ingest_keyframe(&write_uplink_keyframe(1, &payload, Some(entries.clone())), true);
    assert_eq!(ingested.additional_avatar_data_size as usize, entries.len());

    let datas = ingested.additional_avatar_datas.as_ref().unwrap();
    assert_eq!(datas[0].message_index, 1);
    assert_eq!(datas[0].array.as_deref(), Some(&FACE_BYTES[..]));
    assert_eq!(datas[1].message_index, 2);
    assert_eq!(datas[1].array.as_ref().map(|a| a.len()).unwrap_or(0), 0);
    assert_eq!(datas[2].message_index, 3);
    assert_eq!(datas[2].array.as_ref().map(|a| a.len()).unwrap_or(0), 0);
    assert_eq!(datas[3].message_index, 4);
    assert_eq!(datas[3].array.as_deref(), Some(&max[..]));
    assert_eq!(datas[4].message_index, 5);
    assert_eq!(datas[4].array.as_deref(), Some(&[7u8][..]));

    // And through the server fan-out.
    let state = build_state(&ingested, 42, 1);
    let (wire, has_additional) = pre_serialize_keyframe(&state, 3, 42);
    let channel = BasisNetworkCommons::get_player_avatar_channel_for_quality(3, has_additional);
    let ssm = parse_fanout_keyframe(&wire, channel);
    assert_eq!(ssm.avatar_serialization.additional_avatar_data_size as usize, entries.len());
    let out = ssm.avatar_serialization.additional_avatar_datas.as_ref().unwrap();
    assert_eq!(out[0].array.as_deref(), Some(&FACE_BYTES[..]));
    assert_eq!(out[3].array.as_deref(), Some(&max[..]));
    assert_eq!(out[4].array.as_deref(), Some(&[7u8][..]));
}

#[test]
#[serial(reduction_statics)]
fn max_entry_count_255_round_trips() {
    let mut rng = TestRng::new(1018);
    let payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    let entries: Vec<AdditionalAvatarData> = (0..255u8).map(|i| entry(i, &[i])).collect();

    let (ingested, _) = ingest_keyframe(&write_uplink_keyframe(1, &payload, Some(entries)), true);
    assert_eq!(ingested.additional_avatar_data_size, 255);

    let state = build_state(&ingested, 42, 1);
    let (wire, has_additional) = pre_serialize_keyframe(&state, 3, 42);
    let channel = BasisNetworkCommons::get_player_avatar_channel_for_quality(3, has_additional);
    let ssm = parse_fanout_keyframe(&wire, channel);
    assert_eq!(ssm.avatar_serialization.additional_avatar_data_size, 255);
    let out = ssm.avatar_serialization.additional_avatar_datas.as_ref().unwrap();
    for i in 0..255u8 {
        assert_eq!(out[i as usize].message_index, i);
        assert_eq!(out[i as usize].array.as_deref(), Some(&[i][..]));
    }
}

#[test]
#[serial(reduction_statics)]
fn bundle_path_keyframe_and_delta_with_face_data_round_trip_losslessly() {
    // Channel-52 bundles: the server packs [chan:1][n:1][len:2-LE]xn[bodies] groups and LZ4s the
    // block; the client inflates, flattens via BasisAvatarBundleCodec and re-dispatches by inner
    // channel. Face data and the per-receiver interval patch must survive.
    let mut rng = TestRng::new(1019);
    let kf_payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    let (kf_ingested, _) = ingest_keyframe(&write_uplink_keyframe(1, &kf_payload, Some(make_additional())), true);
    let state = build_state(&kf_ingested, 42, 1);
    BasisServerReductionSystemEvents::test_only_pre_serialize_frame(&state, 1, true);
    let (receiver_baseline, keyframe_sequence) = {
        let sender = state.sender.lock();
        (sender.keyframe_payload[3][..sender.keyframe_payload_length[3]].to_vec(), sender.keyframe_sequence)
    };

    let mut cur = kf_payload.clone();
    S::flip_bone(&mut cur, BitQuality::High, 14);
    let (delta_ingested, _) = ingest_delta(&write_uplink_delta(2, 1, &kf_payload, &cur, Some(make_additional())), &kf_payload, 1);
    // Snapshot the keyframe wire BEFORE the delta generation replaces the state's High message.
    let kf_wire: Arc<[u8]> = state.frame.load().serialized_keyframe[3].clone().unwrap();
    replace_high(&state, &delta_ingested, 2);
    BasisServerReductionSystemEvents::test_only_pre_serialize_frame(&state, 2, false);
    assert!(!state.frame.load().current_is_keyframe);
    let delta_wire: Arc<[u8]> = state.frame.load().serialized_delta[3].clone().unwrap();

    // Pending buffer exactly as the send loop stages it: one keyframe + one delta, each with a
    // per-receiver interval byte to patch.
    let mut recv = ReceiverData {
        pending_sends: vec![
            PendingAvatarSend { length: kf_wire.len(), source: kf_wire.clone(), channel: BasisNetworkCommons::get_player_avatar_channel_for_quality(3, true), interval: 37, interval_offset: 1 },
            PendingAvatarSend { length: delta_wire.len(), source: delta_wire, channel: BasisNetworkCommons::DELTA_AVATAR_CHANNEL, interval: 53, interval_offset: 2 },
        ],
        ..Default::default()
    };
    let raw_len = BasisServerReductionSystemEvents::test_only_build_raw_for_range(&mut recv, 0, 2);
    assert!(raw_len > 0);

    // Emit exactly like the deflate step: [count:1][rawLen:2-LE][LZ4 block].
    let raw = recv.bundle_raw_scratch[..raw_len].to_vec();
    let mut compressed = vec![0u8; 3 + lz4_flex::block::get_maximum_output_size(raw_len)];
    let compressed_len = lz4_flex::block::compress_into(&raw, &mut compressed[3..]).unwrap();
    assert!(compressed_len > 0);
    compressed[0] = 2;
    compressed[1] = (raw_len & 0xFF) as u8;
    compressed[2] = ((raw_len >> 8) & 0xFF) as u8;

    // Decode exactly like the client's compressed-bundle handler.
    let mut reader = NetDataReader::from_slice(&compressed[..3 + compressed_len]);
    assert!(reader.try_get_byte().is_some());
    let parsed_raw_len = reader.try_get_ushort().unwrap() as usize;
    assert_eq!(parsed_raw_len, raw_len);
    let mut grouped = vec![0u8; parsed_raw_len];
    let decoded = lz4_flex::block::decompress_into(&reader.raw_data()[reader.position()..reader.position() + reader.available_bytes()], &mut grouped).unwrap();
    assert_eq!(decoded, parsed_raw_len);

    // Ungroup + un-transpose exactly like the client does before dispatching.
    let mut scratch = vec![0u8; BasisAvatarBundleCodec::max_flat_size(decoded)];
    let flat_len = BasisAvatarBundleCodec::try_flatten(&grouped[..decoded], &mut scratch).unwrap();

    let mut offset = 0;
    let mut inner_seen = 0;
    while offset + 3 <= flat_len {
        let inner_channel = scratch[offset];
        let msg_len = u16::from_le_bytes([scratch[offset + 1], scratch[offset + 2]]) as usize;
        offset += 3;
        assert!(msg_len > 0 && offset + msg_len <= flat_len);
        let inner = scratch[offset..offset + msg_len].to_vec();

        if inner_channel == BasisNetworkCommons::DELTA_AVATAR_CHANNEL {
            // Interval byte was patched per receiver inside the bundle copy.
            assert_eq!(inner[2], 53);
            let (dssm, _) = parse_fanout_delta(&inner, &receiver_baseline, keyframe_sequence);
            assert_eq!(dssm.avatar_serialization.array.as_deref(), Some(&cur[..]));
            assert_face_survived(&dssm.avatar_serialization);
        } else {
            assert_eq!(inner_channel, BasisNetworkCommons::get_player_avatar_channel_for_quality(3, true));
            assert_eq!(inner[1], 37);
            let kssm = parse_fanout_keyframe(&inner, inner_channel);
            assert_eq!(kssm.avatar_serialization.array.as_deref(), Some(&kf_payload[..]));
            assert_face_survived(&kssm.avatar_serialization);
        }
        offset += msg_len;
        inner_seen += 1;
    }
    assert_eq!(inner_seen, 2);
}

#[test]
#[serial(reduction_statics)]
fn serialized_wire_bytes_are_immune_to_source_mutation_after_pre_serialize() {
    // The server pre-serializes inside process_message while the inbound message is still
    // pool-owned; the wire buffers must be byte snapshots, not views over pooled data.
    let mut rng = TestRng::new(1020);
    let payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    let (mut ingested, _) = ingest_keyframe(&write_uplink_keyframe(1, &payload, Some(make_additional())), true);
    let state = build_state(&ingested, 42, 1);
    let (before, _) = pre_serialize_keyframe(&state, 3, 42);

    // Simulate the pool overwriting the inbound entries after process_message returned.
    if let Some(datas) = ingested.additional_avatar_datas.as_mut() {
        for d in datas.iter_mut() {
            if let Some(a) = d.array.as_mut() {
                a.fill(0);
            }
        }
    }

    let after = state.sender.lock().serialized_keyframe[3].clone();
    assert_eq!(before, after);
}

#[test]
fn truncated_additional_section_fails_safely() {
    // A frame cut mid-additional-section (worst-case corruption reaching the parser) must fail
    // without panicking and without inventing data.
    let mut rng = TestRng::new(1021);
    let payload = S::make_realistic_payload(BitQuality::High, &mut rng);
    let full = write_uplink_keyframe(1, &payload, Some(make_additional()));
    let wire = full.copy_data();

    for cut in payload.len() + 2..wire.len() {
        let mut reader = NetDataReader::from_slice(&wire[..cut]);
        assert!(reader.try_get_byte().is_some()); // seq
        let mut msg = LocalAvatarSyncMessage::default();
        let _ = msg.deserialize_for_channel(&mut reader, 3, true);
        if msg.additional_avatar_data_size > 0
            && let Some(datas) = msg.additional_avatar_datas.as_ref()
            && let Some(entry) = datas.first()
            && let Some(array) = entry.array.as_ref()
            && array.len() == FACE_BYTES.len()
            && entry.payload_size as usize == FACE_BYTES.len()
        {
            // Either the entry failed to materialize or it holds exactly the original bytes.
            assert_eq!(array.as_slice(), &FACE_BYTES[..]);
        }
    }
}
