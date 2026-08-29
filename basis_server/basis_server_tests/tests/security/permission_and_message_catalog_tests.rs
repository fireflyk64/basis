//! The permission bitset map (append-only wire format), the deflate codec for the dynamic
//! permission strings, the full metadata wire path, the core message catalog, the registry
//! manifest structs, the channel constant layout, and the server-side message registry.

use std::collections::HashSet;
use std::sync::Arc;

use basis_network_core::BasisNetworkCommons as C;
use basis_network_core::SerializableBasis::{BasisMessageCatalog, BasisMessageDescriptor, BasisMessageFlags, BasisMessageSubscribe, BasisMessageSupply, ClientMetaDataMessage, PermissionBitsetMap, PermissionCompression, ServerMetaDataMessage};
use basis_network_core::transport::DeliveryMethod;
use basis_network_core::{NetDataReader, NetDataWriter};
use basis_network_server::messaging::basis_server_message_registry::{BasisServerMessageHandler, BasisServerMessageRegistry};
use basis_server_tests::support::delta_test_support::TestRng;

fn rewind(writer: &NetDataWriter) -> NetDataReader {
    NetDataReader::from_slice(writer.as_read_only_span())
}

fn truncated(writer: &NetDataWriter, size: usize) -> NetDataReader {
    NetDataReader::with_offset(writer.as_read_only_span().to_vec(), 0, size)
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}

// ── PermissionBitsetMapTests ──

// Hand-written pin table. Must mirror the map's index-to-node table exactly.
const KNOWN_NODES: [&str; 28] = [
    "*",
    "basis.server.stats",
    "basis.resource.load.world",
    "basis.resource.unload.world",
    "basis.resource.load.prop",
    "basis.resource.unload.prop",
    "basis.resource.load.avatar",
    "basis.resource.unload.avatar",
    "basis.ownership.transfer",
    "basis.ownership.remove",
    "basis.ownership.get",
    "basis.contentshare.delete",
    "basis.contentshare.create",
    "basis.protection",
    "basis.configuration",
    "basis.moderation",
    "basis.moderation.ban",
    "basis.moderation.kick",
    "basis.moderation.ipban",
    "basis.moderation.unban",
    "basis.moderation.unbanip",
    "basis.moderation.message",
    "basis.moderation.messageall",
    "basis.moderation.teleport",
    "basis.moderation.shout",
    "basis.permissions.view",
    "basis.permissions.edit",
    "basis.moderation.headlessaudio",
];

fn single_bit(index: usize) -> Vec<u8> {
    let mut bitset = vec![0u8; PermissionBitsetMap::byte_count()];
    bitset[index >> 3] |= 1 << (index & 7);
    bitset
}

fn encode(allowed: &[&str]) -> (Vec<u8>, Vec<String>) {
    PermissionBitsetMap::encode(&strings(allowed), None)
}

#[test]
fn known_count_and_byte_count_are_pinned() {
    assert_eq!(PermissionBitsetMap::known_count(), 28);
    assert_eq!(PermissionBitsetMap::byte_count(), 4);
    assert_eq!(KNOWN_NODES.len(), PermissionBitsetMap::known_count());
}

#[test]
fn encoding_single_node_sets_exactly_its_pinned_bit() {
    for (index, node) in KNOWN_NODES.iter().enumerate().skip(1) {
        let (bitset, extras) = encode(&[node]);
        assert!(extras.is_empty(), "{node}");
        assert_eq!(bitset, single_bit(index), "{node}");
    }
}

#[test]
fn decoding_single_bit_yields_exactly_its_pinned_node() {
    for (index, node) in KNOWN_NODES.iter().enumerate() {
        let decoded = PermissionBitsetMap::decode(&single_bit(index), &[]);
        assert_eq!(decoded.len(), 1, "{node}");
        assert!(decoded.contains(*node), "{node}");
    }
}

#[test]
fn wildcard_sets_every_known_bit_and_decodes_to_full_set() {
    let (bitset, extras) = encode(&["*"]);
    assert!(extras.is_empty());
    assert_eq!(bitset.len(), PermissionBitsetMap::byte_count());
    for i in 0..PermissionBitsetMap::known_count() {
        assert!((bitset[i >> 3] & (1 << (i & 7))) != 0, "bit {i} should be set by wildcard");
    }
    // Bits 28..31 of the last byte stay clear.
    assert_eq!(bitset[3] & 0xF0, 0);

    let decoded = PermissionBitsetMap::decode(&bitset, &extras);
    assert_eq!(decoded.len(), KNOWN_NODES.len());
    for node in KNOWN_NODES {
        assert!(decoded.contains(node), "{node}");
    }
}

#[test]
fn all_known_nodes_explicitly_round_trip_to_full_set() {
    let (bitset, extras) = encode(&KNOWN_NODES);
    assert!(extras.is_empty());
    assert_eq!(PermissionBitsetMap::decode(&bitset, &extras).len(), KNOWN_NODES.len());
}

#[test]
fn empty_allowed_list_produces_all_clear_bitset_and_empty_decode() {
    let (bitset, extras) = encode(&[]);
    assert_eq!(bitset, vec![0u8; PermissionBitsetMap::byte_count()]);
    assert!(extras.is_empty());
    assert!(PermissionBitsetMap::decode(&bitset, &extras).is_empty());
}

#[test]
fn each_non_wildcard_node_round_trips_alone() {
    for node in KNOWN_NODES.iter().skip(1) {
        let (bitset, extras) = encode(&[node]);
        let decoded = PermissionBitsetMap::decode(&bitset, &extras);
        assert_eq!(decoded.len(), 1);
        assert!(decoded.contains(*node));
    }
}

#[test]
fn exhaustive_subsets_of_bits_1_through_12_round_trip_exactly() {
    const WIDTH: usize = 12;
    for mask in 0..(1usize << WIDTH) {
        let mut allowed = Vec::new();
        let mut expected = vec![0u8; PermissionBitsetMap::byte_count()];
        for bit in 0..WIDTH {
            if mask & (1 << bit) == 0 {
                continue;
            }
            let index = bit + 1; // skip the wildcard slot
            allowed.push(KNOWN_NODES[index].to_string());
            expected[index >> 3] |= 1 << (index & 7);
        }
        let (bitset, extras) = PermissionBitsetMap::encode(&allowed, None);
        assert!(extras.is_empty());
        assert_eq!(bitset, expected, "mask {mask:#x}");
        let decoded = PermissionBitsetMap::decode(&bitset, &extras);
        assert_eq!(decoded.len(), allowed.len());
        for node in &allowed {
            assert!(decoded.contains(node));
        }
    }
}

#[test]
fn seeded_random_subsets_of_all_non_wildcard_nodes_round_trip() {
    let mut rng = TestRng::new(4242);
    for _ in 0..300 {
        let allowed: Vec<String> = KNOWN_NODES.iter().skip(1).filter(|_| rng.next(2) == 0).map(|s| s.to_string()).collect();
        let (bitset, extras) = PermissionBitsetMap::encode(&allowed, None);
        assert!(extras.is_empty());
        let decoded = PermissionBitsetMap::decode(&bitset, &extras);
        assert_eq!(decoded.len(), allowed.len());
        for node in &allowed {
            assert!(decoded.contains(node));
        }
    }
}

#[test]
fn unknown_nodes_become_extras_in_input_order() {
    let allowed = ["basis.moderation", "com.acme.custom.a", "basis.protection", "com.acme.custom.b"];
    let (bitset, extras) = encode(&allowed);
    assert_eq!(extras, strings(&["com.acme.custom.a", "com.acme.custom.b"]));

    let mut expected = vec![0u8; PermissionBitsetMap::byte_count()];
    expected[15 >> 3] |= 1 << (15 & 7); // basis.moderation
    expected[13 >> 3] |= 1 << (13 & 7); // basis.protection
    assert_eq!(bitset, expected);

    let decoded = PermissionBitsetMap::decode(&bitset, &extras);
    assert_eq!(decoded.len(), 4);
    for node in allowed {
        assert!(decoded.contains(node));
    }
}

#[test]
fn known_node_lookup_is_case_insensitive_and_decodes_canonical_casing() {
    let (bitset, extras) = encode(&["BASIS.MODERATION.KICK"]);
    assert!(extras.is_empty());
    assert_eq!(bitset, single_bit(17));
    let decoded = PermissionBitsetMap::decode(&bitset, &extras);
    assert_eq!(decoded.len(), 1);
    assert!(decoded.contains("basis.moderation.kick"));
}

#[test]
fn denied_nodes_clear_their_bits_from_wildcard_grant() {
    let (bitset, extras) = PermissionBitsetMap::encode(&strings(&["*"]), Some(&strings(&["basis.moderation.ban", "basis.moderation.kick"])));
    assert!(extras.is_empty());
    assert_eq!(bitset[16 >> 3] & (1 << (16 & 7)), 0);
    assert_eq!(bitset[17 >> 3] & (1 << (17 & 7)), 0);

    let decoded = PermissionBitsetMap::decode(&bitset, &extras);
    assert_eq!(decoded.len(), KNOWN_NODES.len() - 2);
    assert!(!decoded.contains("basis.moderation.ban"));
    assert!(!decoded.contains("basis.moderation.kick"));
}

#[test]
fn denied_unknown_nodes_are_ignored() {
    let (bitset, extras) = PermissionBitsetMap::encode(&strings(&["basis.moderation"]), Some(&strings(&["com.unknown.node"])));
    assert!(extras.is_empty());
    assert_eq!(bitset, single_bit(15));
}

#[test]
fn denying_wildcard_only_clears_bit_zero() {
    let (bitset, extras) = PermissionBitsetMap::encode(&strings(&["*"]), Some(&strings(&["*"])));
    assert!(extras.is_empty());
    assert_eq!(bitset[0] & 1, 0);
    let decoded = PermissionBitsetMap::decode(&bitset, &extras);
    assert_eq!(decoded.len(), KNOWN_NODES.len() - 1);
    assert!(!decoded.contains("*"));
}

#[test]
fn decode_ignores_bits_above_known_count() {
    assert_eq!(PermissionBitsetMap::decode(&[0xFF, 0xFF, 0xFF, 0xFF], &[]).len(), KNOWN_NODES.len());
    assert_eq!(PermissionBitsetMap::decode(&[0xFF; 8], &[]).len(), KNOWN_NODES.len());
}

#[test]
fn decode_tolerates_short_and_empty_inputs() {
    let decoded = PermissionBitsetMap::decode(&[0b1000_0001], &[]);
    assert_eq!(decoded.len(), 2);
    assert!(decoded.contains(KNOWN_NODES[0]));
    assert!(decoded.contains(KNOWN_NODES[7]));

    assert!(PermissionBitsetMap::decode(&[], &[]).is_empty());
    let only_extra = PermissionBitsetMap::decode(&[], &strings(&["com.only.extra"]));
    assert_eq!(only_extra.len(), 1);
    assert!(only_extra.contains("com.only.extra"));
    let bit3 = PermissionBitsetMap::decode(&single_bit(3), &[]);
    assert_eq!(bit3.len(), 1);
    assert!(bit3.contains(KNOWN_NODES[3]));
}

// ── PermissionCompressionTests: [flag byte 0=raw 1=deflate][payload of NUL-joined UTF8] ──

#[test]
fn compression_round_trip_representative_arrays() {
    let cases: Vec<Vec<String>> = vec![
        strings(&["a"]),
        strings(&["basis.custom.one", "basis.custom.two"]),
        strings(&["com.acme.plugin.permission"]),
        strings(&["first", "", "third"]),
        strings(&[""]),
        strings(&["日本語のノード", "ünïcode.pérm", "emoji.\u{1F600}.node"]),
    ];
    for case in cases {
        let payload = PermissionCompression::compress_extras(&case);
        assert_eq!(PermissionCompression::decompress_extras(&payload, case.len()), case);
    }
}

#[test]
fn compression_round_trip_seeded_random_strings() {
    let mut rng = TestRng::new(9001);
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz.-_0123456789";
    let values: Vec<String> = (0..100)
        .map(|_| {
            let length = 1 + rng.next(30);
            (0..length).map(|_| ALPHABET[rng.next(ALPHABET.len())] as char).collect()
        })
        .collect();
    let payload = PermissionCompression::compress_extras(&values);
    assert_eq!(PermissionCompression::decompress_extras(&payload, values.len()), values);
}

#[test]
fn compression_empty_input_compresses_to_empty_payload() {
    assert!(PermissionCompression::compress_extras(&[]).is_empty());
}

#[test]
fn decompress_empty_or_zero_count_returns_empty() {
    assert!(PermissionCompression::decompress_extras(&[], 3).is_empty());
    let valid = PermissionCompression::compress_extras(&strings(&["x", "y"]));
    assert!(PermissionCompression::decompress_extras(&valid, 0).is_empty());
}

#[test]
fn tiny_input_uses_raw_flag_with_pinned_size() {
    let payload = PermissionCompression::compress_extras(&strings(&["a"]));
    assert_eq!(payload, vec![0, b'a']);
}

#[test]
fn repetitive_input_uses_deflate_flag_and_shrinks() {
    let values = vec!["basis.custom.permission.node.repeated".to_string(); 64];
    let raw_length = values.join("\0").len();
    let payload = PermissionCompression::compress_extras(&values);
    assert_eq!(payload[0], 1);
    assert!(payload.len() < raw_length + 1, "deflate payload {} should undercut raw {}", payload.len(), raw_length + 1);
    assert_eq!(PermissionCompression::decompress_extras(&payload, values.len()), values);
}

#[test]
fn payload_never_exceeds_raw_size_plus_flag_byte() {
    let inputs: Vec<Vec<String>> = vec![strings(&["a"]), strings(&["z", "y", "x"]), strings(&["com.acme.one", "com.acme.two", "com.acme.three"]), vec!["q".repeat(500)]];
    for values in inputs {
        let raw_length = values.join("\0").len();
        let payload = PermissionCompression::compress_extras(&values);
        assert!(payload.len() <= raw_length + 1, "payload {} exceeds raw {raw_length} + 1 flag byte", payload.len());
    }
}

#[test]
fn expected_count_is_advisory_only_zero_short_circuits() {
    let payload = PermissionCompression::compress_extras(&strings(&["x", "y"]));
    assert_eq!(PermissionCompression::decompress_extras(&payload, 5), strings(&["x", "y"]));
    assert_eq!(PermissionCompression::decompress_extras(&payload, 1), strings(&["x", "y"]));
}

#[test]
fn decompress_guard_rejects_payloads_over_one_mib() {
    let oversized = vec!["a".repeat(1024 * 1024 + 1)];
    let payload = PermissionCompression::compress_extras(&oversized);
    assert_eq!(payload[0], 1);
    assert!(PermissionCompression::decompress_extras(&payload, 1).is_empty());
}

#[test]
fn decompress_guard_allows_exactly_one_mib() {
    let boundary = vec!["a".repeat(1024 * 1024)];
    let payload = PermissionCompression::compress_extras(&boundary);
    assert_eq!(PermissionCompression::decompress_extras(&payload, 1), boundary);
}

/// A deflate payload that is not deflate at all must come back empty, never panic. Any flag
/// other than 1 is read as raw, which is the C# contract.
#[test]
fn decompress_corrupt_deflate_payload_returns_empty() {
    assert!(PermissionCompression::decompress_extras(&[1, 0xFF, 0xFE, 0xFD, 0x00, 0x11], 3).is_empty());
    assert!(PermissionCompression::decompress_extras(&[1], 3).is_empty());
    assert_eq!(PermissionCompression::decompress_extras(&[7, b'a'], 1), vec!["a".to_string()]);
}

// ── PermissionMetaDataWireTests ──

fn make_message() -> ServerMetaDataMessage {
    ServerMetaDataMessage {
        client_meta_data_message: ClientMetaDataMessage { player_uuid: "uuid-1234-abcd".into(), player_display_name: "Wire Tester".into(), player_platform: "OpenXR".into() },
        sync_interval: 50,
        base_multiplier: 1,
        increase_rate: 0.005,
        slowest_send_rate: 2.55,
        peer_limit: 128,
        uplink_delta_enabled: true,
        ..Default::default()
    }
}

fn round_trip(source: &ServerMetaDataMessage) -> ServerMetaDataMessage {
    let mut writer = NetDataWriter::new();
    source.clone().serialize(&mut writer).expect("serialize");
    let mut reader = rewind(&writer);
    let mut decoded = ServerMetaDataMessage::default();
    decoded.deserialize(&mut reader).expect("deserialize");
    assert!(reader.end_of_data(), "deserialize should consume the full payload");
    decoded
}

fn lower(set: &HashSet<String>) -> HashSet<String> {
    set.iter().map(|s| s.to_lowercase()).collect()
}

#[test]
fn known_and_unknown_nodes_round_trip_through_meta_data_message() {
    let allowed = strings(&["basis.server.stats", "basis.moderation.kick", "com.acme.plugin.alpha", "com.acme.plugin.beta"]);
    let mut source = make_message();
    source.set_permissions(&allowed, None);

    let decoded = round_trip(&source);

    assert_eq!(decoded.client_meta_data_message.player_uuid, source.client_meta_data_message.player_uuid);
    assert_eq!(decoded.client_meta_data_message.player_display_name, source.client_meta_data_message.player_display_name);
    assert_eq!(decoded.client_meta_data_message.player_platform, source.client_meta_data_message.player_platform);
    assert_eq!(decoded.sync_interval, source.sync_interval);
    assert_eq!(decoded.base_multiplier, source.base_multiplier);
    assert_eq!(decoded.increase_rate, source.increase_rate);
    assert_eq!(decoded.slowest_send_rate, source.slowest_send_rate);
    assert_eq!(decoded.peer_limit, source.peer_limit);
    assert!(decoded.uplink_delta_enabled);

    assert_eq!(decoded.permissions_bitset, source.permissions_bitset);
    assert_eq!(decoded.extra_permissions, strings(&["com.acme.plugin.alpha", "com.acme.plugin.beta"]));
    assert_eq!(lower(&decoded.get_permissions()), lower(&allowed.iter().cloned().collect()));
}

#[test]
fn wildcard_round_trips_to_full_known_set() {
    let mut source = make_message();
    source.set_permissions(&strings(&["*"]), None);
    let decoded = round_trip(&source);
    assert_eq!(lower(&decoded.get_permissions()), lower(&KNOWN_NODES.iter().map(|s| s.to_string()).collect()));
    assert!(decoded.extra_permissions.is_empty());
}

#[test]
fn no_permissions_round_trip_empty_and_uplink_false_survives() {
    let mut source = make_message();
    source.uplink_delta_enabled = false;
    source.set_permissions(&[], None);
    let decoded = round_trip(&source);
    assert!(decoded.get_permissions().is_empty());
    assert!(decoded.extra_permissions.is_empty());
    assert!(!decoded.uplink_delta_enabled);
}

#[test]
fn many_extras_survive_the_deflate_wire_path() {
    let allowed: Vec<String> = (0..40).map(|i| format!("com.acme.plugin.permission.{i:03}")).collect();
    let mut source = make_message();
    source.set_permissions(&allowed, None);
    let decoded = round_trip(&source);
    assert_eq!(decoded.extra_permissions, allowed);
    assert_eq!(lower(&decoded.get_permissions()), lower(&allowed.iter().cloned().collect()));
}

#[test]
fn legacy_stream_without_permission_block_yields_empty_permissions() {
    let mut writer = NetDataWriter::new();
    ClientMetaDataMessage { player_uuid: "legacy-uuid".into(), player_display_name: "Legacy".into(), player_platform: "Desktop".into() }.serialize(&mut writer).expect("meta");
    writer.put_int(50);
    writer.put_int(1);
    writer.put_float(0.005);
    writer.put_float(2.55);
    writer.put_int(32);

    let mut decoded = ServerMetaDataMessage::default();
    decoded.deserialize(&mut rewind(&writer)).expect("legacy deserialize");

    assert_eq!(decoded.sync_interval, 50);
    assert_eq!(decoded.peer_limit, 32);
    assert!(decoded.permissions_bitset.is_empty());
    assert!(decoded.extra_permissions.is_empty());
    assert!(!decoded.uplink_delta_enabled);
    assert_eq!(decoded.image_pickup_range_meters, 0.0);
    assert!(decoded.get_permissions().is_empty());
}

// ── MessageCatalogTests ──

const PINNED_CORE_CATALOG: [(u8, &str); 59] = [
    (0, "basis.core.auth.identity"),
    (1, "basis.core.metadata"),
    (2, "basis.core.disconnection"),
    (3, "basis.core.voice"),
    (4, "basis.core.voice.shout"),
    (5, "basis.core.voice.recipients"),
    (6, "basis.core.avatar.verylow"),
    (7, "basis.core.avatar.verylow.additional"),
    (8, "basis.core.avatar.low"),
    (9, "basis.core.avatar.low.additional"),
    (10, "basis.core.avatar.medium"),
    (11, "basis.core.avatar.medium.additional"),
    (12, "basis.core.avatar.high"),
    (13, "basis.core.avatar.high.additional"),
    (14, "basis.core.avatar.change"),
    (15, "basis.core.avatar.data"),
    (16, "basis.core.player.create"),
    (17, "basis.core.player.create.bulk"),
    (18, "basis.core.chat"),
    (19, "basis.core.ownership.get"),
    (20, "basis.core.ownership.change"),
    (21, "basis.core.ownership.remove"),
    (22, "basis.core.netid.assign"),
    (23, "basis.core.netid.assigns"),
    (24, "basis.core.scene.data"),
    (25, "basis.core.resource.load"),
    (26, "basis.core.resource.unload"),
    (27, "basis.core.resource.preloadready"),
    (28, "basis.core.resource.spawnpreloaded"),
    (29, "basis.core.contentshare"),
    (30, "basis.core.avatar.delta"),
    (31, "basis.core.serverbound"),
    (34, "basis.core.admin"),
    (35, "basis.core.statistics"),
    (36, "basis.core.camera.pip.state"),
    (37, "basis.core.camera.pip.position"),
    (38, "basis.core.events"),
    (39, "basis.core.voice.recipients.large"),
    (40, "basis.core.voice.large"),
    (41, "basis.core.avatar.verylow.large"),
    (42, "basis.core.avatar.verylow.additional.large"),
    (43, "basis.core.avatar.low.large"),
    (44, "basis.core.avatar.low.additional.large"),
    (45, "basis.core.avatar.medium.large"),
    (46, "basis.core.avatar.medium.additional.large"),
    (47, "basis.core.avatar.high.large"),
    (48, "basis.core.avatar.high.additional.large"),
    (49, "basis.core.voice.recipients.inverted"),
    (50, "basis.core.voice.recipients.inverted.large"),
    (51, "basis.core.voice.recipients.bitfield"),
    (52, "basis.core.avatar.bundle.compressed"),
    (53, "basis.core.library"),
    (54, "basis.core.p2p"),
    (55, "basis.core.resource.modify"),
    (56, "basis.core.scene.direct"),
    (57, "basis.core.scene.direct.server"),
    (58, "basis.core.avatar.direct"),
    (59, "basis.core.avatar.direct.server"),
    (60, "basis.core.registry.control"),
];

#[test]
fn core_covers_channels_0_through_60_except_freed() {
    let core = BasisMessageCatalog::build_core();
    assert_eq!(core.len(), 59);
    let mut channels = HashSet::new();
    for descriptor in core {
        assert!(channels.insert(descriptor.channel), "duplicate channel {}", descriptor.channel);
    }
    for channel in 0u8..=60 {
        if channel == 32 || channel == 33 {
            continue; // freed by the database removal
        }
        assert!(channels.contains(&channel), "channel {channel}");
    }
    assert!(!channels.contains(&32));
    assert!(!channels.contains(&33));
}

#[test]
fn core_ids_equal_their_channel_and_are_unique() {
    let mut ids = HashSet::new();
    for descriptor in BasisMessageCatalog::build_core() {
        assert_eq!(descriptor.id, u16::from(descriptor.channel));
        assert!(ids.insert(descriptor.id), "duplicate id {}", descriptor.id);
    }
}

#[test]
fn core_names_are_unique_non_empty_and_core_prefixed() {
    let mut names = HashSet::new();
    for descriptor in BasisMessageCatalog::build_core() {
        assert!(!descriptor.name.trim().is_empty(), "channel {} has a blank name", descriptor.channel);
        assert!(descriptor.name.starts_with("basis.core."));
        assert!(names.insert(descriptor.name.clone()), "duplicate name {}", descriptor.name);
    }
}

#[test]
fn core_version_and_flags_are_uniform() {
    assert_eq!(BasisMessageCatalog::CORE_VERSION, 1);
    for descriptor in BasisMessageCatalog::build_core() {
        assert_eq!(descriptor.version, BasisMessageCatalog::CORE_VERSION);
        assert_eq!(descriptor.flags, BasisMessageFlags::NONE.bits());
    }
}

#[test]
fn core_channels_stay_below_the_plugin_and_total_range() {
    for descriptor in BasisMessageCatalog::build_core() {
        assert!(descriptor.channel < C::TOTAL_CHANNELS);
        assert!(descriptor.channel < C::PLUGIN_RELIABLE_CHANNEL, "core channel {} intrudes on the multiplexed plugin range", descriptor.channel);
        assert!(!C::is_plugin_channel(descriptor.channel));
    }
}

#[test]
fn core_name_to_channel_bindings_are_pinned() {
    let core = BasisMessageCatalog::build_core();
    assert_eq!(core.len(), PINNED_CORE_CATALOG.len());
    for (channel, name) in PINNED_CORE_CATALOG {
        let matches: Vec<&BasisMessageDescriptor> = core.iter().filter(|d| d.channel == channel).collect();
        assert_eq!(matches.len(), 1, "channel {channel}");
        assert_eq!(matches[0].name, name);
        assert_eq!(matches[0].id, u16::from(channel));
    }
}

#[test]
fn build_core_is_stable_across_calls() {
    let first = BasisMessageCatalog::build_core();
    let second = BasisMessageCatalog::build_core();
    assert_eq!(first.len(), second.len());
    for (a, b) in first.iter().zip(second) {
        assert_eq!((a.id, a.channel, a.version, a.flags, &a.name), (b.id, b.channel, b.version, b.flags, &b.name));
    }
}

// ── MessageManifestSerializationTests ──

#[test]
fn descriptor_round_trips() {
    let cases: [(u16, u8, u8, u8, &str); 5] = [
        (0, 1, 0, BasisMessageFlags::NONE.bits(), "basis.core.auth.identity"),
        (64, 1, 61, BasisMessageFlags::MULTIPLEXED.bits() | BasisMessageFlags::REQUIRED.bits(), "com.acme.plugin.foo"),
        (300, 7, 62, BasisMessageFlags::MULTIPLEXED.bits() | BasisMessageFlags::SERVER_TO_CLIENT.bits() | BasisMessageFlags::CLIENT_TO_SERVER.bits(), "com.例.プラグイン"),
        (u16::MAX, u8::MAX, 63, u8::MAX, "x"),
        (1, 0, 1, 0, ""),
    ];
    for (id, version, channel, flags, name) in cases {
        let source = BasisMessageDescriptor { id, version, channel, flags, name: name.to_string() };
        let mut writer = NetDataWriter::new();
        source.serialize(&mut writer).expect("serialize");

        let mut reader = rewind(&writer);
        let mut decoded = BasisMessageDescriptor::default();
        assert!(decoded.deserialize(&mut reader), "{name}");
        assert!(reader.end_of_data());
        assert_eq!((decoded.id, decoded.version, decoded.channel, decoded.flags, decoded.name.as_str()), (id, version, channel, flags, name));
    }
}

#[test]
fn descriptor_wire_size_is_seven_plus_utf8_name_bytes() {
    let mut writer = NetDataWriter::new();
    BasisMessageDescriptor { id: 3, version: 1, channel: 3, flags: 0, name: "basis.core.voice".into() }.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 7 + "basis.core.voice".len());

    let mut writer = NetDataWriter::new();
    BasisMessageDescriptor { id: 9, version: 1, channel: 9, flags: 0, name: "é".into() }.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 7 + "é".len());

    let mut writer = NetDataWriter::new();
    BasisMessageDescriptor { id: 1, version: 1, channel: 1, flags: 0, name: String::new() }.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 7);
}

#[test]
fn descriptor_deserialize_fails_on_truncated_buffers() {
    let source = BasisMessageDescriptor { id: 42, version: 1, channel: 30, flags: 2, name: "basis.core.avatar.delta".into() };
    let mut writer = NetDataWriter::new();
    source.serialize(&mut writer).expect("serialize");
    for size in [0usize, 1, 2, 3, 4, 5, 6, writer.length() - 1] {
        let mut decoded = BasisMessageDescriptor::default();
        assert!(!decoded.deserialize(&mut truncated(&writer, size)), "size {size} should fail");
    }
}

#[test]
fn supply_round_trips_full_core_catalog_preserving_order() {
    let source = BasisMessageSupply { descriptors: BasisMessageCatalog::build_core().to_vec() };
    let mut writer = NetDataWriter::new();
    source.serialize(&mut writer).expect("serialize");

    let mut reader = rewind(&writer);
    let mut decoded = BasisMessageSupply::default();
    assert!(decoded.deserialize(&mut reader));
    assert!(reader.end_of_data());

    assert_eq!(decoded.descriptors.len(), source.descriptors.len());
    for (a, b) in source.descriptors.iter().zip(&decoded.descriptors) {
        assert_eq!((a.id, a.version, a.channel, a.flags, &a.name), (b.id, b.version, b.channel, b.flags, &b.name));
    }
}

#[test]
fn supply_empty_descriptors_serialize_as_empty() {
    let source = BasisMessageSupply { descriptors: Vec::new() };
    let mut writer = NetDataWriter::new();
    source.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 2);

    let mut decoded = BasisMessageSupply::default();
    assert!(decoded.deserialize(&mut rewind(&writer)));
    assert!(decoded.descriptors.is_empty());
}

#[test]
fn supply_deserialize_fails_on_empty_or_truncated_buffers() {
    let mut empty = BasisMessageSupply::default();
    assert!(!empty.deserialize(&mut NetDataReader::from_slice(&[])));
    assert!(empty.descriptors.is_empty());

    let source = BasisMessageSupply { descriptors: BasisMessageCatalog::build_core().to_vec() };
    let mut writer = NetDataWriter::new();
    source.serialize(&mut writer).expect("serialize");
    for size in [1usize, 2, 10, writer.length() - 1] {
        let mut decoded = BasisMessageSupply::default();
        assert!(!decoded.deserialize(&mut truncated(&writer, size)), "size {size} should fail");
    }
}

#[test]
fn subscribe_round_trips_including_edge_ids_and_duplicates() {
    let ids = vec![0u16, 1, 64, 64, 300, u16::MAX];
    let source = BasisMessageSubscribe { ids: ids.clone() };
    let mut writer = NetDataWriter::new();
    source.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 2 + ids.len() * 2);

    let mut reader = rewind(&writer);
    let mut decoded = BasisMessageSubscribe::default();
    assert!(decoded.deserialize(&mut reader));
    assert!(reader.end_of_data());
    assert_eq!(decoded.ids, ids);
}

#[test]
fn subscribe_empty_ids_serialize_as_empty() {
    let source = BasisMessageSubscribe { ids: Vec::new() };
    let mut writer = NetDataWriter::new();
    source.serialize(&mut writer).expect("serialize");
    assert_eq!(writer.length(), 2);

    let mut decoded = BasisMessageSubscribe::default();
    assert!(decoded.deserialize(&mut rewind(&writer)));
    assert!(decoded.ids.is_empty());
}

#[test]
fn subscribe_deserialize_fails_on_truncated_buffers() {
    let source = BasisMessageSubscribe { ids: vec![1, 2, 3] };
    let mut writer = NetDataWriter::new();
    source.serialize(&mut writer).expect("serialize");
    for size in [0usize, 1, 3, writer.length() - 1] {
        let mut decoded = BasisMessageSubscribe::default();
        assert!(!decoded.deserialize(&mut truncated(&writer, size)), "size {size} should fail");
    }
}

// ── NetworkChannelConstantTests ──

const CHANNEL_PINS: [(&str, u8, u8); 62] = [
    ("AUTH_IDENTITY_CHANNEL", C::AUTH_IDENTITY_CHANNEL, 0),
    ("META_DATA_CHANNEL", C::META_DATA_CHANNEL, 1),
    ("DISCONNECTION_CHANNEL", C::DISCONNECTION_CHANNEL, 2),
    ("VOICE_CHANNEL", C::VOICE_CHANNEL, 3),
    ("SHOUT_VOICE_CHANNEL", C::SHOUT_VOICE_CHANNEL, 4),
    ("AUDIO_RECIPIENTS_CHANNEL", C::AUDIO_RECIPIENTS_CHANNEL, 5),
    ("PLAYER_AVATAR_VERY_LOW_CHANNEL", C::PLAYER_AVATAR_VERY_LOW_CHANNEL, 6),
    ("PLAYER_AVATAR_VERY_LOW_ADDITIONAL_CHANNEL", C::PLAYER_AVATAR_VERY_LOW_ADDITIONAL_CHANNEL, 7),
    ("PLAYER_AVATAR_LOW_CHANNEL", C::PLAYER_AVATAR_LOW_CHANNEL, 8),
    ("PLAYER_AVATAR_LOW_ADDITIONAL_CHANNEL", C::PLAYER_AVATAR_LOW_ADDITIONAL_CHANNEL, 9),
    ("PLAYER_AVATAR_MEDIUM_CHANNEL", C::PLAYER_AVATAR_MEDIUM_CHANNEL, 10),
    ("PLAYER_AVATAR_MEDIUM_ADDITIONAL_CHANNEL", C::PLAYER_AVATAR_MEDIUM_ADDITIONAL_CHANNEL, 11),
    ("PLAYER_AVATAR_HIGH_CHANNEL", C::PLAYER_AVATAR_HIGH_CHANNEL, 12),
    ("PLAYER_AVATAR_HIGH_ADDITIONAL_CHANNEL", C::PLAYER_AVATAR_HIGH_ADDITIONAL_CHANNEL, 13),
    ("AVATAR_CHANGE_MESSAGE_CHANNEL", C::AVATAR_CHANGE_MESSAGE_CHANNEL, 14),
    ("AVATAR_CHANNEL", C::AVATAR_CHANNEL, 15),
    ("CREATE_REMOTE_PLAYER_CHANNEL", C::CREATE_REMOTE_PLAYER_CHANNEL, 16),
    ("CREATE_REMOTE_PLAYERS_FOR_NEW_PEER_CHANNEL", C::CREATE_REMOTE_PLAYERS_FOR_NEW_PEER_CHANNEL, 17),
    ("CHAT_CHANNEL", C::CHAT_CHANNEL, 18),
    ("GET_CURRENT_OWNER_REQUEST_CHANNEL", C::GET_CURRENT_OWNER_REQUEST_CHANNEL, 19),
    ("CHANGE_CURRENT_OWNER_REQUEST_CHANNEL", C::CHANGE_CURRENT_OWNER_REQUEST_CHANNEL, 20),
    ("REMOVE_CURRENT_OWNER_REQUEST_CHANNEL", C::REMOVE_CURRENT_OWNER_REQUEST_CHANNEL, 21),
    ("NET_ID_ASSIGN_CHANNEL", C::NET_ID_ASSIGN_CHANNEL, 22),
    ("NET_ID_ASSIGNS_CHANNEL", C::NET_ID_ASSIGNS_CHANNEL, 23),
    ("SCENE_CHANNEL", C::SCENE_CHANNEL, 24),
    ("LOAD_RESOURCE_CHANNEL", C::LOAD_RESOURCE_CHANNEL, 25),
    ("UNLOAD_RESOURCE_CHANNEL", C::UNLOAD_RESOURCE_CHANNEL, 26),
    ("PRELOAD_READY_CHANNEL", C::PRELOAD_READY_CHANNEL, 27),
    ("SPAWN_PRELOADED_CHANNEL", C::SPAWN_PRELOADED_CHANNEL, 28),
    ("CONTENT_SHARE_CHANNEL", C::CONTENT_SHARE_CHANNEL, 29),
    ("DELTA_AVATAR_CHANNEL", C::DELTA_AVATAR_CHANNEL, 30),
    ("SERVER_BOUND_CHANNEL", C::SERVER_BOUND_CHANNEL, 31),
    ("ADMIN_CHANNEL", C::ADMIN_CHANNEL, 34),
    ("SERVER_STATISTICS_CHANNEL", C::SERVER_STATISTICS_CHANNEL, 35),
    ("CAMERA_PIP_STATE_CHANNEL", C::CAMERA_PIP_STATE_CHANNEL, 36),
    ("CAMERA_PIP_POSITION_CHANNEL", C::CAMERA_PIP_POSITION_CHANNEL, 37),
    ("EVENTS_CHANNEL", C::EVENTS_CHANNEL, 38),
    ("AUDIO_RECIPIENTS_LARGE_CHANNEL", C::AUDIO_RECIPIENTS_LARGE_CHANNEL, 39),
    ("VOICE_LARGE_CHANNEL", C::VOICE_LARGE_CHANNEL, 40),
    ("PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL", C::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL, 41),
    ("PLAYER_AVATAR_VERY_LOW_ADDITIONAL_LARGE_CHANNEL", C::PLAYER_AVATAR_VERY_LOW_ADDITIONAL_LARGE_CHANNEL, 42),
    ("PLAYER_AVATAR_LOW_LARGE_CHANNEL", C::PLAYER_AVATAR_LOW_LARGE_CHANNEL, 43),
    ("PLAYER_AVATAR_LOW_ADDITIONAL_LARGE_CHANNEL", C::PLAYER_AVATAR_LOW_ADDITIONAL_LARGE_CHANNEL, 44),
    ("PLAYER_AVATAR_MEDIUM_LARGE_CHANNEL", C::PLAYER_AVATAR_MEDIUM_LARGE_CHANNEL, 45),
    ("PLAYER_AVATAR_MEDIUM_ADDITIONAL_LARGE_CHANNEL", C::PLAYER_AVATAR_MEDIUM_ADDITIONAL_LARGE_CHANNEL, 46),
    ("PLAYER_AVATAR_HIGH_LARGE_CHANNEL", C::PLAYER_AVATAR_HIGH_LARGE_CHANNEL, 47),
    ("PLAYER_AVATAR_HIGH_ADDITIONAL_LARGE_CHANNEL", C::PLAYER_AVATAR_HIGH_ADDITIONAL_LARGE_CHANNEL, 48),
    ("AUDIO_RECIPIENTS_INVERTED_CHANNEL", C::AUDIO_RECIPIENTS_INVERTED_CHANNEL, 49),
    ("AUDIO_RECIPIENTS_INVERTED_LARGE_CHANNEL", C::AUDIO_RECIPIENTS_INVERTED_LARGE_CHANNEL, 50),
    ("AUDIO_RECIPIENTS_BITFIELD_CHANNEL", C::AUDIO_RECIPIENTS_BITFIELD_CHANNEL, 51),
    ("COMPRESSED_AVATAR_BUNDLE_CHANNEL", C::COMPRESSED_AVATAR_BUNDLE_CHANNEL, 52),
    ("SERVER_LIBRARY_CHANNEL", C::SERVER_LIBRARY_CHANNEL, 53),
    ("P2P_CHANNEL", C::P2P_CHANNEL, 54),
    ("MODIFY_RESOURCE_CHANNEL", C::MODIFY_RESOURCE_CHANNEL, 55),
    ("DIRECT_SCENE_CHANNEL", C::DIRECT_SCENE_CHANNEL, 56),
    ("DIRECT_SCENE_SERVER_CHANNEL", C::DIRECT_SCENE_SERVER_CHANNEL, 57),
    ("DIRECT_AVATAR_CHANNEL", C::DIRECT_AVATAR_CHANNEL, 58),
    ("DIRECT_AVATAR_SERVER_CHANNEL", C::DIRECT_AVATAR_SERVER_CHANNEL, 59),
    ("REGISTRY_CONTROL_CHANNEL", C::REGISTRY_CONTROL_CHANNEL, 60),
    ("PLUGIN_RELIABLE_CHANNEL", C::PLUGIN_RELIABLE_CHANNEL, 61),
    ("PLUGIN_SEQUENCED_CHANNEL", C::PLUGIN_SEQUENCED_CHANNEL, 62),
    ("PLUGIN_UNRELIABLE_CHANNEL", C::PLUGIN_UNRELIABLE_CHANNEL, 63),
];

#[test]
fn total_channels_is_pinned_at_64() {
    assert_eq!(C::TOTAL_CHANNELS, 64);
}

#[test]
fn every_named_channel_has_its_pinned_value() {
    for (name, actual, expected) in CHANNEL_PINS {
        assert_eq!(actual, expected, "{name}");
    }
}

#[test]
fn all_named_channels_are_distinct_and_cover_zero_to_sixty_three_except_freed() {
    let mut seen = HashSet::new();
    for (name, actual, _) in CHANNEL_PINS {
        assert!(seen.insert(actual), "{name} duplicates channel value {actual}");
        assert!(actual < C::TOTAL_CHANNELS, "{name} = {actual} exceeds TOTAL_CHANNELS");
    }
    // 32 & 33 are free (held the removed server-side database).
    assert_eq!(seen.len(), usize::from(C::TOTAL_CHANNELS) - 2);
    assert!(!seen.contains(&32));
    assert!(!seen.contains(&33));
}

#[test]
fn avatar_quality_channel_math_round_trips_both_id_widths() {
    for additional in [false, true] {
        for quality in 0..4 {
            let small = C::get_player_avatar_channel_for_quality(quality, additional);
            assert_eq!(small, C::PLAYER_AVATAR_VERY_LOW_CHANNEL + (quality as u8) * 2 + u8::from(additional));
            assert_eq!(C::get_quality_from_channel(small), quality as u8);
            assert_eq!(C::channel_has_additional_data(small), additional);
            assert!(!C::is_large_player_id_channel(small));

            let large = C::get_player_avatar_large_channel_for_quality(quality, additional);
            assert_eq!(large, C::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL + (quality as u8) * 2 + u8::from(additional));
            assert_eq!(C::get_quality_from_channel(large), quality as u8);
            assert_eq!(C::channel_has_additional_data(large), additional);
            assert!(C::is_large_player_id_channel(large));
        }
    }
    assert!(C::is_large_player_id_channel(C::VOICE_LARGE_CHANNEL));
    assert!(!C::is_large_player_id_channel(C::AUDIO_RECIPIENTS_LARGE_CHANNEL));
    assert!(!C::is_large_player_id_channel(C::AUDIO_RECIPIENTS_INVERTED_CHANNEL));
}

#[test]
fn player_avatar_quality_channels_lists_all_16_in_pinned_order() {
    assert_eq!(C::PLAYER_AVATAR_QUALITY_CHANNELS, [6, 7, 8, 9, 10, 11, 12, 13, 41, 42, 43, 44, 45, 46, 47, 48]);
}

#[test]
fn plugin_channel_delivery_mapping_is_consistent_both_ways() {
    assert_eq!(C::get_plugin_channel_for_delivery(DeliveryMethod::ReliableOrdered), C::PLUGIN_RELIABLE_CHANNEL);
    assert_eq!(C::get_plugin_channel_for_delivery(DeliveryMethod::ReliableUnordered), C::PLUGIN_RELIABLE_CHANNEL);
    assert_eq!(C::get_plugin_channel_for_delivery(DeliveryMethod::ReliableSequenced), C::PLUGIN_RELIABLE_CHANNEL);
    assert_eq!(C::get_plugin_channel_for_delivery(DeliveryMethod::Sequenced), C::PLUGIN_SEQUENCED_CHANNEL);
    assert_eq!(C::get_plugin_channel_for_delivery(DeliveryMethod::Unreliable), C::PLUGIN_UNRELIABLE_CHANNEL);

    assert_eq!(C::get_delivery_for_plugin_channel(C::PLUGIN_RELIABLE_CHANNEL), DeliveryMethod::ReliableOrdered);
    assert_eq!(C::get_delivery_for_plugin_channel(C::PLUGIN_SEQUENCED_CHANNEL), DeliveryMethod::Sequenced);
    assert_eq!(C::get_delivery_for_plugin_channel(C::PLUGIN_UNRELIABLE_CHANNEL), DeliveryMethod::Unreliable);

    for channel in C::PLUGIN_RELIABLE_CHANNEL..=C::PLUGIN_UNRELIABLE_CHANNEL {
        assert_eq!(C::get_plugin_channel_for_delivery(C::get_delivery_for_plugin_channel(channel)), channel);
    }

    assert!(!C::is_plugin_channel(0));
    assert!(!C::is_plugin_channel(59));
    assert!(!C::is_plugin_channel(C::REGISTRY_CONTROL_CHANNEL));
    assert!(C::is_plugin_channel(61));
    assert!(C::is_plugin_channel(62));
    assert!(C::is_plugin_channel(63));
    assert!(!C::is_plugin_channel(64));
    assert!(!C::is_plugin_channel(u8::MAX));
}

#[test]
fn sub_type_constants_are_pinned_and_distinct_within_their_group() {
    assert_eq!(C::CONTENT_SHARE_SUB_DROP, 0);
    assert_eq!(C::CONTENT_SHARE_SUB_CLEANUP, 1);
    assert_eq!(C::REGISTRY_SUB_SUPPLY, 0);
    assert_eq!(C::REGISTRY_SUB_SUBSCRIBE, 1);

    let p2p = [C::P2P_SUB_REQUEST, C::P2P_SUB_ACCEPT, C::P2P_SUB_DECLINE, C::P2P_SUB_CANCEL, C::P2P_SUB_LINK_LOST, C::P2P_SUB_SERVER_ARMED, C::P2P_SUB_LINK_UP, C::P2P_SUB_OFFLOADED];
    assert_eq!(p2p, [0, 1, 2, 3, 4, 5, 6, 7]);

    let event_types = [
        C::EVENT_TYPE_CAMERA_SHUTTER_SOUND,
        C::EVENT_TYPE_CAMERA_COUNTDOWN,
        C::EVENT_TYPE_PLAYER_TEMP_BLOCK,
        C::EVENT_TYPE_AVATAR_RATE_CHANGE,
        C::EVENT_TYPE_TALK_MODE_CHANGED,
        C::EVENT_TYPE_MUTE_STATE_CHANGED,
        C::EVENT_TYPE_PLAYER_CHAT_TYPING,
        C::EVENT_TYPE_ERROR_REPORT,
        C::EVENT_TYPE_VOICE_RECORD_REQUEST,
        C::EVENT_TYPE_VOICE_RECORD_CONSENT,
    ];
    assert_eq!(event_types, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
}

#[test]
fn server_info_and_reject_wire_constants_are_pinned() {
    assert_eq!(C::SERVER_INFO_QUERY_MAGIC, 0xBA515101);
    assert_eq!(C::SERVER_INFO_RESPONSE_MAGIC, 0xBA515102);
    assert_ne!(C::SERVER_INFO_QUERY_MAGIC, C::SERVER_INFO_RESPONSE_MAGIC);
    assert_eq!(C::SERVER_INFO_PROTOCOL_VERSION, 1);
    assert_eq!(C::SERVER_INFO_NAME_MAX_LENGTH, 64);
    assert_eq!(C::SERVER_INFO_MOTD_MAX_LENGTH, 256);
    assert_eq!(C::SERVER_INFO_MIN_REQUEST_BYTES, 384);

    assert_eq!(C::REJECT_MAGIC, 0xBA515CE1);
    assert_eq!(C::REJECT_KIND_VERSION_MISMATCH, 1);
    assert_eq!(C::REJECT_KIND_SERVER_FULL, 2);
}

// ── ServerMessageRegistryTests ──

fn no_op_handler() -> BasisServerMessageHandler {
    Arc::new(|_, _, _, _| {})
}

#[test]
fn core_inbound_handlers_are_bound_after_init() {
    BasisServerMessageRegistry::ensure_initialized();
    for channel in [C::AUTH_IDENTITY_CHANNEL, C::VOICE_CHANNEL, C::DELTA_AVATAR_CHANNEL, C::CHAT_CHANNEL, C::REGISTRY_CONTROL_CHANNEL] {
        assert!(BasisServerMessageRegistry::resolve_core(channel).is_some(), "channel {channel}");
    }
}

#[test]
fn register_core_resolve_core_returns_the_registered_handler() {
    BasisServerMessageRegistry::ensure_initialized();
    // The disconnection channel the C# used is outbound-only here; chat is a bound inbound channel.
    let channel = C::CHAT_CHANNEL;
    let original = BasisServerMessageRegistry::resolve_core(channel).expect("original");
    let custom = no_op_handler();
    BasisServerMessageRegistry::register_core(channel, custom.clone());
    let resolved = BasisServerMessageRegistry::resolve_core(channel).expect("custom");
    BasisServerMessageRegistry::register_core(channel, original);
    assert!(Arc::ptr_eq(&custom, &resolved));
}

#[test]
fn register_server_plugin_assigns_ids_above_core_range_and_advertises_descriptor() {
    BasisServerMessageRegistry::ensure_initialized();
    const NAME_A: &str = "com.basistests.registry.alpha";
    const NAME_B: &str = "com.basistests.registry.beta";

    let id_a = BasisServerMessageRegistry::register_server_plugin(NAME_A, DeliveryMethod::Sequenced, no_op_handler(), 3, BasisMessageFlags::NONE);
    let id_b = BasisServerMessageRegistry::register_server_plugin(NAME_B, DeliveryMethod::Unreliable, no_op_handler(), 1, BasisMessageFlags::NONE);

    assert!(id_a >= u16::from(C::TOTAL_CHANNELS), "plugin id {id_a} must clear the core channel range");
    assert!(id_b >= u16::from(C::TOTAL_CHANNELS));
    assert_ne!(id_a, id_b);

    assert_eq!(BasisServerMessageRegistry::register_server_plugin(NAME_A, DeliveryMethod::Sequenced, no_op_handler(), 3, BasisMessageFlags::NONE), id_a);
    assert_eq!(BasisServerMessageRegistry::try_get_plugin_id(NAME_A), Some(id_a));

    let supply = BasisServerMessageRegistry::build_supply();
    let descriptor_a = supply.iter().find(|d| d.name == NAME_A).expect("descriptor a");
    assert_eq!(descriptor_a.id, id_a);
    assert_eq!(descriptor_a.channel, C::PLUGIN_SEQUENCED_CHANNEL);
    assert_eq!(descriptor_a.version, 3);
    assert_ne!(descriptor_a.flags & BasisMessageFlags::MULTIPLEXED.bits(), 0);

    let descriptor_b = supply.iter().find(|d| d.name == NAME_B).expect("descriptor b");
    assert_eq!(descriptor_b.channel, C::PLUGIN_UNRELIABLE_CHANNEL);

    BasisServerMessageRegistry::unregister_plugin(id_a);
    BasisServerMessageRegistry::unregister_plugin(id_b);
}

#[test]
fn unregister_plugin_removes_from_supply_but_name_keeps_its_stable_id() {
    BasisServerMessageRegistry::ensure_initialized();
    const NAME: &str = "com.basistests.registry.gamma";

    let id = BasisServerMessageRegistry::register_server_plugin(NAME, DeliveryMethod::ReliableOrdered, no_op_handler(), 1, BasisMessageFlags::NONE);
    assert!(BasisServerMessageRegistry::build_supply().iter().any(|d| d.name == NAME));

    assert!(BasisServerMessageRegistry::unregister_plugin(id));
    assert!(!BasisServerMessageRegistry::unregister_plugin(id));
    assert!(!BasisServerMessageRegistry::build_supply().iter().any(|d| d.name == NAME));

    assert_eq!(BasisServerMessageRegistry::try_get_plugin_id(NAME), Some(id));

    let again = BasisServerMessageRegistry::register_server_plugin(NAME, DeliveryMethod::ReliableOrdered, no_op_handler(), 1, BasisMessageFlags::NONE);
    BasisServerMessageRegistry::unregister_plugin(again);
    assert_eq!(again, id);
}

#[test]
fn build_supply_always_contains_the_full_core_catalog() {
    BasisServerMessageRegistry::ensure_initialized();
    let supply = BasisServerMessageRegistry::build_supply();
    for core in BasisMessageCatalog::build_core() {
        assert!(supply.iter().any(|d| d.id == core.id && d.channel == core.channel && d.name == core.name), "{}", core.name);
    }
}

#[test]
fn subscriptions_filter_only_after_a_peer_reports() {
    BasisServerMessageRegistry::ensure_initialized();
    const PEER_ID: i32 = 987654;
    assert!(BasisServerMessageRegistry::is_subscribed(PEER_ID, 64));

    BasisServerMessageRegistry::set_subscription(PEER_ID, &[64, 70]);
    assert!(BasisServerMessageRegistry::is_subscribed(PEER_ID, 64));
    assert!(BasisServerMessageRegistry::is_subscribed(PEER_ID, 70));
    assert!(!BasisServerMessageRegistry::is_subscribed(PEER_ID, 65));

    BasisServerMessageRegistry::set_subscription(PEER_ID, &[]);
    assert!(!BasisServerMessageRegistry::is_subscribed(PEER_ID, 64));

    BasisServerMessageRegistry::clear_subscription(PEER_ID);
    assert!(BasisServerMessageRegistry::is_subscribed(PEER_ID, 64));
}
