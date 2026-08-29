//! Wire-format lock for the control/resource/misc message structs: serialize→deserialize round
//! trips (including nested composites like ServerReadyMessage), empty and boundary values,
//! size-guard behaviour, and the fallbacks on truncated input.
//!
//! Where the C# pinned "does not throw and falls back", the Rust reports the fault as an `Err`;
//! the tests then pin the state the message is left in, which is what the caller acts on.

use std::collections::HashSet;

use basis_network_core::SerializableBasis::{
    AdminRequest, AdminRequestMode, BytesMessage, CameraCountdownMessage, CameraPIPPositionMessage, CameraPIPStateMessage, CameraShutterSoundMessage, ChatMessage, ClientAvatarChangeMessage, ClientCameraCountdownMessage,
    ClientCameraPIPPositionMessage, ClientCameraPIPStateMessage, ClientMetaDataMessage, ConsoleData, ContentShareCleanupMessage, ContentShareMessage, ContentShareType, ErrorMessage, LocalAvatarSyncMessage, LocalLoadResource,
    ModifyResource, NetIDMessage, OwnershipTransferMessage, PermissionBitsetMap, PlayerIdMessage, PreloadReadyMessage, ReadyMessage, ServerChatMessage, ServerContentShareCleanupMessage, ServerContentShareMessage,
    ServerLibraryItem, ServerLibraryMessage, ServerMetaDataMessage, ServerNetIDMessage, ServerReadyMessage, ServerStatisticMessage, ServerUniqueIDMessages, SpawnPreloadedMessage, UnLoadResource, UshortUniqueIDMessage,
};
use basis_network_core::compression::{BasisAvatarBitPacking, BitQuality};
use basis_network_core::{NetDataReader, NetDataWriter};
use basis_server_tests::support::delta_test_support::TestRng;

fn reader_for(writer: &NetDataWriter) -> NetDataReader {
    NetDataReader::new(writer.copy_data())
}

fn empty_reader() -> NetDataReader {
    NetDataReader::from_slice(&[])
}

fn seeded_bytes(length: usize, seed: u64) -> Vec<u8> {
    TestRng::new(seed).bytes(length)
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}

// ── AdminRequest ──

#[test]
fn admin_request_round_trips_every_mode() {
    for mode in AdminRequestMode::ALL {
        let mut writer = NetDataWriter::new();
        AdminRequest::default().serialize(&mut writer, mode).expect("serialize");
        assert_eq!(writer.length(), 1);

        let mut back = AdminRequest::default();
        back.deserialize(&mut reader_for(&writer));
        assert_eq!(back.get_admin_request_mode(), Some(mode));
    }
}

#[test]
fn admin_request_empty_reader_does_not_panic_and_defaults_to_ban() {
    let mut request = AdminRequest::default();
    request.deserialize(&mut empty_reader());
    assert_eq!(request.get_admin_request_mode(), Some(AdminRequestMode::Ban));
}

#[test]
fn admin_request_unknown_mode_byte_is_not_a_mode() {
    let mut request = AdminRequest::default();
    request.deserialize(&mut NetDataReader::from_slice(&[0xFE]));
    assert_eq!(request.get_admin_request_mode(), None);
}

// ── BytesMessage ──

#[test]
fn bytes_message_round_trips_payload() {
    let source = seeded_bytes(257, 101);
    let mut writer = NetDataWriter::new();
    BytesMessage.serialize(&mut writer, &source).expect("serialize");
    assert_eq!(writer.length(), 2 + source.len());
    assert_eq!(BytesMessage.deserialize(&mut reader_for(&writer)), Some(source));
}

#[test]
fn bytes_message_round_trips_max_ushort_payload() {
    let source = seeded_bytes(usize::from(u16::MAX), 102);
    let mut writer = NetDataWriter::new();
    BytesMessage.serialize(&mut writer, &source).expect("serialize");
    assert_eq!(BytesMessage.deserialize(&mut reader_for(&writer)), Some(source));
}

#[test]
fn bytes_message_empty_payload_round_trips_to_empty() {
    let mut writer = NetDataWriter::new();
    BytesMessage.serialize(&mut writer, &[]).expect("serialize");
    assert_eq!(writer.length(), 2);
    assert_eq!(BytesMessage.deserialize(&mut reader_for(&writer)), Some(Vec::new()));
}

#[test]
fn bytes_message_declared_length_beyond_available_returns_none() {
    let mut writer = NetDataWriter::new();
    writer.put_ushort(10);
    writer.put_bytes(&[1, 2, 3]);
    assert!(BytesMessage.deserialize(&mut reader_for(&writer)).is_none());
}

#[test]
fn bytes_message_empty_reader_returns_none() {
    assert!(BytesMessage.deserialize(&mut empty_reader()).is_none());
}

#[test]
fn bytes_message_over_ushort_payload_is_refused() {
    let mut writer = NetDataWriter::new();
    assert!(BytesMessage.serialize(&mut writer, &vec![0u8; usize::from(u16::MAX) + 1]).is_err());
    assert_eq!(writer.length(), 0);
}

// ── Camera PIP messages ──

#[test]
fn camera_pip_state_message_active_round_trips_all_fields() {
    let mut msg = CameraPIPStateMessage { player_id: 1234, is_active: true, position_x: -1.5, position_y: 2.25, position_z: -300.125, rotation_x: 0.1, rotation_y: -0.2, rotation_z: 0.3, rotation_w: -0.9 };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 3 + 7 * 4);

    let mut back = CameraPIPStateMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back, msg);
}

#[test]
fn camera_pip_state_message_inactive_omits_transform() {
    let mut msg = CameraPIPStateMessage { player_id: u16::MAX, is_active: false, position_x: 99.0, rotation_w: 1.0, ..Default::default() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 3);

    let mut back = CameraPIPStateMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.player_id, u16::MAX);
    assert!(!back.is_active);
    assert_eq!(back.position_x, 0.0);
    assert_eq!(back.rotation_w, 0.0);
}

#[test]
fn camera_pip_state_message_double_round_trip_is_byte_identical() {
    let mut msg = CameraPIPStateMessage { player_id: 77, is_active: true, position_x: 1.0, position_y: 2.0, position_z: 3.0, rotation_x: 4.0, rotation_y: 5.0, rotation_z: 6.0, rotation_w: 7.0 };
    let mut first = NetDataWriter::new();
    msg.serialize(&mut first).expect("serialize");
    let mut back = CameraPIPStateMessage::default();
    back.deserialize(&mut reader_for(&first)).expect("deserialize");
    let mut second = NetDataWriter::new();
    back.serialize(&mut second).expect("serialize");
    assert_eq!(first.copy_data(), second.copy_data());
}

#[test]
fn camera_pip_state_message_truncated_transform_is_an_error() {
    let mut writer = NetDataWriter::new();
    writer.put_ushort(5);
    writer.put_bool(true);
    writer.put_float(1.0);
    let mut back = CameraPIPStateMessage::default();
    assert!(back.deserialize(&mut reader_for(&writer)).is_err());
}

#[test]
fn camera_pip_position_message_round_trips_all_fields() {
    let mut msg = CameraPIPPositionMessage { player_id: u16::MAX, position_x: 10.5, position_y: -0.0625, position_z: 512.0, rotation_x: -0.5, rotation_y: 0.5, rotation_z: -0.25, rotation_w: 0.75 };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 2 + 7 * 4);
    let mut back = CameraPIPPositionMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back, msg);
}

#[test]
fn client_camera_pip_state_message_active_round_trips() {
    let mut msg = ClientCameraPIPStateMessage { is_active: true, position_x: 1.25, position_y: -2.5, position_z: 3.75, rotation_x: -0.125, rotation_y: 0.375, rotation_z: -0.625, rotation_w: 0.875 };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 1 + 7 * 4);
    let mut back = ClientCameraPIPStateMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back, msg);
}

#[test]
fn client_camera_pip_state_message_inactive_writes_single_byte() {
    let mut msg = ClientCameraPIPStateMessage { is_active: false, position_x: 42.0, ..Default::default() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 1);
    let mut back = ClientCameraPIPStateMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert!(!back.is_active);
    assert_eq!(back.position_x, 0.0);
}

#[test]
fn client_camera_pip_position_message_round_trips() {
    let mut msg = ClientCameraPIPPositionMessage { position_x: f32::MAX, position_y: f32::EPSILON, position_z: -1.0, rotation_x: 0.0, rotation_y: 1.0, rotation_z: -0.5, rotation_w: 0.25 };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 28);
    let mut back = ClientCameraPIPPositionMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back, msg);
}

// ── Camera shutter / countdown ──

#[test]
fn camera_shutter_sound_message_round_trips() {
    for player_id in [0u16, 1, u16::MAX] {
        let mut msg = CameraShutterSoundMessage { player_id };
        let mut writer = NetDataWriter::new();
        msg.serialize(&mut writer).expect("serialize");
        assert_eq!(writer.length(), 2);
        let mut back = CameraShutterSoundMessage::default();
        back.deserialize(&mut reader_for(&writer)).expect("deserialize");
        assert_eq!(back.player_id, player_id);
    }
}

#[test]
fn camera_countdown_message_round_trips() {
    for (player_id, seconds) in [(0u16, 0u8), (500, 3), (u16::MAX, u8::MAX)] {
        let mut msg = CameraCountdownMessage { player_id, seconds };
        let mut writer = NetDataWriter::new();
        msg.serialize(&mut writer).expect("serialize");
        assert_eq!(writer.length(), 3);
        let mut back = CameraCountdownMessage::default();
        back.deserialize(&mut reader_for(&writer)).expect("deserialize");
        assert_eq!((back.player_id, back.seconds), (player_id, seconds));
    }
}

#[test]
fn client_camera_countdown_message_round_trips() {
    let mut msg = ClientCameraCountdownMessage { seconds: u8::MAX };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 1);
    let mut back = ClientCameraCountdownMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.seconds, u8::MAX);
}

// ── ChatMessage ──

#[test]
fn chat_message_round_trips_utf8_payload_and_sound_flag() {
    for sound in [true, false] {
        let payload = "chat ✦ 你好, ωorld".as_bytes().to_vec();
        let mut msg = ChatMessage { payload: payload.clone(), play_notification_sound: sound, ..Default::default() };
        let mut writer = NetDataWriter::new();
        msg.serialize(&mut writer).expect("serialize");
        let mut back = ChatMessage::default();
        back.deserialize(&mut reader_for(&writer)).expect("deserialize");
        assert_eq!(back.payload, payload);
        assert_eq!(back.payload_size, payload.len() as u16);
        assert_eq!(back.play_notification_sound, sound);
    }
}

#[test]
fn chat_message_empty_payload_round_trips_as_empty() {
    let mut msg = ChatMessage { payload: Vec::new(), play_notification_sound: false, ..Default::default() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 3);
    let mut back = ChatMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert!(back.payload.is_empty());
    assert_eq!(back.payload_size, 0);
    assert!(!back.play_notification_sound);
}

#[test]
fn chat_message_serialize_caps_payload_at_512_bytes() {
    let oversized = seeded_bytes(600, 103);
    let mut msg = ChatMessage { payload: oversized.clone(), play_notification_sound: true, ..Default::default() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 2 + ChatMessage::MAX_PAYLOAD_BYTES + 1);
    let mut back = ChatMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.payload_size, ChatMessage::MAX_PAYLOAD_BYTES as u16);
    assert_eq!(back.payload, oversized[..ChatMessage::MAX_PAYLOAD_BYTES]);
    assert!(back.play_notification_sound);
}

#[test]
fn chat_message_oversized_wire_declaration_reads_cap_and_skips_excess() {
    let wire_payload = seeded_bytes(600, 104);
    let mut writer = NetDataWriter::new();
    writer.put_ushort(600);
    writer.put_bytes(&wire_payload);
    writer.put_bool(true);
    let mut back = ChatMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.payload_size, ChatMessage::MAX_PAYLOAD_BYTES as u16);
    assert_eq!(back.payload, wire_payload[..ChatMessage::MAX_PAYLOAD_BYTES]);
    assert!(back.play_notification_sound);
}

#[test]
fn chat_message_truncated_payload_falls_back_to_empty() {
    let mut writer = NetDataWriter::new();
    writer.put_ushort(100);
    writer.put_bytes(&seeded_bytes(10, 105));
    let mut back = ChatMessage::default();
    let _ = back.deserialize(&mut reader_for(&writer));
    assert!(back.payload.is_empty());
    assert_eq!(back.payload_size, 0);
    assert!(back.play_notification_sound);
}

#[test]
fn chat_message_missing_sound_byte_defaults_to_true() {
    let payload = vec![10u8, 20, 30];
    let mut writer = NetDataWriter::new();
    writer.put_ushort(payload.len() as u16);
    writer.put_bytes(&payload);
    let mut back = ChatMessage::default();
    let _ = back.deserialize(&mut reader_for(&writer));
    assert_eq!(back.payload, payload);
    assert!(back.play_notification_sound);
}

// ── ClientMetaDataMessage / ServerMetaDataMessage ──

#[test]
fn client_meta_data_message_round_trips_unicode_fields() {
    let mut msg = ClientMetaDataMessage { player_uuid: "uuid-Ω-123".into(), player_display_name: "Ada 🚀 ラブレス".into(), player_platform: "OpenXR".into() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ClientMetaDataMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back, msg);
}

#[test]
fn client_meta_data_message_empty_fields_serialize_as_failure() {
    let mut msg = ClientMetaDataMessage::default();
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ClientMetaDataMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.player_uuid, "Failure");
    assert_eq!(back.player_display_name, "Failure");
    assert_eq!(back.player_platform, "Failure");
}

fn meta(uuid: &str, name: &str, platform: &str) -> ClientMetaDataMessage {
    ClientMetaDataMessage { player_uuid: uuid.into(), player_display_name: name.into(), player_platform: platform.into() }
}

#[test]
fn server_meta_data_message_round_trips_all_fields() {
    let mut msg = ServerMetaDataMessage {
        client_meta_data_message: meta("uuid-42", "Tester 猫", "Desktop"),
        sync_interval: 33,
        base_multiplier: 4,
        increase_rate: 0.25,
        slowest_send_rate: 1.75,
        peer_limit: 128,
        uplink_delta_enabled: true,
        image_share_egress_megabits_per_second: 321,
        image_pickup_range_meters: 72.5,
        ..Default::default()
    };
    msg.set_permissions(&strings(&["basis.moderation", "basis.moderation.kick", "custom.perm.alpha", "custom.perm.beta"]), None);

    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ServerMetaDataMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");

    assert_eq!(back.client_meta_data_message, msg.client_meta_data_message);
    assert_eq!(back.sync_interval, 33);
    assert_eq!(back.base_multiplier, 4);
    assert_eq!(back.increase_rate, 0.25);
    assert_eq!(back.slowest_send_rate, 1.75);
    assert_eq!(back.peer_limit, 128);
    assert!(back.uplink_delta_enabled);
    assert_eq!(back.image_share_egress_megabits_per_second, 321);
    assert_eq!(back.image_pickup_range_meters, 72.5);
    assert_eq!(back.permissions_bitset, msg.permissions_bitset);
    assert_eq!(back.extra_permissions, strings(&["custom.perm.alpha", "custom.perm.beta"]));
    let expected: HashSet<String> = strings(&["basis.moderation", "basis.moderation.kick", "custom.perm.alpha", "custom.perm.beta"]).into_iter().collect();
    assert_eq!(back.get_permissions(), expected);
}

#[test]
fn server_meta_data_message_zero_tuning_values_serialize_as_defaults() {
    let mut msg = ServerMetaDataMessage { client_meta_data_message: meta("u", "n", "p"), sync_interval: 0, base_multiplier: 0, increase_rate: 0.0, slowest_send_rate: 0.0, peer_limit: 32, ..Default::default() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ServerMetaDataMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.sync_interval, 50);
    assert_eq!(back.base_multiplier, 1);
    assert_eq!(back.increase_rate, 0.005);
    assert_eq!(back.slowest_send_rate, 2.55);
    assert_eq!(back.peer_limit, 32);
}

#[test]
fn server_meta_data_message_empty_permissions_round_trip() {
    let mut msg = ServerMetaDataMessage { client_meta_data_message: meta("u", "n", "p"), sync_interval: 50, base_multiplier: 1, increase_rate: 0.005, slowest_send_rate: 2.55, peer_limit: 8, uplink_delta_enabled: false, ..Default::default() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ServerMetaDataMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert!(back.permissions_bitset.is_empty());
    assert!(back.extra_permissions.is_empty());
    assert!(back.get_permissions().is_empty());
    assert!(!back.uplink_delta_enabled);
}

#[test]
fn server_meta_data_message_wildcard_permission_expands_to_all_known_nodes() {
    let mut msg = ServerMetaDataMessage { client_meta_data_message: meta("u", "n", "p"), sync_interval: 50, base_multiplier: 1, increase_rate: 0.005, slowest_send_rate: 2.55, peer_limit: 8, ..Default::default() };
    msg.set_permissions(&strings(&["*"]), None);
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ServerMetaDataMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    let perms = back.get_permissions();
    assert_eq!(perms.len(), PermissionBitsetMap::known_count());
    assert!(perms.contains("*"));
    assert!(perms.contains("basis.server.stats"));
    assert!(perms.contains("basis.moderation.headlessaudio"));
}

#[test]
fn server_meta_data_message_double_round_trip_is_byte_identical() {
    let mut msg = ServerMetaDataMessage { client_meta_data_message: meta("uuid-7", "Name", "Android"), sync_interval: 66, base_multiplier: 2, increase_rate: 0.5, slowest_send_rate: 3.5, peer_limit: 64, uplink_delta_enabled: true, ..Default::default() };
    msg.set_permissions(&strings(&["basis.protection", "custom.node.one"]), None);
    let mut first = NetDataWriter::new();
    msg.serialize(&mut first).expect("serialize");
    let mut back = ServerMetaDataMessage::default();
    back.deserialize(&mut reader_for(&first)).expect("deserialize");
    let mut second = NetDataWriter::new();
    back.serialize(&mut second).expect("serialize");
    assert_eq!(first.copy_data(), second.copy_data());
}

#[test]
fn server_meta_data_message_truncated_after_metadata_is_an_error() {
    let mut writer = NetDataWriter::new();
    meta("u", "n", "p").serialize(&mut writer).expect("meta");
    writer.put_int(50);
    let mut back = ServerMetaDataMessage::default();
    assert!(back.deserialize(&mut reader_for(&writer)).is_err());
}

// ── ConsoleData ──

#[test]
fn console_data_round_trips_payload() {
    let mut msg = ConsoleData { message_index: 200, array: Some(seeded_bytes(40, 106)) };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 1 + 2 + 40);
    let mut back = ConsoleData::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.message_index, 200);
    assert_eq!(back.array, msg.array);
}

#[test]
fn console_data_no_array_round_trips_to_empty() {
    let mut msg = ConsoleData { message_index: 5, array: None };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 3);
    let mut back = ConsoleData::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.message_index, 5);
    assert_eq!(back.array.as_deref().map(<[u8]>::len), Some(0));
}

#[test]
fn console_data_declared_payload_beyond_available_yields_empty() {
    let mut writer = NetDataWriter::new();
    writer.put_byte(9);
    writer.put_ushort(100);
    writer.put_bytes(&seeded_bytes(3, 107));
    let mut back = ConsoleData::default();
    let _ = back.deserialize(&mut reader_for(&writer));
    assert_eq!(back.message_index, 9);
    assert!(back.array.as_deref().is_none_or(<[u8]>::is_empty));
}

#[test]
fn console_data_empty_reader_is_an_error_without_panicking() {
    let mut back = ConsoleData::default();
    assert!(back.deserialize(&mut empty_reader()).is_err());
    assert_eq!(back.message_index, 0);
    assert!(back.array.is_none());
}

// ── ErrorMessage ──

#[test]
fn error_message_round_trips_unicode() {
    let mut msg = ErrorMessage { message: "błąd: 接続に失敗しました ⚠".into() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ErrorMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.message, msg.message);
}

#[test]
fn error_message_long_message_round_trips() {
    let long_message = format!("{}終", "x".repeat(10_000));
    let mut msg = ErrorMessage { message: long_message.clone() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ErrorMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.message, long_message);
}

#[test]
fn error_message_empty_message_round_trips_as_empty() {
    let mut msg = ErrorMessage { message: String::new() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 2);
    let mut back = ErrorMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.message, "");
}

#[test]
fn error_message_empty_reader_is_an_error_without_panicking() {
    let mut back = ErrorMessage::default();
    assert!(back.deserialize(&mut empty_reader()).is_err());
    assert_eq!(back.message, "");
}

// ── ModifyResource ──

#[test]
fn modify_resource_round_trips_all_fields() {
    for (is_static, admin_locked) in [(false, false), (true, false), (false, true), (true, true)] {
        let mut msg = ModifyResource { loaded_net_id: "net-α-ボール".into(), mode: 1, r#static: is_static, static_admin_locked: admin_locked };
        let mut writer = NetDataWriter::new();
        msg.serialize(&mut writer).expect("serialize");
        let mut back = ModifyResource::default();
        back.deserialize(&mut reader_for(&writer)).expect("deserialize");
        assert_eq!(back, msg);
    }
}

#[test]
fn modify_resource_double_round_trip_is_byte_identical() {
    let mut msg = ModifyResource { loaded_net_id: "prop-99".into(), mode: 0, r#static: true, static_admin_locked: true };
    let mut first = NetDataWriter::new();
    msg.serialize(&mut first).expect("serialize");
    let mut back = ModifyResource::default();
    back.deserialize(&mut reader_for(&first)).expect("deserialize");
    let mut second = NetDataWriter::new();
    back.serialize(&mut second).expect("serialize");
    assert_eq!(first.copy_data(), second.copy_data());
}

// ── NetIDMessage ──

#[test]
fn net_id_message_round_trips_player_id() {
    let mut msg = NetIDMessage { player_id: "プレイヤー-42-Ø".into() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = NetIDMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.player_id, msg.player_id);
}

#[test]
fn net_id_message_256_character_id_round_trips_exactly() {
    let id = "a".repeat(256);
    let mut msg = NetIDMessage { player_id: id.clone() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = NetIDMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.player_id, id);
}

#[test]
fn net_id_message_id_longer_than_256_chars_reads_as_empty() {
    let mut msg = NetIDMessage { player_id: "b".repeat(300) };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = NetIDMessage::default();
    let _ = back.deserialize(&mut reader_for(&writer));
    assert_eq!(back.player_id, "");
}

#[test]
fn net_id_message_serialize_empty_writes_nothing() {
    let mut writer = NetDataWriter::new();
    NetIDMessage { player_id: String::new() }.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 0);
}

#[test]
fn net_id_message_empty_reader_is_an_error_without_panicking() {
    let mut back = NetIDMessage::default();
    assert!(back.deserialize(&mut empty_reader()).is_err());
    assert_eq!(back.player_id, "");
}

// ── OwnershipTransferMessage ──

#[test]
fn ownership_transfer_message_round_trips() {
    let mut msg = OwnershipTransferMessage { player_id_message: PlayerIdMessage { player_id: u16::MAX }, ownership_id: "владелец-Ω/pickup_01".into() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = OwnershipTransferMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back, msg);
}

#[test]
fn ownership_transfer_message_ownership_id_over_256_chars_reads_as_empty() {
    let mut msg = OwnershipTransferMessage { player_id_message: PlayerIdMessage { player_id: 3 }, ownership_id: "o".repeat(300) };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = OwnershipTransferMessage::default();
    let _ = back.deserialize(&mut reader_for(&writer));
    assert_eq!(back.player_id_message.player_id, 3);
    assert_eq!(back.ownership_id, "");
}

// ── PlayerIdMessage ──

#[test]
fn player_id_message_round_trips_ushort() {
    for id in [0u16, 1, 255, 256, u16::MAX] {
        let msg = PlayerIdMessage { player_id: id };
        let mut writer = NetDataWriter::new();
        msg.serialize(&mut writer).expect("serialize");
        assert_eq!(writer.length(), 2);
        let mut back = PlayerIdMessage::default();
        back.deserialize(&mut reader_for(&writer)).expect("deserialize");
        assert_eq!(back.player_id, id);
    }
}

#[test]
fn player_id_message_large_id_flag_controls_wire_width() {
    let small = PlayerIdMessage { player_id: 200 };
    let mut byte_writer = NetDataWriter::new();
    small.serialize_sized(&mut byte_writer, false).expect("serialize");
    assert_eq!(byte_writer.length(), 1);
    let mut back_small = PlayerIdMessage::default();
    back_small.deserialize_sized(&mut reader_for(&byte_writer), false).expect("deserialize");
    assert_eq!(back_small.player_id, 200);

    let large = PlayerIdMessage { player_id: u16::MAX };
    let mut ushort_writer = NetDataWriter::new();
    large.serialize_sized(&mut ushort_writer, true).expect("serialize");
    assert_eq!(ushort_writer.length(), 2);
    let mut back_large = PlayerIdMessage::default();
    back_large.deserialize_sized(&mut reader_for(&ushort_writer), true).expect("deserialize");
    assert_eq!(back_large.player_id, u16::MAX);
}

/// A byte-wide id cannot carry a large player id; the writer must refuse rather than truncate.
#[test]
fn player_id_message_large_id_on_the_byte_path_is_refused() {
    let mut writer = NetDataWriter::new();
    assert!(PlayerIdMessage { player_id: 300 }.serialize_sized(&mut writer, false).is_err());
}

// ── ReadyMessage / ServerReadyMessage ──

fn make_ready_message(quality: BitQuality, seed: u64) -> ReadyMessage {
    let payload_size = BasisAvatarBitPacking::convert_to_size(quality);
    ReadyMessage {
        player_meta_data_message: meta("uuid-πλ-9000", "Réady Player 一", "Desktop"),
        client_avatar_change_message: ClientAvatarChangeMessage { load_mode: 1, byte_array: Some(seeded_bytes(48, seed + 1)), local_avatar_index: 250, ..Default::default() },
        local_avatar_sync_message: LocalAvatarSyncMessage { data_quality_level: quality as u8, array: Some(seeded_bytes(payload_size, seed)), ..Default::default() },
    }
}

#[test]
fn ready_message_deep_round_trips_across_all_qualities() {
    for quality in BitQuality::ALL {
        let mut msg = make_ready_message(quality, 200 + quality as u64);
        let mut writer = NetDataWriter::new();
        msg.serialize(&mut writer).expect("serialize");
        let mut back = ReadyMessage::default();
        back.deserialize(&mut reader_for(&writer)).expect("deserialize");
        assert!(back.was_deserialized_correctly());
        assert_eq!(back.player_meta_data_message, msg.player_meta_data_message);
        assert_eq!(back.client_avatar_change_message.load_mode, 1);
        assert_eq!(back.client_avatar_change_message.byte_array, msg.client_avatar_change_message.byte_array);
        assert_eq!(back.client_avatar_change_message.local_avatar_index, 250);
        assert_eq!(back.local_avatar_sync_message.data_quality_level, quality as u8);
        assert_eq!(back.local_avatar_sync_message.array, msg.local_avatar_sync_message.array);
        assert!(back.local_avatar_sync_message.additional_avatar_datas.is_none());
    }
}

#[test]
fn ready_message_no_avatar_change_bytes_fails_was_deserialized_correctly() {
    let mut msg = make_ready_message(BitQuality::High, 210);
    msg.client_avatar_change_message.byte_array = None;
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ReadyMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert!(back.client_avatar_change_message.byte_array.is_none());
    assert!(!back.was_deserialized_correctly());
}

#[test]
fn ready_message_double_round_trip_is_byte_identical() {
    let mut msg = make_ready_message(BitQuality::High, 220);
    let mut first = NetDataWriter::new();
    msg.serialize(&mut first).expect("serialize");
    let mut back = ReadyMessage::default();
    back.deserialize(&mut reader_for(&first)).expect("deserialize");
    let mut second = NetDataWriter::new();
    back.serialize(&mut second).expect("serialize");
    assert_eq!(first.copy_data(), second.copy_data());
}

#[test]
fn ready_message_truncated_mid_avatar_is_an_error() {
    let mut msg = make_ready_message(BitQuality::Low, 221);
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let full = writer.copy_data();
    let mut back = ReadyMessage::default();
    assert!(back.deserialize(&mut NetDataReader::from_slice(&full[..full.len() / 2])).is_err());
    assert!(!back.was_deserialized_correctly());
}

#[test]
fn server_ready_message_deep_round_trip_preserves_everything() {
    let mut msg = ServerReadyMessage { player_id_message: PlayerIdMessage { player_id: 4242 }, local_ready_message: make_ready_message(BitQuality::Medium, 230) };
    let mut first = NetDataWriter::new();
    msg.serialize(&mut first).expect("serialize");
    let mut back = ServerReadyMessage::default();
    back.deserialize(&mut reader_for(&first)).expect("deserialize");
    assert_eq!(back.player_id_message.player_id, 4242);
    assert!(back.local_ready_message.was_deserialized_correctly());
    assert_eq!(back.local_ready_message.player_meta_data_message.player_uuid, msg.local_ready_message.player_meta_data_message.player_uuid);
    assert_eq!(back.local_ready_message.client_avatar_change_message.byte_array, msg.local_ready_message.client_avatar_change_message.byte_array);
    assert_eq!(back.local_ready_message.local_avatar_sync_message.array, msg.local_ready_message.local_avatar_sync_message.array);
    assert_eq!(back.local_ready_message.local_avatar_sync_message.data_quality_level, BitQuality::Medium as u8);
    let mut second = NetDataWriter::new();
    back.serialize(&mut second).expect("serialize");
    assert_eq!(first.copy_data(), second.copy_data());
}

// ── LocalLoadResource / PreloadReadyMessage / SpawnPreloadedMessage ──

#[test]
fn local_load_resource_game_object_mode_round_trips_all_fields() {
    let mut msg = LocalLoadResource {
        mode: 0,
        loaded_net_id: "net-α".into(),
        unlock_password: "pässwörd".into(),
        combined_url: "https://example.com/bundle#雪".into(),
        uuid_of_creator: "creator-9".into(),
        is_admin_locked: true,
        persist: true,
        r#static: true,
        static_admin_locked: true,
        modify_scale: true,
        load_strategy: 3,
        position_x: 1.5,
        position_y: -2.25,
        position_z: 3.125,
        quaternion_x: -0.5,
        quaternion_y: 0.25,
        quaternion_z: -0.125,
        quaternion_w: 0.875,
        scale_x: 2.0,
        scale_y: 0.5,
        scale_z: 4.0,
    };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = LocalLoadResource::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back, msg);
}

#[test]
fn local_load_resource_scene_mode_omits_transform() {
    let mut msg = LocalLoadResource { mode: 1, loaded_net_id: "scene-1".into(), unlock_password: String::new(), combined_url: "https://example.com/world".into(), uuid_of_creator: "creator".into(), persist: true, load_strategy: 2, position_x: 123.0, scale_z: 456.0, ..Default::default() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = LocalLoadResource::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.mode, 1);
    assert_eq!(back.loaded_net_id, "scene-1");
    assert_eq!(back.unlock_password, "");
    assert_eq!(back.combined_url, "https://example.com/world");
    assert!(back.persist);
    assert_eq!(back.load_strategy, 2);
    assert_eq!(back.position_x, 0.0);
    assert_eq!(back.quaternion_w, 0.0);
    assert_eq!(back.scale_z, 0.0);
}

#[test]
fn local_load_resource_double_round_trip_is_byte_identical() {
    let mut msg = LocalLoadResource {
        mode: 0,
        loaded_net_id: "net-β".into(),
        unlock_password: "pw".into(),
        combined_url: "https://a/b".into(),
        uuid_of_creator: "c".into(),
        is_admin_locked: true,
        persist: false,
        r#static: true,
        static_admin_locked: false,
        modify_scale: true,
        load_strategy: 0,
        position_x: 1.0,
        position_y: 2.0,
        position_z: 3.0,
        quaternion_x: 4.0,
        quaternion_y: 5.0,
        quaternion_z: 6.0,
        quaternion_w: 7.0,
        scale_x: 8.0,
        scale_y: 9.0,
        scale_z: 10.0,
    };
    let mut first = NetDataWriter::new();
    msg.serialize(&mut first).expect("serialize");
    let mut back = LocalLoadResource::default();
    back.deserialize(&mut reader_for(&first)).expect("deserialize");
    let mut second = NetDataWriter::new();
    back.serialize(&mut second).expect("serialize");
    assert_eq!(first.copy_data(), second.copy_data());
}

#[test]
fn local_load_resource_truncated_transform_is_an_error() {
    let mut msg = LocalLoadResource { mode: 0, loaded_net_id: "net".into(), combined_url: "u".into(), uuid_of_creator: "c".into(), ..Default::default() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let full = writer.copy_data();
    let mut back = LocalLoadResource::default();
    assert!(back.deserialize(&mut NetDataReader::from_slice(&full[..full.len() - 5])).is_err());
}

#[test]
fn preload_ready_message_round_trips() {
    for is_ready in [true, false] {
        let mut msg = PreloadReadyMessage { loaded_net_id: "preload-µ".into(), is_ready };
        let mut writer = NetDataWriter::new();
        msg.serialize(&mut writer).expect("serialize");
        let mut back = PreloadReadyMessage::default();
        back.deserialize(&mut reader_for(&writer)).expect("deserialize");
        assert_eq!(back, msg);
    }
}

#[test]
fn spawn_preloaded_message_round_trips() {
    let mut msg = SpawnPreloadedMessage { loaded_net_id: "spawn-λ-01".into() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = SpawnPreloadedMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.loaded_net_id, "spawn-λ-01");
}

// ── ServerChatMessage ──

#[test]
fn server_chat_message_deep_round_trip() {
    let payload = "hello from the sérver 🌐".as_bytes().to_vec();
    let mut msg = ServerChatMessage { player_id_message: PlayerIdMessage { player_id: u16::MAX }, chat_message: ChatMessage { payload: payload.clone(), play_notification_sound: false, ..Default::default() } };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ServerChatMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.player_id_message.player_id, u16::MAX);
    assert_eq!(back.chat_message.payload, payload);
    assert_eq!(back.chat_message.payload_size, payload.len() as u16);
    assert!(!back.chat_message.play_notification_sound);
}

// ── ServerLibraryItem / ServerLibraryMessage ──

#[test]
fn server_library_item_round_trips() {
    let mut item = ServerLibraryItem { mode: 2, url: "https://cdn/prop.bee#日本".into(), password: "s3cret-ß".into() };
    let mut writer = NetDataWriter::new();
    item.serialize(&mut writer).expect("serialize");
    let mut back = ServerLibraryItem::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back, item);
}

#[test]
fn server_library_item_empty_strings_serialize_as_empty() {
    let mut item = ServerLibraryItem { mode: 0, url: String::new(), password: String::new() };
    let mut writer = NetDataWriter::new();
    item.serialize(&mut writer).expect("serialize");
    let mut back = ServerLibraryItem::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.url, "");
    assert_eq!(back.password, "");
}

#[test]
fn server_library_message_round_trips_multiple_items() {
    let mut msg = ServerLibraryMessage {
        items: vec![
            ServerLibraryItem { mode: 0, url: "https://a/avatar".into(), password: String::new() },
            ServerLibraryItem { mode: 1, url: "https://b/world".into(), password: "秘密".into() },
            ServerLibraryItem { mode: 2, url: String::new(), password: "p2".into() },
        ],
    };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ServerLibraryMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.items, msg.items);
}

#[test]
fn server_library_message_no_items_round_trips_to_empty() {
    let mut msg = ServerLibraryMessage { items: Vec::new() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 2);
    let mut back = ServerLibraryMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert!(back.items.is_empty());
}

#[test]
fn server_library_message_overclaimed_count_is_an_error() {
    let mut writer = NetDataWriter::new();
    writer.put_ushort(40);
    writer.put_byte(1);
    let mut back = ServerLibraryMessage::default();
    assert!(back.deserialize(&mut reader_for(&writer)).is_err());
}

// ── ServerStatisticMessage ──

#[test]
fn server_statistic_message_round_trips_raw_bytes() {
    let mut msg = ServerStatisticMessage { data: seeded_bytes(33, 108) };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer);
    assert_eq!(writer.length(), 33);
    let mut back = ServerStatisticMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.data, msg.data);
}

#[test]
fn server_statistic_message_empty_data_round_trips() {
    let mut msg = ServerStatisticMessage { data: Vec::new() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer);
    assert_eq!(writer.length(), 0);
    let mut back = ServerStatisticMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert!(back.data.is_empty());
}

// ── ServerNetIDMessage / ServerUniqueIDMessages ──

#[test]
fn server_net_id_message_round_trips() {
    let mut msg = ServerNetIDMessage { net_id_message: NetIDMessage { player_id: "object-Ω".into() }, ushort_unique_id_message: UshortUniqueIDMessage { unique_id_ushort: u16::MAX } };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ServerNetIDMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back, msg);
}

fn net_id(id: &str, ushort: u16) -> ServerNetIDMessage {
    ServerNetIDMessage { net_id_message: NetIDMessage { player_id: id.into() }, ushort_unique_id_message: UshortUniqueIDMessage { unique_id_ushort: ushort } }
}

#[test]
fn server_unique_id_messages_round_trips_entries() {
    let entries = vec![net_id("alpha", 1), net_id("βeta", 32768), net_id("gamma-γ", u16::MAX)];
    let mut msg = ServerUniqueIDMessages { message_count: entries.len() as u16, messages: Some(entries.clone()) };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ServerUniqueIDMessages::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.message_count, 3);
    assert_eq!(back.messages, Some(entries));
}

#[test]
fn server_unique_id_messages_empty_array_round_trips() {
    let mut msg = ServerUniqueIDMessages { message_count: 0, messages: Some(Vec::new()) };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 2);
    let mut back = ServerUniqueIDMessages::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.message_count, 0);
    assert_eq!(back.messages.as_deref().map(<[ServerNetIDMessage]>::len), Some(0));
}

#[test]
fn server_unique_id_messages_serialize_no_messages_writes_nothing() {
    let mut msg = ServerUniqueIDMessages { message_count: 0, messages: None };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 0);
}

#[test]
fn server_unique_id_messages_truncated_reader_leaves_messages_none() {
    let mut from_empty = ServerUniqueIDMessages::default();
    let _ = from_empty.deserialize(&mut empty_reader());
    assert!(from_empty.messages.is_none());

    let mut from_one_byte = ServerUniqueIDMessages::default();
    let _ = from_one_byte.deserialize(&mut NetDataReader::from_slice(&[0x7F]));
    assert!(from_one_byte.messages.is_none());

    // A count that claims more entries than follow is a fault, not a partial list.
    let mut writer = NetDataWriter::new();
    writer.put_ushort(3);
    net_id("only", 1).serialize(&mut writer).expect("serialize");
    let mut short = ServerUniqueIDMessages::default();
    assert!(short.deserialize(&mut reader_for(&writer)).is_err());
}

// ── UnLoadResource ──

#[test]
fn unload_resource_round_trips_and_reports_success() {
    let mut msg = UnLoadResource { mode: 1, loaded_net_id: "unload-ζ".into() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = UnLoadResource::default();
    assert!(back.deserialize(&mut reader_for(&writer)));
    assert_eq!(back, msg);
}

#[test]
fn unload_resource_empty_reader_returns_false() {
    let mut back = UnLoadResource::default();
    assert!(!back.deserialize(&mut empty_reader()));
}

#[test]
fn unload_resource_truncated_string_returns_false() {
    let mut mode_only = NetDataWriter::new();
    mode_only.put_byte(1);
    let mut back_mode_only = UnLoadResource::default();
    assert!(!back_mode_only.deserialize(&mut reader_for(&mode_only)));

    let mut truncated = NetDataWriter::new();
    truncated.put_byte(1);
    truncated.put_ushort(50);
    let mut back_truncated = UnLoadResource::default();
    assert!(!back_truncated.deserialize(&mut reader_for(&truncated)));
    assert_eq!(back_truncated.loaded_net_id, "");
}

// ── UshortUniqueIDMessage ──

#[test]
fn ushort_unique_id_message_round_trips() {
    for id in [0u16, 1, u16::MAX] {
        let msg = UshortUniqueIDMessage { unique_id_ushort: id };
        let mut writer = NetDataWriter::new();
        msg.serialize(&mut writer).expect("serialize");
        assert_eq!(writer.length(), 2);
        let mut back = UshortUniqueIDMessage::default();
        back.deserialize(&mut reader_for(&writer)).expect("deserialize");
        assert_eq!(back.unique_id_ushort, id);
    }
}

#[test]
fn ushort_unique_id_message_empty_reader_is_an_error_without_panicking() {
    let mut back = UshortUniqueIDMessage::default();
    assert!(back.deserialize(&mut empty_reader()).is_err());
    assert_eq!(back.unique_id_ushort, 0);
}

// ── ContentShare messages ──

#[test]
fn content_share_message_round_trips_all_fields() {
    for content_type in [ContentShareType::Avatar, ContentShareType::Prop, ContentShareType::World, ContentShareType::Server] {
        let mut msg = ContentShareMessage { sphere_net_id: "sphere-β-42".into(), content_url: "https://example.com/bundle?v=1&q=日本".into(), unlock_password: "pässword".into(), content_type, position_x: -12.5, position_y: 0.03125, position_z: 4096.0 };
        let mut writer = NetDataWriter::new();
        msg.serialize(&mut writer).expect("serialize");
        let mut back = ContentShareMessage::default();
        back.deserialize(&mut reader_for(&writer)).expect("deserialize");
        assert_eq!(back, msg);
    }
}

#[test]
fn content_share_message_empty_strings_round_trip_as_empty() {
    let mut msg = ContentShareMessage { sphere_net_id: String::new(), content_url: "https://example.com".into(), unlock_password: String::new(), content_type: ContentShareType::Avatar, ..Default::default() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ContentShareMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.sphere_net_id, "");
    assert_eq!(back.content_url, "https://example.com");
    assert_eq!(back.unlock_password, "");
}

#[test]
fn server_content_share_message_deep_round_trip() {
    let mut msg = ServerContentShareMessage {
        player_id_message: PlayerIdMessage { player_id: u16::MAX },
        sharer_uuid: "sharer-uuid-δ".into(),
        sharer_display_name: "Sharer 分享".into(),
        content_share_message: ContentShareMessage { sphere_net_id: "sphere-1".into(), content_url: "https://cdn/av".into(), unlock_password: "pw".into(), content_type: ContentShareType::World, position_x: 1.0, position_y: 2.0, position_z: 3.0 },
    };
    let mut first = NetDataWriter::new();
    msg.serialize(&mut first).expect("serialize");
    let mut back = ServerContentShareMessage::default();
    back.deserialize(&mut reader_for(&first)).expect("deserialize");
    assert_eq!(back, msg);
    let mut second = NetDataWriter::new();
    back.serialize(&mut second).expect("serialize");
    assert_eq!(first.copy_data(), second.copy_data());
}

#[test]
fn server_content_share_message_empty_sharer_identity_serializes_as_empty() {
    let mut msg = ServerContentShareMessage {
        player_id_message: PlayerIdMessage { player_id: 7 },
        sharer_uuid: String::new(),
        sharer_display_name: String::new(),
        content_share_message: ContentShareMessage { sphere_net_id: "s".into(), content_url: "u".into(), unlock_password: "p".into(), content_type: ContentShareType::Prop, ..Default::default() },
    };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ServerContentShareMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.player_id_message.player_id, 7);
    assert_eq!(back.sharer_uuid, "");
    assert_eq!(back.sharer_display_name, "");
    assert_eq!(back.content_share_message.content_type, ContentShareType::Prop);
}

#[test]
fn content_share_cleanup_message_round_trips() {
    let mut msg = ContentShareCleanupMessage { sphere_net_id: "cleanup-ω".into() };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ContentShareCleanupMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back.sphere_net_id, "cleanup-ω");
}

#[test]
fn server_content_share_cleanup_message_deep_round_trip() {
    let mut msg = ServerContentShareCleanupMessage { player_id_message: PlayerIdMessage { player_id: 888 }, content_share_cleanup_message: ContentShareCleanupMessage { sphere_net_id: "sphere-χ".into() } };
    let mut writer = NetDataWriter::new();
    msg.serialize(&mut writer).expect("serialize");
    let mut back = ServerContentShareCleanupMessage::default();
    back.deserialize(&mut reader_for(&writer)).expect("deserialize");
    assert_eq!(back, msg);
}
