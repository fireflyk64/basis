//! Wire contracts of the avatar, scene and audio messages: field-exact round trips, the
//! null/empty and oversized collapses, byte-identical double round trips, and what a truncated
//! or overclaimed buffer leaves behind. Where the C# threw `ArgumentException` the Rust returns
//! `Err`; where it "did not throw" the message state is pinned instead.

use basis_network_core::SerializableBasis::{
    AdditionalAvatarData, AudioSegmentDataMessage, AvatarDataMessage, AvatarLoadDataMessage, BasisAvatarCloneRequest, BasisAvatarCloneResponse, BasisCompactId, BasisPlatformCodec, ClientAvatarChangeMessage,
    ClientBodyFitMessage, ClientMetaDataMessage, LocalAvatarSyncMessage, PlayerIdMessage, ReadyMessage, RemoteAvatarDataMessage, RemoteSceneDataMessage, SceneDataMessage, ServerAudioSegmentMessage, ServerAvatarChangeMessage,
    ServerAvatarDataMessage, ServerBodyFitMessage, ServerReadyBatchMessage, ServerReadyMessage, ServerSceneDataMessage, ServerSideSyncPlayerMessage, VoiceReceiversMessage,
};
use basis_network_core::compression::{BasisAvatarBitPacking, BitQuality};
use basis_network_core::{NetDataReader, NetDataWriter};
use basis_server_tests::support::delta_test_support::TestRng;

fn reader(w: &NetDataWriter) -> NetDataReader {
    NetDataReader::new(w.copy_data())
}

fn empty() -> NetDataReader {
    NetDataReader::from_slice(&[])
}

fn random_bytes(seed: u64, count: usize) -> Vec<u8> {
    TestRng::new(seed).bytes(count)
}

fn payload_size(q: BitQuality) -> usize {
    BasisAvatarBitPacking::convert_to_size(q)
}

// 16 bits over a range of 1.0 => 1.5e-5 step, so a round-tripped scale lands within half of that.
const FIT_TOL: f32 = 1e-4;

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= FIT_TOL
}

// ── AdditionalAvatarData: [PayloadSize:1][messageIndex:1][payload], collapsing to a 2-byte header
// for a missing or oversized (>255) array ──

#[test]
fn additional_avatar_data_round_trip_preserves_all_fields() {
    let payload = random_bytes(42, 32);
    let mut msg = AdditionalAvatarData { message_index: 7, array: Some(payload.clone()), ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 2 + 32);

    let mut result = AdditionalAvatarData::default();
    let mut r = reader(&w);
    result.deserialize(&mut r).expect("deserialize");
    assert_eq!(result.payload_size, 32);
    assert_eq!(result.message_index, 7);
    assert_eq!(result.array, Some(payload));
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn additional_avatar_data_max_payload_255_round_trips() {
    let payload = random_bytes(43, 255);
    let mut msg = AdditionalAvatarData { message_index: 255, array: Some(payload.clone()), ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 257);
    let mut result = AdditionalAvatarData::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.payload_size, 255);
    assert_eq!(result.message_index, 255);
    assert_eq!(result.array, Some(payload));
}

#[test]
fn additional_avatar_data_no_array_writes_full_two_byte_header() {
    // Every entry writes [size:1][messageIndex:1] even when empty — a bare size-0 byte was
    // ambiguous against the next entry's header and desynced the whole additional section.
    let mut msg = AdditionalAvatarData { message_index: 9, array: None, ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.as_read_only_span(), &[0, 9]);

    let mut result = AdditionalAvatarData::default();
    let mut r = reader(&w);
    result.deserialize(&mut r).expect("deserialize");
    assert_eq!(result.payload_size, 0);
    assert_eq!(result.message_index, 9);
    assert!(result.array.is_none());
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn additional_avatar_data_array_over_255_rejected_as_zero_payload() {
    let mut msg = AdditionalAvatarData { message_index: 3, array: Some(vec![0u8; 256]), ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.as_read_only_span(), &[0, 3]);
    let mut result = AdditionalAvatarData::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.payload_size, 0);
    assert_eq!(result.message_index, 3);
    assert!(result.array.is_none());
}

#[test]
fn additional_avatar_data_empty_reader_zero_fallback() {
    let mut result = AdditionalAvatarData::default();
    let _ = result.deserialize(&mut empty());
    assert_eq!(result.payload_size, 0);
    assert!(result.array.is_none());
}

#[test]
fn additional_avatar_data_missing_message_index_no_panic() {
    let mut result = AdditionalAvatarData::default();
    let _ = result.deserialize(&mut NetDataReader::from_slice(&[5]));
    assert_eq!(result.payload_size, 5);
    assert!(result.array.is_none());
}

#[test]
fn additional_avatar_data_truncated_payload_array_stays_none() {
    let mut w = NetDataWriter::new();
    w.put_byte(10);
    w.put_byte(4);
    w.put_bytes(&[1, 2, 3]);
    let mut result = AdditionalAvatarData::default();
    let _ = result.deserialize(&mut reader(&w));
    assert_eq!(result.payload_size, 10);
    assert_eq!(result.message_index, 4);
    assert!(result.array.is_none());
}

#[test]
fn additional_avatar_data_double_round_trip_is_byte_identical() {
    let mut msg = AdditionalAvatarData { message_index: 12, array: Some(random_bytes(44, 17)), ..Default::default() };
    let mut w1 = NetDataWriter::new();
    msg.serialize(&mut w1).expect("serialize");
    let mut mid = AdditionalAvatarData::default();
    mid.deserialize(&mut reader(&w1)).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize(&mut w2).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

// ── AudioSegmentDataMessage ([seq:1][silence:1][opus bytes = remainder]) and its wrapper ──

#[test]
fn audio_segment_data_message_round_trip_preserves_all_fields() {
    let audio = random_bytes(7, 60);
    let mut msg = AudioSegmentDataMessage { sequence_number: 200, total_played_in_silence: 3, buffer: audio.clone(), total_length: audio.len(), length_used: audio.len() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 62);

    let mut result = AudioSegmentDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.sequence_number, 200);
    assert_eq!(result.total_played_in_silence, 3);
    assert_eq!(result.buffer, audio);
    assert_eq!(result.total_length, 60);
    assert_eq!(result.length_used, 60);
}

#[test]
fn audio_segment_data_message_zero_length_segment_round_trips_to_empty_buffer() {
    let mut msg = AudioSegmentDataMessage { sequence_number: 5, total_played_in_silence: 9, buffer: Vec::new(), total_length: 0, length_used: 0 };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 2);
    let mut result = AudioSegmentDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.sequence_number, 5);
    assert_eq!(result.total_played_in_silence, 9);
    assert!(result.buffer.is_empty());
    assert_eq!(result.total_length, 0);
    assert_eq!(result.length_used, 0);
}

#[test]
fn audio_segment_data_message_length_used_limits_written_bytes() {
    let pooled = vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
    let mut msg = AudioSegmentDataMessage { sequence_number: 1, total_played_in_silence: 0, buffer: pooled, total_length: 10, length_used: 4 };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 6);
    let mut result = AudioSegmentDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.buffer, vec![1, 2, 3, 4]);
    assert_eq!(result.total_length, 4);
    assert_eq!(result.length_used, 4);
}

#[test]
fn audio_segment_data_message_double_round_trip_is_byte_identical() {
    let mut msg = AudioSegmentDataMessage { sequence_number: 90, total_played_in_silence: 2, buffer: random_bytes(8, 16), total_length: 16, length_used: 16 };
    let mut w1 = NetDataWriter::new();
    msg.serialize(&mut w1).expect("serialize");
    let mut mid = AudioSegmentDataMessage::default();
    mid.deserialize(&mut reader(&w1)).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize(&mut w2).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

#[test]
fn audio_segment_data_message_truncated_header_is_an_error() {
    let mut result = AudioSegmentDataMessage::default();
    assert!(result.deserialize(&mut NetDataReader::from_slice(&[7])).is_err());
    assert!(result.deserialize(&mut empty()).is_err());
}

fn segment(seed: u64, count: usize, sequence: u8, silence: u8) -> (Vec<u8>, AudioSegmentDataMessage) {
    let audio = random_bytes(seed, count);
    (audio.clone(), AudioSegmentDataMessage { sequence_number: sequence, total_played_in_silence: silence, buffer: audio, total_length: count, length_used: count })
}

#[test]
fn server_audio_segment_message_round_trip_ushort_id_and_audio() {
    let (audio, data) = segment(21, 48, 9, 1);
    let mut msg = ServerAudioSegmentMessage { player_id_message: PlayerIdMessage { player_id: u16::MAX }, audio_segment_data: data };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 2 + 2 + 48);
    let mut result = ServerAudioSegmentMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.player_id_message.player_id, u16::MAX);
    assert_eq!(result.audio_segment_data.sequence_number, 9);
    assert_eq!(result.audio_segment_data.total_played_in_silence, 1);
    assert_eq!(result.audio_segment_data.buffer, audio);
    assert_eq!(result.audio_segment_data.length_used, 48);
}

#[test]
fn server_audio_segment_message_small_id_variant_round_trips() {
    let (audio, data) = segment(22, 20, 3, 0);
    let mut msg = ServerAudioSegmentMessage { player_id_message: PlayerIdMessage { player_id: 200 }, audio_segment_data: data };
    let mut w = NetDataWriter::new();
    msg.serialize_sized(&mut w, false).expect("serialize");
    assert_eq!(w.length(), 1 + 2 + 20);
    let mut result = ServerAudioSegmentMessage::default();
    result.deserialize_sized(&mut reader(&w), false).expect("deserialize");
    assert_eq!(result.player_id_message.player_id, 200);
    assert_eq!(result.audio_segment_data.buffer, audio);
}

#[test]
fn server_audio_segment_message_large_id_variant_round_trips() {
    let (audio, data) = segment(23, 10, 8, 4);
    let mut msg = ServerAudioSegmentMessage { player_id_message: PlayerIdMessage { player_id: 40000 }, audio_segment_data: data };
    let mut w = NetDataWriter::new();
    msg.serialize_sized(&mut w, true).expect("serialize");
    assert_eq!(w.length(), 2 + 2 + 10);
    let mut result = ServerAudioSegmentMessage::default();
    result.deserialize_sized(&mut reader(&w), true).expect("deserialize");
    assert_eq!(result.player_id_message.player_id, 40000);
    assert_eq!(result.audio_segment_data.buffer, audio);
}

#[test]
fn server_audio_segment_message_zero_length_audio_small_id_round_trips() {
    let mut msg = ServerAudioSegmentMessage { player_id_message: PlayerIdMessage { player_id: 1 }, audio_segment_data: AudioSegmentDataMessage { sequence_number: 77, total_played_in_silence: 255, ..Default::default() } };
    let mut w = NetDataWriter::new();
    msg.serialize_sized(&mut w, false).expect("serialize");
    assert_eq!(w.length(), 3);
    let mut result = ServerAudioSegmentMessage::default();
    result.deserialize_sized(&mut reader(&w), false).expect("deserialize");
    assert_eq!(result.player_id_message.player_id, 1);
    assert_eq!(result.audio_segment_data.sequence_number, 77);
    assert_eq!(result.audio_segment_data.total_played_in_silence, 255);
    assert!(result.audio_segment_data.buffer.is_empty());
}

// ── AvatarDataMessage: [playerID:2][AvatarLinkIndex:1][messageIndex:1][recipientsSize:2][recipients...][payload] ──

#[test]
fn avatar_data_message_round_trip_preserves_all_fields() {
    let payload = random_bytes(11, 20);
    let recipients = vec![1u16, 500, u16::MAX];
    let mut msg = AvatarDataMessage { player_id_message: PlayerIdMessage { player_id: 4242 }, avatar_link_index: 5, message_index: 77, recipients: Some(recipients.clone()), payload: Some(payload.clone()), ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 2 + 1 + 1 + 2 + 6 + 20);

    let mut result = AvatarDataMessage::default();
    let mut r = reader(&w);
    result.deserialize(&mut r).expect("deserialize");
    assert_eq!(result.player_id_message.player_id, 4242);
    assert_eq!(result.avatar_link_index, 5);
    assert_eq!(result.message_index, 77);
    assert_eq!(result.recipients_size, 3);
    assert_eq!(result.recipients, Some(recipients));
    assert_eq!(result.payload, Some(payload));
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn avatar_data_message_ushort_max_player_id_round_trips() {
    let mut msg = AvatarDataMessage { player_id_message: PlayerIdMessage { player_id: u16::MAX }, avatar_link_index: 255, message_index: 255, payload: Some(vec![42]), ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    let mut result = AvatarDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.player_id_message.player_id, u16::MAX);
    assert_eq!(result.avatar_link_index, 255);
    assert_eq!(result.message_index, 255);
    assert_eq!(result.payload, Some(vec![42]));
}

#[test]
fn avatar_data_message_no_recipients_deserializes_to_empty_list_payload_intact() {
    let payload = random_bytes(12, 8);
    let mut msg = AvatarDataMessage { player_id_message: PlayerIdMessage { player_id: 3 }, avatar_link_index: 1, message_index: 2, recipients: None, payload: Some(payload.clone()), ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    let mut result = AvatarDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.recipients_size, 0);
    assert_eq!(result.recipients.as_deref().map(<[u16]>::len), Some(0));
    assert_eq!(result.payload, Some(payload));
}

#[test]
fn avatar_data_message_recipients_only_no_payload_deserializes_to_none_payload() {
    // recipients_size == available / 2 exactly: the size guard boundary must pass.
    let recipients = vec![10u16, 20, 30];
    let mut msg = AvatarDataMessage { player_id_message: PlayerIdMessage { player_id: 6 }, avatar_link_index: 0, message_index: 1, recipients: Some(recipients.clone()), payload: None, ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    let mut result = AvatarDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.recipients, Some(recipients));
    assert!(result.payload.is_none());
}

#[test]
fn avatar_data_message_oversized_recipients_size_is_an_error() {
    let mut w = NetDataWriter::new();
    w.put_ushort(1); // playerID
    w.put_byte(0); // AvatarLinkIndex
    w.put_byte(0); // messageIndex
    w.put_ushort(4); // recipientsSize claims 4 entries
    w.put_byte(9); // but only 1 byte remains
    let mut msg = AvatarDataMessage::default();
    assert!(msg.deserialize(&mut reader(&w)).is_err());
}

#[test]
fn avatar_data_message_truncated_after_player_id_is_an_error() {
    let mut w = NetDataWriter::new();
    w.put_ushort(9);
    let mut msg = AvatarDataMessage::default();
    assert!(msg.deserialize(&mut reader(&w)).is_err());
}

#[test]
fn avatar_data_message_missing_recipients_size_leaves_nothing_behind() {
    let mut w = NetDataWriter::new();
    w.put_ushort(9);
    w.put_byte(4);
    w.put_byte(8);
    let mut msg = AvatarDataMessage::default();
    let _ = msg.deserialize(&mut reader(&w));
    assert_eq!(msg.avatar_link_index, 4);
    assert_eq!(msg.message_index, 8);
    assert!(msg.recipients.is_none());
    assert!(msg.payload.is_none());
}

#[test]
fn avatar_data_message_double_round_trip_is_byte_identical() {
    let mut msg = AvatarDataMessage { player_id_message: PlayerIdMessage { player_id: 100 }, avatar_link_index: 2, message_index: 3, recipients: Some(vec![7, 8]), payload: Some(random_bytes(13, 11)), ..Default::default() };
    let mut w1 = NetDataWriter::new();
    msg.serialize(&mut w1).expect("serialize");
    let mut mid = AvatarDataMessage::default();
    mid.deserialize(&mut reader(&w1)).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize(&mut w2).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

// ── RemoteAvatarDataMessage: [playerID:2][AvatarLinkIndex:1][messageIndex:1][payload] ──

#[test]
fn remote_avatar_data_message_round_trip_preserves_all_fields() {
    let payload = random_bytes(14, 25);
    let mut msg = RemoteAvatarDataMessage { player_id_message: PlayerIdMessage { player_id: u16::MAX }, avatar_link_index: 9, message_index: 44, payload: Some(payload.clone()) };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 2 + 1 + 1 + 25);
    let mut result = RemoteAvatarDataMessage::default();
    let mut r = reader(&w);
    result.deserialize(&mut r).expect("deserialize");
    assert_eq!(result.player_id_message.player_id, u16::MAX);
    assert_eq!(result.avatar_link_index, 9);
    assert_eq!(result.message_index, 44);
    assert_eq!(result.payload, Some(payload));
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn remote_avatar_data_message_no_payload_deserializes_to_none() {
    let mut msg = RemoteAvatarDataMessage { player_id_message: PlayerIdMessage { player_id: 5 }, avatar_link_index: 1, message_index: 2, payload: None };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 4);
    let mut result = RemoteAvatarDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert!(result.payload.is_none());
}

#[test]
fn remote_avatar_data_message_truncated_header_is_an_error() {
    let mut w = NetDataWriter::new();
    w.put_ushort(5);
    w.put_byte(1); // AvatarLinkIndex present, messageIndex missing
    let mut msg = RemoteAvatarDataMessage::default();
    assert!(msg.deserialize(&mut reader(&w)).is_err());
}

#[test]
fn remote_avatar_data_message_double_round_trip_is_byte_identical() {
    let mut msg = RemoteAvatarDataMessage { player_id_message: PlayerIdMessage { player_id: 321 }, avatar_link_index: 7, message_index: 6, payload: Some(random_bytes(15, 9)) };
    let mut w1 = NetDataWriter::new();
    msg.serialize(&mut w1).expect("serialize");
    let mut mid = RemoteAvatarDataMessage::default();
    mid.deserialize(&mut reader(&w1)).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize(&mut w2).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

// ── SceneDataMessage: [messageIndex:2][recipientsSize:2][recipients...][payload] ──

#[test]
fn scene_data_message_round_trip_preserves_all_fields() {
    let payload = random_bytes(16, 16);
    let recipients = vec![2u16, 40000, u16::MAX];
    let mut msg = SceneDataMessage { message_index: u16::MAX, recipients: Some(recipients.clone()), payload: Some(payload.clone()), ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 2 + 2 + 6 + 16);
    let mut result = SceneDataMessage::default();
    let mut r = reader(&w);
    result.deserialize(&mut r).expect("deserialize");
    assert_eq!(result.message_index, u16::MAX);
    assert_eq!(result.recipients_size, 3);
    assert_eq!(result.recipients, Some(recipients));
    assert_eq!(result.payload, Some(payload));
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn scene_data_message_no_recipients_and_payload_round_trips() {
    let mut msg = SceneDataMessage { message_index: 12, recipients: None, payload: None, ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 4);
    let mut result = SceneDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.message_index, 12);
    assert_eq!(result.recipients_size, 0);
    assert_eq!(result.recipients.as_deref().map(<[u16]>::len), Some(0));
    assert!(result.payload.is_none());
}

#[test]
fn scene_data_message_oversized_recipients_size_is_an_error() {
    let mut w = NetDataWriter::new();
    w.put_ushort(1); // messageIndex
    w.put_ushort(1000); // recipientsSize claims 1000 entries
    w.put_byte(1); // but only 1 byte remains
    let mut msg = SceneDataMessage::default();
    assert!(msg.deserialize(&mut reader(&w)).is_err());
}

#[test]
fn scene_data_message_missing_recipients_size_leaves_nothing_behind() {
    let mut w = NetDataWriter::new();
    w.put_ushort(77);
    let mut msg = SceneDataMessage::default();
    let _ = msg.deserialize(&mut reader(&w));
    assert_eq!(msg.message_index, 77);
    assert!(msg.recipients.is_none());
    assert!(msg.payload.is_none());
}

#[test]
fn scene_data_message_empty_reader_is_an_error() {
    let mut msg = SceneDataMessage::default();
    assert!(msg.deserialize(&mut empty()).is_err());
}

#[test]
fn scene_data_message_double_round_trip_is_byte_identical() {
    let mut msg = SceneDataMessage { message_index: 900, recipients: Some(vec![4]), payload: Some(random_bytes(17, 5)), ..Default::default() };
    let mut w1 = NetDataWriter::new();
    msg.serialize(&mut w1).expect("serialize");
    let mut mid = SceneDataMessage::default();
    mid.deserialize(&mut reader(&w1)).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize(&mut w2).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

// ── RemoteSceneDataMessage: [messageIndex:2][payload]; payload_length is the valid prefix ──

#[test]
fn remote_scene_data_message_round_trip_preserves_all_fields() {
    let payload = random_bytes(18, 24);
    let mut msg = RemoteSceneDataMessage { message_index: 700, payload: Some(payload.clone()), payload_length: payload.len() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 26);
    let mut result = RemoteSceneDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.message_index, 700);
    assert_eq!(result.payload, Some(payload));
    assert_eq!(result.payload_length, 24);
}

#[test]
fn remote_scene_data_message_no_payload_leaves_payload_none() {
    let mut msg = RemoteSceneDataMessage { message_index: 8, payload: None, payload_length: 0 };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 2);
    let mut result = RemoteSceneDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.message_index, 8);
    assert!(result.payload.is_none());
    assert_eq!(result.payload_length, 0);
}

#[test]
fn remote_scene_data_message_payload_length_limits_written_bytes() {
    let mut msg = RemoteSceneDataMessage { message_index: 1, payload: Some(vec![1, 2, 3, 4, 5, 6, 7, 8]), payload_length: 5 };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 7);
    let mut result = RemoteSceneDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.payload, Some(vec![1, 2, 3, 4, 5]));
    assert_eq!(result.payload_length, 5);
}

#[test]
fn remote_scene_data_message_empty_reader_is_an_error() {
    let mut msg = RemoteSceneDataMessage::default();
    assert!(msg.deserialize(&mut empty()).is_err());
}

#[test]
fn remote_scene_data_message_double_round_trip_is_byte_identical() {
    let mut msg = RemoteSceneDataMessage { message_index: 31, payload: Some(random_bytes(19, 12)), payload_length: 12 };
    let mut w1 = NetDataWriter::new();
    msg.serialize(&mut w1).expect("serialize");
    let mut mid = RemoteSceneDataMessage::default();
    mid.deserialize(&mut reader(&w1)).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize(&mut w2).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

// ── Server wrapper messages that prepend a ushort player id ──

#[test]
fn server_avatar_data_message_round_trip_preserves_nested_fields() {
    let payload = random_bytes(24, 10);
    let mut msg = ServerAvatarDataMessage { player_id_message: PlayerIdMessage { player_id: 77 }, avatar_data_message: RemoteAvatarDataMessage { player_id_message: PlayerIdMessage { player_id: 88 }, avatar_link_index: 3, message_index: 9, payload: Some(payload.clone()) } };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 2 + 2 + 1 + 1 + 10);
    let mut result = ServerAvatarDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.player_id_message.player_id, 77);
    assert_eq!(result.avatar_data_message.player_id_message.player_id, 88);
    assert_eq!(result.avatar_data_message.avatar_link_index, 3);
    assert_eq!(result.avatar_data_message.message_index, 9);
    assert_eq!(result.avatar_data_message.payload, Some(payload));
}

#[test]
fn server_avatar_data_message_double_round_trip_is_byte_identical() {
    let mut msg = ServerAvatarDataMessage { player_id_message: PlayerIdMessage { player_id: 1 }, avatar_data_message: RemoteAvatarDataMessage { player_id_message: PlayerIdMessage { player_id: 2 }, avatar_link_index: 0, message_index: 1, payload: Some(random_bytes(25, 7)) } };
    let mut w1 = NetDataWriter::new();
    msg.serialize(&mut w1).expect("serialize");
    let mut mid = ServerAvatarDataMessage::default();
    mid.deserialize(&mut reader(&w1)).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize(&mut w2).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

#[test]
fn server_scene_data_message_round_trip_preserves_nested_fields() {
    let payload = random_bytes(26, 6);
    let mut msg = ServerSceneDataMessage { player_id_message: PlayerIdMessage { player_id: 4 }, scene_data_message: RemoteSceneDataMessage { message_index: 700, payload: Some(payload.clone()), payload_length: payload.len() } };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 2 + 2 + 6);
    let mut result = ServerSceneDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.player_id_message.player_id, 4);
    assert_eq!(result.scene_data_message.message_index, 700);
    assert_eq!(result.scene_data_message.payload, Some(payload));
    assert_eq!(result.scene_data_message.payload_length, 6);
}

#[test]
fn server_scene_data_message_double_round_trip_is_byte_identical() {
    let mut msg = ServerSceneDataMessage { player_id_message: PlayerIdMessage { player_id: u16::MAX }, scene_data_message: RemoteSceneDataMessage { message_index: 1, payload: Some(random_bytes(27, 3)), payload_length: 3 } };
    let mut w1 = NetDataWriter::new();
    msg.serialize(&mut w1).expect("serialize");
    let mut mid = ServerSceneDataMessage::default();
    mid.deserialize(&mut reader(&w1)).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize(&mut w2).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

#[test]
fn server_avatar_change_message_round_trip_preserves_nested_fields() {
    let avatar_bytes = random_bytes(28, 12);
    let mut msg = ServerAvatarChangeMessage { ushort_player_id: PlayerIdMessage { player_id: 123 }, client_avatar_change_message: ClientAvatarChangeMessage { load_mode: 1, byte_array: Some(avatar_bytes.clone()), local_avatar_index: 200, ..Default::default() } };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 2 + 1 + 2 + 12 + 1 + 6); // +6 = three quantized body-fit scales
    let mut result = ServerAvatarChangeMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.ushort_player_id.player_id, 123);
    assert_eq!(result.client_avatar_change_message.load_mode, 1);
    assert_eq!(result.client_avatar_change_message.byte_array, Some(avatar_bytes));
    assert_eq!(result.client_avatar_change_message.local_avatar_index, 200);
}

#[test]
fn server_avatar_change_message_double_round_trip_is_byte_identical() {
    let mut msg = ServerAvatarChangeMessage { ushort_player_id: PlayerIdMessage { player_id: 9 }, client_avatar_change_message: ClientAvatarChangeMessage { load_mode: 0, byte_array: Some(random_bytes(29, 5)), local_avatar_index: 3, ..Default::default() } };
    let mut w1 = NetDataWriter::new();
    msg.serialize(&mut w1).expect("serialize");
    let mut mid = ServerAvatarChangeMessage::default();
    mid.deserialize(&mut reader(&w1)).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize(&mut w2).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

// ── ClientAvatarChangeMessage: [loadMode:1][length:2][bytes][LocalAvatarIndex:1][arm:2][leg:2][torso:2] ──

const FIT_BYTES: usize = 6;

#[test]
fn client_avatar_change_message_round_trip_preserves_all_fields() {
    let avatar_bytes = random_bytes(51, 40);
    let mut msg = ClientAvatarChangeMessage { load_mode: 2, byte_array: Some(avatar_bytes.clone()), local_avatar_index: 254, arm_scale: 1.0625, leg_scale: 0.9375, torso_scale: 1.125 };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 1 + 2 + 40 + 1 + FIT_BYTES);

    let mut result = ClientAvatarChangeMessage::default();
    let mut r = reader(&w);
    result.deserialize(&mut r).expect("deserialize");
    assert_eq!(result.load_mode, 2);
    assert_eq!(result.byte_array, Some(avatar_bytes));
    assert_eq!(result.local_avatar_index, 254);
    assert!(close(result.arm_scale, 1.0625));
    assert!(close(result.leg_scale, 0.9375));
    assert!(close(result.torso_scale, 1.125));
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn client_avatar_change_message_no_byte_array_round_trips() {
    let mut msg = ClientAvatarChangeMessage { load_mode: 1, byte_array: None, local_avatar_index: 7, ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 4 + FIT_BYTES);
    let mut result = ClientAvatarChangeMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.load_mode, 1);
    assert!(result.byte_array.is_none());
    assert_eq!(result.local_avatar_index, 7);
}

/// Most construction sites never touch the fit fields, so a default-constructed message must put
/// identity on the wire — a raw 0 would collapse every fitted bone to zero length on the receiver.
#[test]
fn client_avatar_change_message_unset_fit_serializes_as_identity_not_zero() {
    let mut msg = ClientAvatarChangeMessage { load_mode: 0, byte_array: None, local_avatar_index: 0, ..Default::default() };
    assert_eq!(msg.arm_scale, 0.0);
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    let mut result = ClientAvatarChangeMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert!(close(result.arm_scale, 1.0));
    assert!(close(result.leg_scale, 1.0));
    assert!(close(result.torso_scale, 1.0));
}

#[test]
fn sanitize_fit_scale_clamps_to_the_valid_band() {
    for (input, expected) in [(0.0f32, 1.0f32), (-2.0, 1.0), (f32::NAN, 1.0), (f32::INFINITY, 1.0), (1e9, 1.5), (1e-9, 0.5), (1.15, 1.15)] {
        assert_eq!(ClientAvatarChangeMessage::sanitize_fit_scale(input), expected, "{input}");
    }
}

#[test]
fn client_avatar_change_message_empty_byte_array_same_bytes_as_none_deserializes_to_none() {
    let mut none_msg = ClientAvatarChangeMessage { load_mode: 1, byte_array: None, local_avatar_index: 7, ..Default::default() };
    let mut empty_msg = ClientAvatarChangeMessage { load_mode: 1, byte_array: Some(Vec::new()), local_avatar_index: 7, ..Default::default() };
    let mut w1 = NetDataWriter::new();
    none_msg.serialize(&mut w1).expect("serialize");
    let mut w2 = NetDataWriter::new();
    empty_msg.serialize(&mut w2).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
    let mut result = ClientAvatarChangeMessage::default();
    result.deserialize(&mut reader(&w2)).expect("deserialize");
    assert!(result.byte_array.is_none());
}

#[test]
fn client_avatar_change_message_length_beyond_available_is_an_error() {
    let mut w = NetDataWriter::new();
    w.put_byte(1); // loadMode
    w.put_ushort(50); // claims 50 bytes
    w.put_bytes(&[1, 2, 3]);
    let mut msg = ClientAvatarChangeMessage::default();
    assert!(msg.deserialize(&mut reader(&w)).is_err());
}

#[test]
fn client_avatar_change_message_double_round_trip_is_byte_identical() {
    let mut msg = ClientAvatarChangeMessage { load_mode: 3, byte_array: Some(random_bytes(52, 21)), local_avatar_index: 90, ..Default::default() };
    let mut w1 = NetDataWriter::new();
    msg.serialize(&mut w1).expect("serialize");
    let mut mid = ClientAvatarChangeMessage::default();
    mid.deserialize(&mut reader(&w1)).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize(&mut w2).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

// ── ClientBodyFitMessage / ServerBodyFitMessage ──

/// Everything the body-fit solver can produce lands in [0.5, 1.5], which is exactly the quantized
/// range — so no legitimate fit is degraded, and nothing outside the band is representable.
#[test]
fn every_scale_the_solver_can_produce_survives_quantization() {
    for i in 0..=1000 {
        let scale = 0.5 + i as f32 * (1.0 / 1000.0);
        let round_tripped = ClientAvatarChangeMessage::decompress_fit_scale(ClientAvatarChangeMessage::compress_fit_scale(scale));
        assert!(close(scale, round_tripped), "{scale} -> {round_tripped}");
    }
}

#[test]
fn quantized_scale_is_never_outside_the_valid_band() {
    for raw in [0u16, 1, 32767, 32768, 65534, 65535] {
        let decoded = ClientAvatarChangeMessage::decompress_fit_scale(raw);
        assert!((ClientAvatarChangeMessage::FIT_SCALE_MIN..=ClientAvatarChangeMessage::FIT_SCALE_MAX).contains(&decoded), "{raw}");
    }
}

#[test]
fn client_body_fit_message_round_trip_preserves_scales() {
    let mut msg = ClientBodyFitMessage { arm_scale: 1.0625, leg_scale: 0.9375, torso_scale: 1.125 };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 6);
    let mut result = ClientBodyFitMessage::default();
    let mut r = reader(&w);
    result.deserialize(&mut r).expect("deserialize");
    assert!(close(result.arm_scale, 1.0625));
    assert!(close(result.leg_scale, 0.9375));
    assert!(close(result.torso_scale, 1.125));
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn client_body_fit_message_unset_scales_read_back_as_identity() {
    let mut w = NetDataWriter::new();
    ClientBodyFitMessage::default().serialize(&mut w).expect("serialize");
    let mut result = ClientBodyFitMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert!(close(result.arm_scale, 1.0));
    assert!(close(result.leg_scale, 1.0));
    assert!(close(result.torso_scale, 1.0));
}

#[test]
fn server_body_fit_message_round_trip_preserves_sender_and_scales() {
    let mut msg = ServerBodyFitMessage { ushort_player_id: PlayerIdMessage { player_id: 4242 }, body_fit: ClientBodyFitMessage { arm_scale: 1.05, leg_scale: 0.95, torso_scale: 1.02 } };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 2 + 6);
    let mut result = ServerBodyFitMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.ushort_player_id.player_id, 4242);
    assert!(close(result.body_fit.arm_scale, 1.05));
    assert!(close(result.body_fit.leg_scale, 0.95));
    assert!(close(result.body_fit.torso_scale, 1.02));
}

#[test]
fn server_body_fit_message_double_round_trip_is_byte_identical() {
    let mut msg = ServerBodyFitMessage { ushort_player_id: PlayerIdMessage { player_id: 17 }, body_fit: ClientBodyFitMessage { arm_scale: 0.88, leg_scale: 1.12, torso_scale: 0.94 } };
    let mut w1 = NetDataWriter::new();
    msg.serialize(&mut w1).expect("serialize");
    let mut mid = ServerBodyFitMessage::default();
    mid.deserialize(&mut reader(&w1)).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize(&mut w2).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

/// A hostile or corrupt scale must be clamped at the boundary, not carried into a remote's skeleton.
#[test]
fn client_body_fit_message_hostile_scales_are_clamped_on_read() {
    let mut w = NetDataWriter::new();
    w.put_ushort(ClientAvatarChangeMessage::compress_fit_scale(0.0));
    w.put_ushort(ClientAvatarChangeMessage::compress_fit_scale(f32::NAN));
    w.put_ushort(ClientAvatarChangeMessage::compress_fit_scale(1e9));
    let mut result = ClientBodyFitMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert!(close(result.arm_scale, 1.0));
    assert!(close(result.leg_scale, 1.0));
    assert!(close(result.torso_scale, 1.5));
}

#[test]
fn client_body_fit_message_truncated_is_an_error() {
    let mut result = ClientBodyFitMessage::default();
    assert!(result.deserialize(&mut NetDataReader::from_slice(&[0, 0, 0])).is_err());
}

// ── BasisCompactId — the polymorphic player-id encoding ──

fn compact_round_trip(input: &str) -> String {
    let mut w = NetDataWriter::new();
    BasisCompactId::write(&mut w, input).expect("write");
    BasisCompactId::read(&mut reader(&w)).expect("read")
}

fn compact_encoded(input: &str) -> usize {
    let mut w = NetDataWriter::new();
    BasisCompactId::write(&mut w, input).expect("write");
    w.length()
}

/// Old cost: a 2-byte length prefix plus UTF-8.
fn legacy(input: &str) -> usize {
    2 + input.len()
}

#[test]
fn any_id_round_trips_exactly() {
    for input in [
        "76561198012345678",
        "76561197960287930",
        "18446744073709551615", // u64::MAX, the longest numeric id that still packs
        "0",
        "d3b07384-d9a0-4f1e-8b1a-2c3d4e5f6071",
        "D3B07384-D9A0-4F1E-8B1A-2C3D4E5F6071",
        "d3b07384d9a04f1e8b1a2c3d4e5f6071",
        "D3B07384D9A04F1E8B1A2C3D4E5F6071",
        "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK",
        "7a0ab549e93cf2bc804168473e065a2f4d293b1be6cefb87df862eb6de086219",
        "7A0AB549E93CF2BC804168473E065A2F4D293B1BE6CEFB87DF862EB6DE086219",
        "",
        "Failure",
        "007", // leading zeros would not survive a u64
        "99999999999999999999999", // overflows u64
        "dEadBeEf", // mixed-case hex
        "steam:76561198012345678",
        "did:web:example.com:users:alice",
        "a-perfectly-ordinary-username",
        "ünïcøde-ïd-ヘ",
    ] {
        assert_eq!(compact_round_trip(input), input);
    }
}

#[test]
fn recognised_shapes_get_smaller() {
    for (input, expected_bytes) in [("76561198012345678", 9usize), ("d3b07384-d9a0-4f1e-8b1a-2c3d4e5f6071", 18), ("d3b07384d9a04f1e8b1a2c3d4e5f6071", 18), ("7a0ab549e93cf2bc804168473e065a2f4d293b1be6cefb87df862eb6de086219", 35)] {
        assert_eq!(compact_encoded(input), expected_bytes, "{input}");
        assert!(compact_encoded(input) < legacy(input));
    }
}

#[test]
fn did_key_elides_its_fixed_prefix() {
    const DID: &str = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
    assert_eq!(compact_encoded(DID), legacy(DID) - 8);
}

/// The fallback must never cost more than one byte over the old encoding, so an id shape nobody
/// anticipated cannot regress the wire.
#[test]
fn unrecognised_shapes_cost_at_most_one_extra_byte() {
    for input in ["a-perfectly-ordinary-username", "steam:76561198012345678", "dEadBeEf", ""] {
        assert!(compact_encoded(input) <= legacy(input) + 1, "{input}");
    }
}

#[test]
fn long_ids_still_round_trip() {
    let long_hex = "a".repeat(600); // past the hex fast path
    let long_text = "x".repeat(4000);
    assert_eq!(compact_round_trip(&long_hex), long_hex);
    assert_eq!(compact_round_trip(&long_text), long_text);
}

#[test]
fn compact_id_truncated_buffers_are_errors() {
    for input in ["76561198012345678", "d3b07384-d9a0-4f1e-8b1a-2c3d4e5f6071", "a-perfectly-ordinary-username"] {
        let mut w = NetDataWriter::new();
        BasisCompactId::write(&mut w, input).expect("write");
        let full = w.copy_data();
        assert!(BasisCompactId::read(&mut NetDataReader::from_slice(&full[..full.len() - 1])).is_err(), "{input}");
    }
    assert!(BasisCompactId::read(&mut empty()).is_err());
}

// ── BasisPlatformCodec — platform names collapse to one byte; unknown ones round-trip as text ──

fn platform_round_trip(input: &str) -> String {
    let mut w = NetDataWriter::new();
    BasisPlatformCodec::write(&mut w, input).expect("write");
    BasisPlatformCodec::read(&mut reader(&w)).expect("read")
}

fn platform_encoded(input: &str) -> usize {
    let mut w = NetDataWriter::new();
    BasisPlatformCodec::write(&mut w, input).expect("write");
    w.length()
}

#[test]
fn known_platform_round_trips_in_one_byte() {
    for platform in ["WindowsPlayer", "WindowsEditor", "Android", "OSXPlayer", "LinuxPlayer", "IPhonePlayer", "PS5", "VisionOS", "WebGLPlayer"] {
        assert_eq!(platform_round_trip(platform), platform);
        assert_eq!(platform_encoded(platform), 1, "{platform}");
    }
}

/// The load-test console reports "Headless", which is not a Unity platform. Left out of the table
/// it would fall back to a 10-byte string on every simulated client.
#[test]
fn headless_load_test_platform_is_in_the_table() {
    assert_eq!(platform_round_trip("Headless"), "Headless");
    assert_eq!(platform_encoded("Headless"), 1);
}

#[test]
fn unknown_platform_falls_back_to_a_string() {
    for platform in ["SomeFuturePlatform", "Failure", "", "windowsplayer"] {
        assert_eq!(platform_round_trip(platform), platform);
    }
}

#[test]
fn platform_codec_empty_reader_is_an_error() {
    assert!(BasisPlatformCodec::read(&mut empty()).is_err());
}

// ── ClientMetaDataMessage carries a compact id + platform ──

#[test]
fn meta_data_round_trips_all_three_fields() {
    let mut msg = ClientMetaDataMessage { player_uuid: "76561198012345678".into(), player_display_name: "Some Player".into(), player_platform: "WindowsPlayer".into() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    let mut result = ClientMetaDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result, msg);
}

#[test]
fn empty_fields_still_report_failure_as_before() {
    let mut w = NetDataWriter::new();
    ClientMetaDataMessage::default().serialize(&mut w).expect("serialize");
    let mut result = ClientMetaDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.player_uuid, "Failure");
    assert_eq!(result.player_display_name, "Failure");
    assert_eq!(result.player_platform, "Failure");
}

#[test]
fn steam_id_on_windows_is_smaller_than_the_old_encoding() {
    let mut msg = ClientMetaDataMessage { player_uuid: "76561198012345678".into(), player_display_name: "Some Player".into(), player_platform: "WindowsPlayer".into() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    let legacy = (2 + 17) + (2 + 11) + (2 + 13); // three length-prefixed UTF-8 strings
    assert!(w.length() < legacy, "encoded {}B, legacy {legacy}B", w.length());
    assert_eq!(w.length(), 9 + (2 + 11) + 1);
}

// ── ServerReadyBatchMessage — the join fill ──

/// Join-fill-shaped data: a small alphabet with heavy repetition across records, which is exactly
/// why batch compression pays where per-record compression did not.
fn batch_payload(length: usize, seed: u64) -> Vec<u8> {
    let mut rng = TestRng::new(seed);
    let urls = ["https://BasisFramework.b-cdn.net/Avatars/BEE/BEE/leona/27ca99b1efe04383b061c7def2684f60.BEE", "https://BasisFramework.b-cdn.net/Avatars/BEE/BEE/rex/8812aa4cfe1140239bb17ce4a1120fa2.BEE"];
    let mut text = String::new();
    while text.len() < length {
        text.push_str(urls[rng.next(urls.len())]);
    }
    text.as_bytes()[..length].to_vec()
}

#[test]
fn batch_round_trips_payload_and_count() {
    let payload = batch_payload(4096, 11);
    let mut batch = ServerReadyBatchMessage { count: 37, payload: payload.clone(), ..Default::default() };
    let mut w = NetDataWriter::new();
    batch.serialize(&mut w).expect("serialize");
    let mut result = ServerReadyBatchMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.count, 37);
    assert_eq!(result.payload, payload);
}

#[test]
fn repetitive_batch_is_actually_compressed() {
    let payload = batch_payload(8192, 12);
    let mut batch = ServerReadyBatchMessage { count: 60, payload: payload.clone(), ..Default::default() };
    let mut w = NetDataWriter::new();
    batch.serialize(&mut w).expect("serialize");
    assert!(batch.was_compressed);
    assert!(w.length() < payload.len() / 2, "batch was {}B for {}B of payload", w.length(), payload.len());
}

/// Deflate expands short or high-entropy input, so the encoder must be free to skip it — and the
/// decoder must honour the per-batch flag rather than assuming compression happened.
#[test]
fn tiny_batch_skips_compression_and_still_round_trips() {
    let payload = b"one small record".to_vec();
    let mut batch = ServerReadyBatchMessage { count: 1, payload: payload.clone(), ..Default::default() };
    let mut w = NetDataWriter::new();
    batch.serialize(&mut w).expect("serialize");
    assert!(!batch.was_compressed);
    let mut result = ServerReadyBatchMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.payload, payload);
}

#[test]
fn incompressible_batch_is_not_stored_larger_than_raw() {
    let payload = random_bytes(13, 4096); // high entropy, deflate cannot win
    let mut batch = ServerReadyBatchMessage { count: 5, payload: payload.clone(), ..Default::default() };
    let mut w = NetDataWriter::new();
    batch.serialize(&mut w).expect("serialize");
    let mut result = ServerReadyBatchMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.payload, payload);
    assert!(w.length() <= payload.len() + 16, "batch grew to {}B from {}B", w.length(), payload.len());
}

#[test]
fn empty_batch_round_trips() {
    let mut batch = ServerReadyBatchMessage { count: 0, payload: Vec::new(), ..Default::default() };
    let mut w = NetDataWriter::new();
    batch.serialize(&mut w).expect("serialize");
    let mut result = ServerReadyBatchMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.count, 0);
    assert!(result.payload.is_empty());
}

#[test]
fn batch_length_beyond_available_is_an_error() {
    let mut w = NetDataWriter::new();
    w.put_ushort(3);
    w.put_bool(false);
    w.put_int(9999); // claims far more than follows
    w.put_bytes(&[1, 2, 3]);
    let mut batch = ServerReadyBatchMessage::default();
    assert!(batch.deserialize(&mut reader(&w)).is_err());
}

#[test]
fn batch_corrupt_compressed_body_is_an_error() {
    let mut w = NetDataWriter::new();
    w.put_ushort(3);
    w.put_bool(true);
    w.put_int(6);
    w.put_bytes(&[0xFF, 0xFE, 0xFD, 0xFC, 0xFB, 0xFA]);
    let mut batch = ServerReadyBatchMessage::default();
    assert!(batch.deserialize(&mut reader(&w)).is_err());
}

/// The real shape: many ServerReadyMessages concatenated, then read back one at a time.
#[test]
fn concatenated_ready_messages_read_back_individually() {
    let mut inner = NetDataWriter::new();
    const COUNT: u16 = 25;
    for i in 0..COUNT {
        ServerReadyMessage {
            player_id_message: PlayerIdMessage { player_id: 1000 + i },
            local_ready_message: ReadyMessage {
                player_meta_data_message: ClientMetaDataMessage { player_uuid: format!("7656119801234{i:04}"), player_display_name: format!("Player{i}"), player_platform: "WindowsPlayer".into() },
                client_avatar_change_message: ClientAvatarChangeMessage { load_mode: 1, byte_array: Some(vec![1, 2, 3, 4]), local_avatar_index: i as u8, ..Default::default() },
                local_avatar_sync_message: LocalAvatarSyncMessage { data_quality_level: BitQuality::High as u8, array: Some(vec![0u8; payload_size(BitQuality::High)]), ..Default::default() },
            },
        }
        .serialize(&mut inner)
        .expect("serialize");
    }

    let mut batch = ServerReadyBatchMessage { count: COUNT, payload: inner.copy_data(), ..Default::default() };
    let mut w = NetDataWriter::new();
    batch.serialize(&mut w).expect("serialize");

    let mut received = ServerReadyBatchMessage::default();
    received.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(received.count, COUNT);

    let mut batch_reader = NetDataReader::new(received.payload);
    for i in 0..COUNT {
        let mut srm = ServerReadyMessage::default();
        srm.deserialize(&mut batch_reader).expect("record");
        assert_eq!(srm.player_id_message.player_id, 1000 + i);
        assert_eq!(srm.local_ready_message.player_meta_data_message.player_uuid, format!("7656119801234{i:04}"));
        assert_eq!(srm.local_ready_message.player_meta_data_message.player_platform, "WindowsPlayer");
        assert_eq!(srm.local_ready_message.client_avatar_change_message.local_avatar_index, i as u8);
    }
    assert_eq!(batch_reader.available_bytes(), 0);
}

// ── BasisAvatarCloneRequest/Response, PlayerIdMessage widths, AvatarLoadDataMessage ──

#[test]
fn basis_avatar_clone_request_round_trip_boundary_ids() {
    for id in [0u16, 1, u16::MAX] {
        let mut msg = BasisAvatarCloneRequest { requesting_user: id };
        let mut w = NetDataWriter::new();
        msg.serialize(&mut w).expect("serialize");
        assert_eq!(w.length(), 2);
        let mut result = BasisAvatarCloneRequest::default();
        result.deserialize(&mut reader(&w)).expect("deserialize");
        assert_eq!(result.requesting_user, id);
    }
}

#[test]
fn basis_avatar_clone_response_round_trip_boundary_ids() {
    for id in [0u16, u16::MAX] {
        let mut msg = BasisAvatarCloneResponse { requesting_user: id };
        let mut w = NetDataWriter::new();
        msg.serialize(&mut w).expect("serialize");
        assert_eq!(w.length(), 2);
        let mut result = BasisAvatarCloneResponse::default();
        result.deserialize(&mut reader(&w)).expect("deserialize");
        assert_eq!(result.requesting_user, id);
    }
}

#[test]
fn player_id_message_default_path_round_trips_ushort() {
    let msg = PlayerIdMessage { player_id: 4242 };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    assert_eq!(w.length(), 2);
    let mut result = PlayerIdMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.player_id, 4242);
}

#[test]
fn player_id_message_small_id_variant_writes_single_byte() {
    let msg = PlayerIdMessage { player_id: 200 };
    let mut w = NetDataWriter::new();
    msg.serialize_sized(&mut w, false).expect("serialize");
    assert_eq!(w.as_read_only_span(), &[200]);
    let mut result = PlayerIdMessage::default();
    result.deserialize_sized(&mut reader(&w), false).expect("deserialize");
    assert_eq!(result.player_id, 200);
}

#[test]
fn player_id_message_large_id_variant_round_trips_ushort_max() {
    let msg = PlayerIdMessage { player_id: u16::MAX };
    let mut w = NetDataWriter::new();
    msg.serialize_sized(&mut w, true).expect("serialize");
    assert_eq!(w.length(), 2);
    let mut result = PlayerIdMessage::default();
    result.deserialize_sized(&mut reader(&w), true).expect("deserialize");
    assert_eq!(result.player_id, u16::MAX);
}

#[test]
fn avatar_load_data_message_serialize_layout_header_sender_size_then_raw_payload() {
    let mut msg = AvatarLoadDataMessage { message_index: 4, who_sent_us_this: 777, payload: Some(vec![1, 2, 3, 4, 5]), ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    let mut r = reader(&w);
    assert_eq!(r.get_byte().expect("index"), 4);
    assert_eq!(r.get_ushort().expect("sender"), 777);
    assert_eq!(r.get_ushort().expect("size"), 5);
    assert_eq!(r.get_remaining_bytes(), vec![1, 2, 3, 4, 5]);

    let mut w_none = NetDataWriter::new();
    AvatarLoadDataMessage { message_index: 1, who_sent_us_this: 2, payload: None, ..Default::default() }.serialize(&mut w_none).expect("serialize");
    assert_eq!(w_none.length(), 5); // header + sender + size 0, no payload bytes
}

#[test]
fn avatar_load_data_message_empty_reader_is_an_error() {
    let mut msg = AvatarLoadDataMessage::default();
    assert!(msg.deserialize(&mut empty()).is_err());
}

#[test]
fn avatar_load_data_message_round_trip_preserves_all_fields() {
    let mut msg = AvatarLoadDataMessage { message_index: 4, who_sent_us_this: u16::MAX, payload: Some(vec![9, 8, 7, 6, 5, 4, 3, 2, 1, 0]), ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    let mut result = AvatarLoadDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.message_index, 4);
    assert_eq!(result.who_sent_us_this, u16::MAX);
    assert_eq!(result.payload_size, 10);
    assert_eq!(result.payload, msg.payload);
}

#[test]
fn avatar_load_data_message_no_payload_round_trips_as_none() {
    let mut msg = AvatarLoadDataMessage { message_index: 1, who_sent_us_this: 2, payload: None, ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w).expect("serialize");
    let mut result = AvatarLoadDataMessage::default();
    result.deserialize(&mut reader(&w)).expect("deserialize");
    assert_eq!(result.message_index, 1);
    assert_eq!(result.who_sent_us_this, 2);
    assert_eq!(result.payload_size, 0);
    assert!(result.payload.is_none());
}

#[test]
fn avatar_load_data_message_overclaimed_payload_is_an_error() {
    let mut w = NetDataWriter::new();
    w.put_byte(4);
    w.put_ushort(1);
    w.put_ushort(500);
    w.put_bytes(&[1, 2]);
    let mut result = AvatarLoadDataMessage::default();
    assert!(result.deserialize(&mut reader(&w)).is_err());
}

// ── LocalAvatarSyncMessage: quality-in-payload path, channel-derived path, additional-only ──

#[test]
fn local_avatar_sync_message_payload_path_round_trips() {
    for q in BitQuality::ALL {
        let size = payload_size(q);
        let payload = random_bytes(100 + q as u64, size);
        let mut msg = LocalAvatarSyncMessage { array: Some(payload.clone()), ..Default::default() };
        let mut w = NetDataWriter::new();
        msg.serialize(&mut w, Some(q)).expect("serialize");
        assert_eq!(w.length(), 1 + size + 1);
        assert_eq!(w.as_read_only_span()[0], q as u8);

        let mut result = LocalAvatarSyncMessage::default();
        let mut r = reader(&w);
        result.deserialize(&mut r).expect("deserialize");
        assert_eq!(result.data_quality_level, q as u8);
        assert_eq!(result.array, Some(payload));
        assert_eq!(result.additional_avatar_data_size, 0);
        assert!(result.additional_avatar_datas.is_none());
        assert_eq!(r.available_bytes(), 0);
    }
}

#[test]
fn local_avatar_sync_message_channel_path_no_additional_writes_bare_payload() {
    for q in BitQuality::ALL {
        let size = payload_size(q);
        let payload = random_bytes(200 + q as u64, size);
        let mut msg = LocalAvatarSyncMessage { array: Some(payload.clone()), ..Default::default() };
        let mut w = NetDataWriter::new();
        msg.serialize_for_channel(&mut w, q).expect("serialize");
        assert_eq!(w.length(), size);

        let mut result = LocalAvatarSyncMessage::default();
        let mut r = reader(&w);
        result.deserialize_for_channel(&mut r, q as u8, false).expect("deserialize");
        assert_eq!(result.data_quality_level, q as u8);
        assert_eq!(result.array, Some(payload));
        assert_eq!(result.additional_avatar_data_size, 0);
        assert!(result.additional_avatar_datas.is_none());
        assert_eq!(r.available_bytes(), 0);
    }
}

fn additional(index: u8, bytes: &[u8]) -> AdditionalAvatarData {
    AdditionalAvatarData { message_index: index, array: Some(bytes.to_vec()), ..Default::default() }
}

#[test]
fn local_avatar_sync_message_channel_path_with_additional_data_round_trips() {
    let q = BitQuality::Medium;
    let size = payload_size(q);
    let payload = random_bytes(30, size);
    let mut msg = LocalAvatarSyncMessage { array: Some(payload.clone()), additional_avatar_datas: Some(vec![additional(1, &[10, 20, 30]), additional(6, &[42])]), linked_avatar_index: 4, ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize_for_channel(&mut w, q).expect("serialize");
    assert_eq!(w.length(), size + 2 + 5 + 3); // payload + [count][linked] + entries

    let mut result = LocalAvatarSyncMessage::default();
    let mut r = reader(&w);
    result.deserialize_for_channel(&mut r, q as u8, true).expect("deserialize");
    assert_eq!(result.array, Some(payload));
    assert_eq!(result.additional_avatar_data_size, 2);
    assert_eq!(result.linked_avatar_index, 4);
    let datas = result.additional_avatar_datas.expect("additional");
    assert_eq!(datas[0].message_index, 1);
    assert_eq!(datas[0].array, Some(vec![10, 20, 30]));
    assert_eq!(datas[1].message_index, 6);
    assert_eq!(datas[1].array, Some(vec![42]));
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn local_avatar_sync_message_additional_only_section_round_trips_including_empty_entry() {
    let mut msg = LocalAvatarSyncMessage {
        additional_avatar_datas: Some(vec![additional(1, &[5, 6, 7]), AdditionalAvatarData { message_index: 2, array: None, ..Default::default() }, additional(3, &[9])]),
        linked_avatar_index: 11,
        ..Default::default()
    };
    let mut w = NetDataWriter::new();
    msg.serialize_additional_only(&mut w).expect("serialize");
    assert_eq!(w.length(), 2 + 5 + 2 + 3); // the empty entry keeps its full [size:0][messageIndex] header

    let mut result = LocalAvatarSyncMessage::default();
    let mut r = reader(&w);
    result.deserialize_additional_data(&mut r).expect("deserialize");
    assert_eq!(result.additional_avatar_data_size, 3);
    assert_eq!(result.linked_avatar_index, 11);
    let datas = result.additional_avatar_datas.expect("additional");
    assert_eq!(datas[0].message_index, 1);
    assert_eq!(datas[0].array, Some(vec![5, 6, 7]));
    assert_eq!(datas[1].payload_size, 0);
    assert_eq!(datas[1].message_index, 2);
    assert!(datas[1].array.is_none());
    assert_eq!(datas[2].message_index, 3);
    assert_eq!(datas[2].array, Some(vec![9]));
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn local_avatar_sync_message_empty_additional_array_serializes_same_as_none() {
    let q = BitQuality::VeryLow;
    let payload = random_bytes(33, payload_size(q));
    let mut none = LocalAvatarSyncMessage { array: Some(payload.clone()), ..Default::default() };
    let mut empty_msg = LocalAvatarSyncMessage { array: Some(payload), additional_avatar_datas: Some(Vec::new()), ..Default::default() };
    let mut w1 = NetDataWriter::new();
    none.serialize(&mut w1, Some(q)).expect("serialize");
    let mut w2 = NetDataWriter::new();
    empty_msg.serialize(&mut w2, Some(q)).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
    let mut result = LocalAvatarSyncMessage::default();
    result.deserialize(&mut reader(&w2)).expect("deserialize");
    assert!(result.additional_avatar_datas.is_none());
}

#[test]
fn local_avatar_sync_message_additional_count_over_255_serializes_same_as_none() {
    let q = BitQuality::Low;
    let payload = random_bytes(34, payload_size(q));
    let mut none = LocalAvatarSyncMessage { array: Some(payload.clone()), ..Default::default() };
    let mut oversized = LocalAvatarSyncMessage { array: Some(payload), additional_avatar_datas: Some(vec![AdditionalAvatarData::default(); 256]), ..Default::default() };
    let mut w1 = NetDataWriter::new();
    none.serialize(&mut w1, Some(q)).expect("serialize");
    let mut w2 = NetDataWriter::new();
    oversized.serialize(&mut w2, Some(q)).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

#[test]
fn local_avatar_sync_message_no_array_writes_stub_deserialize_no_panic() {
    let mut msg = LocalAvatarSyncMessage { array: None, ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w, Some(BitQuality::High)).expect("serialize");
    assert_eq!(w.as_read_only_span(), &[BitQuality::High as u8, 0]);
    let mut result = LocalAvatarSyncMessage::default();
    let _ = result.deserialize(&mut reader(&w));
    assert_eq!(result.data_quality_level, BitQuality::High as u8);
    assert!(result.array.is_none());
}

#[test]
fn local_avatar_sync_message_invalid_quality_writes_stub_deserialize_no_panic() {
    let mut msg = LocalAvatarSyncMessage { array: Some(vec![0u8; 4]), data_quality_level: 9, ..Default::default() };
    let mut w = NetDataWriter::new();
    msg.serialize(&mut w, None).expect("serialize");
    assert_eq!(w.length(), 2);
    assert_eq!(w.as_read_only_span()[0], 9);
    let mut result = LocalAvatarSyncMessage::default();
    let _ = result.deserialize(&mut reader(&w));
    assert_eq!(result.data_quality_level, 9);
    assert!(result.array.is_none());
}

#[test]
fn local_avatar_sync_message_empty_reader_deserialize_no_panic() {
    let mut result = LocalAvatarSyncMessage::default();
    let _ = result.deserialize(&mut empty());
    assert_eq!(result.data_quality_level, 0);
    assert!(result.array.is_none());
}

#[test]
fn local_avatar_sync_message_channel_path_truncated_payload_no_panic() {
    let mut result = LocalAvatarSyncMessage::default();
    let _ = result.deserialize_for_channel(&mut NetDataReader::from_slice(&[1, 2, 3, 4, 5]), BitQuality::High as u8, false);
    assert_eq!(result.data_quality_level, BitQuality::High as u8);
    assert!(result.array.is_none());
}

#[test]
fn local_avatar_sync_message_payload_path_double_round_trip_is_byte_identical() {
    let q = BitQuality::Medium;
    let mut msg = LocalAvatarSyncMessage { array: Some(random_bytes(32, payload_size(q))), ..Default::default() };
    let mut w1 = NetDataWriter::new();
    msg.serialize(&mut w1, Some(q)).expect("serialize");
    let mut mid = LocalAvatarSyncMessage::default();
    mid.deserialize(&mut reader(&w1)).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize(&mut w2, BitQuality::from_byte(mid.data_quality_level)).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

#[test]
fn local_avatar_sync_message_channel_path_double_round_trip_is_byte_identical() {
    let q = BitQuality::Low;
    let mut msg = LocalAvatarSyncMessage { array: Some(random_bytes(31, payload_size(q))), additional_avatar_datas: Some(vec![additional(8, &[1, 2])]), linked_avatar_index: 6, ..Default::default() };
    let mut w1 = NetDataWriter::new();
    msg.serialize_for_channel(&mut w1, q).expect("serialize");
    let mut mid = LocalAvatarSyncMessage::default();
    mid.deserialize_for_channel(&mut reader(&w1), q as u8, true).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize_for_channel(&mut w2, q).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

// ── ServerSideSyncPlayerMessage: [playerID][interval:1][sequence:1] then the sync payload ──

#[test]
fn server_side_sync_player_message_round_trip_preserves_all_fields() {
    for q in [BitQuality::Low, BitQuality::High] {
        let size = payload_size(q);
        let payload = random_bytes(300 + q as u64, size);
        let mut msg = ServerSideSyncPlayerMessage { player_id_message: PlayerIdMessage { player_id: u16::MAX }, interval: 33, sequence: 250, avatar_serialization: LocalAvatarSyncMessage { array: Some(payload.clone()), data_quality_level: q as u8, ..Default::default() } };
        let mut w = NetDataWriter::new();
        msg.serialize(&mut w).expect("serialize");
        assert_eq!(w.length(), 2 + 1 + 1 + 1 + size + 1);

        let mut result = ServerSideSyncPlayerMessage::default();
        let mut r = reader(&w);
        result.deserialize(&mut r).expect("deserialize");
        assert_eq!(result.player_id_message.player_id, u16::MAX);
        assert_eq!(result.interval, 33);
        assert_eq!(result.sequence, 250);
        assert_eq!(result.avatar_serialization.data_quality_level, q as u8);
        assert_eq!(result.avatar_serialization.array, Some(payload));
        assert_eq!(r.available_bytes(), 0);
    }
}

#[test]
fn server_side_sync_player_message_channel_deserialize_large_id_no_additional() {
    let q = BitQuality::VeryLow;
    let payload = random_bytes(40, payload_size(q));
    let mut w = NetDataWriter::new();
    PlayerIdMessage { player_id: 5000 }.serialize(&mut w).expect("id");
    w.put_byte(55); // interval
    w.put_byte(128); // sequence
    LocalAvatarSyncMessage { array: Some(payload.clone()), ..Default::default() }.serialize_for_channel(&mut w, q).expect("sync");

    let mut result = ServerSideSyncPlayerMessage::default();
    let mut r = reader(&w);
    result.deserialize_for_channel(&mut r, q as u8, false).expect("deserialize");
    assert_eq!(result.player_id_message.player_id, 5000);
    assert_eq!(result.interval, 55);
    assert_eq!(result.sequence, 128);
    assert_eq!(result.avatar_serialization.array, Some(payload));
    assert!(result.avatar_serialization.additional_avatar_datas.is_none());
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn server_side_sync_player_message_channel_deserialize_small_id_with_additional_data() {
    let q = BitQuality::High;
    let payload = random_bytes(41, payload_size(q));
    let mut w = NetDataWriter::new();
    PlayerIdMessage { player_id: 42 }.serialize_sized(&mut w, false).expect("id");
    w.put_byte(120); // interval
    w.put_byte(7); // sequence
    LocalAvatarSyncMessage { array: Some(payload.clone()), additional_avatar_datas: Some(vec![additional(2, &[4, 5, 6])]), linked_avatar_index: 1, ..Default::default() }.serialize_for_channel(&mut w, q).expect("sync");

    let mut result = ServerSideSyncPlayerMessage::default();
    let mut r = reader(&w);
    result.deserialize_for_channel_sized(&mut r, q as u8, true, false).expect("deserialize");
    assert_eq!(result.player_id_message.player_id, 42);
    assert_eq!(result.interval, 120);
    assert_eq!(result.sequence, 7);
    assert_eq!(result.avatar_serialization.array, Some(payload));
    assert_eq!(result.avatar_serialization.additional_avatar_data_size, 1);
    assert_eq!(result.avatar_serialization.linked_avatar_index, 1);
    assert_eq!(result.avatar_serialization.additional_avatar_datas.expect("additional")[0].array, Some(vec![4, 5, 6]));
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn server_side_sync_player_message_double_round_trip_is_byte_identical() {
    let q = BitQuality::Medium;
    let mut msg = ServerSideSyncPlayerMessage { player_id_message: PlayerIdMessage { player_id: 77 }, interval: 50, sequence: 1, avatar_serialization: LocalAvatarSyncMessage { array: Some(random_bytes(42, payload_size(q))), data_quality_level: q as u8, ..Default::default() } };
    let mut w1 = NetDataWriter::new();
    msg.serialize(&mut w1).expect("serialize");
    let mut mid = ServerSideSyncPlayerMessage::default();
    mid.deserialize(&mut reader(&w1)).expect("deserialize");
    let mut w2 = NetDataWriter::new();
    mid.serialize(&mut w2).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

#[test]
fn server_side_sync_player_message_truncated_header_is_an_error() {
    let mut result = ServerSideSyncPlayerMessage::default();
    assert!(result.deserialize(&mut NetDataReader::from_slice(&[1, 0, 33])).is_err());
}

// ── VoiceReceiversMessage: [count:1|2][ushort ids...] ──

#[test]
fn voice_receivers_message_large_count_round_trips() {
    let users = vec![1u16, 2, 70, u16::MAX];
    let mut msg = VoiceReceiversMessage { users: Some(users.clone()), users_length: users.len() };
    let mut w = NetDataWriter::new();
    msg.serialize_sized(&mut w, true).expect("serialize");
    assert_eq!(w.length(), 2 + users.len() * 2);

    let mut result = VoiceReceiversMessage::default();
    result.deserialize(&mut reader(&w), true).expect("deserialize");
    assert_eq!(result.users_length, users.len());
    let got = result.users.expect("users");
    assert!(got.len() >= result.users_length);
    assert_eq!(&got[..result.users_length], &users[..]);
}

#[test]
fn voice_receivers_message_byte_count_round_trips() {
    let users = vec![5u16, 10, 15];
    let mut msg = VoiceReceiversMessage { users: Some(users.clone()), users_length: users.len() };
    let mut w = NetDataWriter::new();
    msg.serialize_sized(&mut w, false).expect("serialize");
    assert_eq!(w.length(), 1 + users.len() * 2);
    assert_eq!(w.as_read_only_span()[0], 3);

    let mut result = VoiceReceiversMessage::default();
    result.deserialize(&mut reader(&w), false).expect("deserialize");
    assert_eq!(result.users_length, users.len());
    assert_eq!(&result.users.expect("users")[..3], &users[..]);
}

#[test]
fn voice_receivers_message_default_serialize_matches_large_count() {
    let users = vec![3u16, 9];
    let mut m1 = VoiceReceiversMessage { users: Some(users.clone()), users_length: users.len() };
    let mut m2 = VoiceReceiversMessage { users: Some(users), users_length: 2 };
    let mut w1 = NetDataWriter::new();
    m1.serialize(&mut w1).expect("serialize");
    let mut w2 = NetDataWriter::new();
    m2.serialize_sized(&mut w2, true).expect("serialize");
    assert_eq!(w1.copy_data(), w2.copy_data());
}

#[test]
fn voice_receivers_message_empty_users_writes_zero_count() {
    for (large_count, expected_bytes) in [(true, 2usize), (false, 1)] {
        let mut msg = VoiceReceiversMessage::default();
        let mut w = NetDataWriter::new();
        msg.serialize_sized(&mut w, large_count).expect("serialize");
        assert_eq!(w.length(), expected_bytes);
        let mut result = VoiceReceiversMessage::default();
        result.deserialize(&mut reader(&w), large_count).expect("deserialize");
        assert_eq!(result.users.as_deref().map(<[u16]>::len), Some(0));
        assert_eq!(result.users_length, 0);
    }
}

#[test]
fn voice_receivers_message_empty_reader_no_panic_empty_users() {
    let mut result = VoiceReceiversMessage::default();
    let _ = result.deserialize(&mut empty(), true);
    assert_eq!(result.users.as_deref().map(<[u16]>::len), Some(0));
}

#[test]
fn voice_receivers_message_truncated_count_no_panic_empty_users() {
    let mut r = NetDataReader::from_slice(&[42]); // 1 byte, large channel needs 2
    let mut result = VoiceReceiversMessage::default();
    let _ = result.deserialize(&mut r, true);
    assert!(result.users.as_deref().is_none_or(<[u16]>::is_empty));
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn voice_receivers_message_count_exceeds_data_no_panic_no_users() {
    let mut w = NetDataWriter::new();
    w.put_byte(10); // claims 10 recipients
    w.put_ushort(1); // but only 2 fit
    w.put_ushort(2);
    let mut r = reader(&w);
    let mut result = VoiceReceiversMessage::default();
    let _ = result.deserialize(&mut r, false);
    assert!(result.users.is_none());
    assert_eq!(result.users_length, 0);
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn voice_receivers_message_byte_count_over_255_truncates_to_255() {
    let users: Vec<u16> = (0..300).collect();
    let mut msg = VoiceReceiversMessage { users: Some(users.clone()), users_length: users.len() };
    let mut w = NetDataWriter::new();
    msg.serialize_sized(&mut w, false).expect("serialize");
    assert_eq!(w.length(), 1 + 255 * 2);
    assert_eq!(w.as_read_only_span()[0], 255);

    let mut result = VoiceReceiversMessage::default();
    result.deserialize(&mut reader(&w), false).expect("deserialize");
    assert_eq!(result.users_length, 255);
    assert_eq!(&result.users.expect("users")[..255], &users[..255]);
}
