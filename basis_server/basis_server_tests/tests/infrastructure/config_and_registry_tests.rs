//! Server configuration defaults and persistence, the per-transport config sidecars, the
//! connection-target property bag and its LiteNetLib parser, the network stack registry, the
//! inbound message binding table, and the XML doc-comment injector.
//!
//! Differences from the C# suite: the Rust registry's default stack is iroh rather than
//! LiteNetLib (both are registered), argument guards that threw `ArgumentException` are checked
//! as panics from a programming error, and a tick that throws has no Rust equivalent because a
//! tick cannot fail.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use basis_network_core::BasisNetworkCommons;
use basis_network_core::configuration::{BasisConfigXmlDocs, BasisTransportConfigStore, BasisXmlConfig, Configuration, FieldKind, LNLTransportConfig};
use basis_network_core::identity::BasisUserRestrictionMode;
use basis_network_core::transport::basis_network_stack_registry::ServerProbeResult;
use basis_network_core::transport::connection_target::{ConnectionTarget, ConnectionTargetKeys, IConnectionTargetParser};
use basis_network_core::transport::lnl_connection_target_parser::LNLConnectionTargetParser;
use basis_network_core::p2p::{IPeerIntroducer, PeerIntroduction};
use basis_network_core::transport::{BasisNetworkStackRegistry, EventBasedNetListener, NetManagerRef};
use basis_network_server::messaging::basis_server_message_registry::BasisServerMessageRegistry;
use parking_lot::Mutex;
use serial_test::serial;

// ── shared helpers ──

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join("BasisCfgTests").join(uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Sets every serialized field to a value different from its current one.
fn mutate_all_fields<T: BasisXmlConfig>(target: &mut T) {
    for name in T::field_names() {
        let current = target.get_field(name).unwrap_or_else(|| panic!("field {name} unreadable"));
        let kind = T::field_kind(name).unwrap_or_else(|| panic!("field {name} has no kind"));
        let next = match kind {
            FieldKind::Str => format!("{current}_x"),
            FieldKind::Bool => (!current.parse::<bool>().expect("bool")).to_string(),
            FieldKind::Int | FieldKind::Long => (current.parse::<i64>().expect("int") + 7).to_string(),
            FieldKind::UShort => (current.parse::<u16>().expect("ushort").wrapping_add(5)).to_string(),
            FieldKind::Byte => (current.parse::<u8>().expect("byte").wrapping_add(3)).to_string(),
            FieldKind::Float | FieldKind::Double => (current.parse::<f64>().expect("float") + 1.5).to_string(),
            FieldKind::RestrictionMode => if current == "Normal" { "BanList".to_string() } else { "Normal".to_string() },
        };
        target.set_field(name, &next).unwrap_or_else(|e| panic!("set {name}={next}: {e}"));
    }
}

fn assert_fields_equal<T: BasisXmlConfig>(expected: &T, actual: &T, skip: &[&str]) {
    for name in T::field_names() {
        if skip.contains(name) {
            continue;
        }
        assert_eq!(expected.get_field(name), actual.get_field(name), "field {name}");
    }
}

fn new_stack_id() -> String {
    format!("teststack-{}", uuid::Uuid::new_v4().simple())
}

fn lnl_sidecar(config_dir: &Path) -> PathBuf {
    config_dir.join(BasisTransportConfigStore::TRANSPORTS_FOLDER_NAME).join(format!("{}.xml", BasisNetworkStackRegistry::LITE_NET_LIB_ID))
}

// ── ServerConfigurationDefaultsTests ──

#[test]
fn defaults_networking_and_health() {
    let cfg = Configuration::default();
    assert_eq!(cfg.peer_limit, i32::from(u16::MAX));
    assert_eq!(cfg.set_port, 4296);
    assert_eq!(cfg.server_name, "Basis Server");
    assert_eq!(cfg.server_motd, "");
    assert!(cfg.enable_statistics);
    assert!(cfg.has_file_support);
    assert_eq!(cfg.health_check_host, "localhost");
    assert_eq!(cfg.health_check_port, 10666);
    assert_eq!(cfg.health_path, "/health");
    assert!(!cfg.health_include_bsr_profiling);
    assert!(!cfg.override_auto_discovery_of_ipv);
    assert_eq!(cfg.i_pv4_address, "0.0.0.0");
    assert_eq!(cfg.i_pv6_address, "::");
    assert_eq!(cfg.network_stack_id, "");
}

#[test]
fn defaults_v42_bandwidth_features() {
    let cfg = Configuration::default();
    assert_eq!(cfg.voice_frame_duration_ms, 20);
    assert!(cfg.enable_avatar_bundle_compression);
    assert_eq!(cfg.avatar_bundle_min_messages, 2);
    assert_eq!(cfg.avatar_bundle_min_bytes, 128);
    assert!(cfg.enable_avatar_delta_compression);
    assert_eq!(cfg.avatar_delta_keyframe_interval_ms, 500);
    assert_eq!(cfg.avatar_delta_keyframe_max_interval_ms, 2000);
    assert!(cfg.strip_additional_data_at_low_quality);
    assert!(cfg.enable_uplink_avatar_delta);
    assert_eq!(cfg.image_share_egress_megabits_per_second, 200);
    assert_eq!(cfg.image_pickup_range_meters, 64.0);
}

#[test]
fn defaults_auth_content_locks_and_limits() {
    let cfg = Configuration::default();
    assert_eq!(cfg.password, "default_password");
    assert!(cfg.use_auth);
    assert!(cfg.use_auth_identity);
    assert_eq!(cfg.basis_user_restriction_mode, BasisUserRestrictionMode::Normal);
    assert_eq!(cfg.how_many_duplicate_auth_can_exist, 2);
    assert_eq!(cfg.auth_validation_time_out_miliseconds, 9000);
    assert!(!cfg.avatars_locked);
    assert!(!cfg.props_locked);
    assert!(cfg.worlds_locked);
    assert!(!cfg.servers_locked);
    assert!(!cfg.third_person_disabled);
    assert!(!cfg.additional_avatar_data_lock);
    assert_eq!(cfg.camera_metadata_disallow_mask, 0);
    assert_eq!(cfg.max_microphone_range_meters, 25.0);
    assert_eq!(cfg.max_hearing_range_meters, 25.0);
    assert_eq!(cfg.min_avatar_eye_height_meters, 0.1);
    assert_eq!(cfg.max_avatar_eye_height_meters, 100.0);
    assert!(!cfg.disallow_headless);
}

#[test]
fn defaults_reduction_system_curve() {
    let cfg = Configuration::default();
    assert_eq!(cfg.bsrs_millisecond_default_interval, 50);
    assert_eq!(cfg.bsr_base_multiplier, 1);
    assert_eq!(cfg.bsrs_increase_rate, 0.005);
    assert_eq!(cfg.bsr_slowest_send_rate, 2.55);
    assert_eq!(cfg.high_quality_distance, 10.0);
    assert_eq!(cfg.medium_quality_distance, 20.0);
    assert_eq!(cfg.low_quality_distance, 40.0);
    assert!(!cfg.enable_bsr_profiling);
}

#[test]
fn defaults_rest_api_and_diagnostics() {
    let cfg = Configuration::default();
    assert!(!cfg.api_enabled);
    assert_eq!(cfg.api_host, "localhost");
    assert_eq!(cfg.api_port, 10667);
    assert_eq!(cfg.api_key, "");
    assert!(cfg.crash_reporting_enabled);
    assert_eq!(cfg.max_content_spheres_per_player, 32);
}

#[test]
fn defaults_versioning_and_folder_constants() {
    // 13: added LogConnectionHandshake; the per-connection auth chatter is now off by default.
    assert_eq!(Configuration::CURRENT_CONFIG_VERSION, 13);
    assert_eq!(Configuration::default().config_version, 0);
    assert_eq!(Configuration::CONFIG_FOLDER_NAME, "config");
    assert_eq!(Configuration::LOGS_FOLDER_NAME, "logs");
    let default_path = Configuration::get_default_path();
    assert!(default_path.is_absolute());
    assert!(default_path.ends_with(Path::new(Configuration::CONFIG_FOLDER_NAME).join("config.xml")));
}

#[test]
fn default_server_port_matches_parser_default_port() {
    assert_eq!(LNLConnectionTargetParser::DEFAULT_PORT, Configuration::default().set_port);
    assert_eq!(LNLConnectionTargetParser::DEFAULT_PORT, 4296);
}

// ── ConfigurationPersistenceTests ── config.xml load/save; shares a key with the transport
// store tests because load_from_xml/save_to_xml also drive the static store.

#[test]
#[serial(config_statics)]
fn load_from_xml_missing_file_writes_defaults_with_doc_comments() {
    let dir = TempDir::new();
    let path = dir.file("config.xml");

    let loaded = Configuration::load_from_xml(&path).expect("load");

    assert!(path.exists());
    assert_fields_equal(&Configuration::default(), &loaded, &["ConfigVersion"]);
    assert_eq!(loaded.config_version, Configuration::CURRENT_CONFIG_VERSION);

    let xml = std::fs::read_to_string(&path).expect("read");
    assert!(xml.contains("<!--"));
    assert!(xml.contains("Basis dedicated-server configuration"));
    assert!(xml.contains("<PeerLimit>"));

    assert!(lnl_sidecar(dir.path()).exists());
    assert!(dir.path().join(BasisTransportConfigStore::TRANSPORTS_FOLDER_NAME).join(format!("{}.xml", BasisNetworkStackRegistry::IROH_ID)).exists());
}

#[test]
#[serial(config_statics)]
fn save_then_load_round_trips_every_public_field() {
    let dir = TempDir::new();
    let path = dir.file("config.xml");

    let mut expected = Configuration::default();
    mutate_all_fields(&mut expected);
    expected.save_to_xml(&path).expect("save");

    let loaded = Configuration::load_from_xml(&path).expect("load");
    assert_fields_equal(&expected, &loaded, &["ConfigVersion"]);
    assert_eq!(loaded.config_version, Configuration::CURRENT_CONFIG_VERSION);
}

#[test]
#[serial(config_statics)]
fn load_from_xml_partial_file_keeps_values_heals_missing_ignores_unknown_elements() {
    let dir = TempDir::new();
    let path = dir.file("config.xml");
    std::fs::write(&path, "<Configuration><SetPort>5555</SetPort><ServerName>Partial Server</ServerName><NotARealSetting>ignored</NotARealSetting></Configuration>").expect("write");

    let loaded = Configuration::load_from_xml(&path).expect("load");

    assert_eq!(loaded.set_port, 5555);
    assert_eq!(loaded.server_name, "Partial Server");
    assert_eq!(loaded.peer_limit, Configuration::default().peer_limit);
    assert_eq!(loaded.config_version, Configuration::CURRENT_CONFIG_VERSION);

    let healed = std::fs::read_to_string(&path).expect("read");
    assert!(healed.contains("<SetPort>5555</SetPort>"));
    assert!(healed.contains("EnableUplinkAvatarDelta"));
    assert!(healed.contains("VoiceFrameDurationMs"));
    assert!(!healed.contains("NotARealSetting"));
    assert!(!BasisConfigXmlDocs::is_missing_any_field::<Configuration>(&path));
}

#[test]
#[serial(config_statics)]
fn load_from_xml_malformed_or_empty_file_is_an_error() {
    let dir = TempDir::new();
    let garbage = dir.file("garbage.xml");
    std::fs::write(&garbage, "this is not xml at all <<<>").expect("write");
    assert!(Configuration::load_from_xml(&garbage).is_err());

    let empty = dir.file("empty.xml");
    std::fs::write(&empty, "").expect("write");
    assert!(Configuration::load_from_xml(&empty).is_err());

    // A file with the wrong root, and one with a value that does not parse, are refused rather
    // than silently loaded as defaults.
    let wrong_root = dir.file("wrong-root.xml");
    std::fs::write(&wrong_root, "<NotConfiguration><SetPort>1</SetPort></NotConfiguration>").expect("write");
    assert!(Configuration::load_from_xml(&wrong_root).is_err());

    let bad_value = dir.file("bad-value.xml");
    std::fs::write(&bad_value, "<Configuration><SetPort>not-a-port</SetPort></Configuration>").expect("write");
    assert!(Configuration::load_from_xml(&bad_value).is_err());
}

#[test]
#[serial(config_statics)]
fn save_to_xml_into_an_unwritable_location_is_an_error() {
    let dir = TempDir::new();
    let blocked = dir.file("blocked");
    std::fs::write(&blocked, "a file, not a directory").expect("write");
    let mut cfg = Configuration::default();
    assert!(cfg.save_to_xml(&blocked.join("config.xml")).is_err());
}

#[test]
#[serial(environment)]
fn environmental_overrides_apply_by_field_name_and_reject_unparseable_values() {
    let pairs = [("PeerLimit", "512"), ("ServerMotd", "env motd"), ("UseAuthIdentity", "false"), ("BSRSIncreaseRate", "0.25"), ("SetPort", "not-a-port")];
    for (name, value) in pairs {
        // SAFETY: the test is serialised on the `environment` key and no other thread reads the
        // environment while it runs.
        unsafe { std::env::set_var(name, value) };
    }
    let mut cfg = Configuration::default();
    cfg.process_environmental_overrides();
    for (name, _) in pairs {
        unsafe { std::env::remove_var(name) };
    }

    assert_eq!(cfg.peer_limit, 512);
    assert_eq!(cfg.server_motd, "env motd");
    assert!(!cfg.use_auth_identity);
    assert_eq!(cfg.bsrs_increase_rate, 0.25);
    assert_eq!(cfg.set_port, 4296);
}

// ── TransportConfigStoreTests ── per-transport sidecars ({configDir}/transports/{stackId}.xml).

#[test]
fn lnl_transport_config_defaults() {
    let cfg = LNLTransportConfig::default();
    // 10: added MaxPriorityUnreliableQueuePerPeer, which splits voice out of the bulk queue.
    assert_eq!(LNLTransportConfig::CURRENT_CONFIG_VERSION, 10);
    assert_eq!(cfg.max_priority_unreliable_queue_per_peer, 0); // 0 = auto from population + memory
    assert!(cfg.compact_merged);
    assert_eq!(cfg.max_send_sockets, 0); // 0 = auto: half the cores, 4 to 64
    assert_eq!(cfg.peer_update_peers_per_worker, 0);
    assert_eq!(cfg.max_unreliable_queue_per_peer, 0); // 0 = auto from population + memory
    assert_eq!(cfg.config_version(), 0);
    assert_eq!(cfg.merge_hold_ms, 3.0);
    assert_eq!(cfg.peer_update_parallelism, 0);
    assert!(cfg.use_native_sockets);
    assert!(cfg.nat_punch_enabled);
    assert_eq!(cfg.nat_port_prediction_range, 32);
    assert_eq!(cfg.ping_interval, 1500);
    assert_eq!(cfg.disconnect_timeout, 30000);
    assert!(!cfg.simulate_packet_loss);
    assert!(!cfg.simulate_latency);
    assert_eq!(cfg.simulation_packet_loss_chance, 10);
    assert_eq!(cfg.simulation_min_latency, 50);
    assert_eq!(cfg.simulation_max_latency, 150);
    assert_eq!(cfg.reconnect_delay, 500);
    assert_eq!(cfg.max_connect_attempts, 10);
    assert!(!cfg.reuse_addresss);
    assert!(!cfg.dont_route);
    assert!(cfg.i_pv6_enabled);
    assert_eq!(cfg.mtu_override, 0);
    assert!(cfg.mtu_discovery);
    assert!(!cfg.disconnect_on_unreachable);
    assert!(cfg.allow_peer_address_change);
    assert_eq!(cfg.multi_socket_count, 1);
    // Packet pool scales with peer count rather than sitting at a fixed ceiling; the floor stays
    // PacketPoolSize, so small servers behave exactly as before.
    assert_eq!(cfg.packet_pool_size_per_peer, 48);
    assert_eq!(cfg.packet_pool_size_max, 0); // 0 = auto from population + memory
}

#[test]
#[serial(config_statics)]
fn load_all_creates_default_sidecar_with_doc_comments() {
    BasisNetworkStackRegistry::ensure_initialized();
    let dir = TempDir::new();

    BasisTransportConfigStore::load_all(dir.path());

    let sidecar = lnl_sidecar(dir.path());
    assert!(sidecar.exists());
    let xml = std::fs::read_to_string(&sidecar).expect("read");
    assert!(xml.contains("<!--"));
    assert!(xml.contains("LiteNetLib transport tuning"));
    assert!(xml.contains("MultiSocketCount"));

    let cfg = BasisTransportConfigStore::get::<LNLTransportConfig>(BasisNetworkStackRegistry::LITE_NET_LIB_ID);
    assert_eq!(cfg.ping_interval, 1500);
    assert_eq!(cfg.config_version(), LNLTransportConfig::CURRENT_CONFIG_VERSION);
}

#[test]
#[serial(config_statics)]
fn save_all_then_load_all_round_trips_every_public_field() {
    BasisNetworkStackRegistry::ensure_initialized();
    let dir = TempDir::new();
    BasisTransportConfigStore::load_all(dir.path());

    let mut expected = LNLTransportConfig::default();
    mutate_all_fields(&mut expected);
    BasisTransportConfigStore::set(BasisNetworkStackRegistry::LITE_NET_LIB_ID, expected.clone());
    BasisTransportConfigStore::save_all(dir.path());
    BasisTransportConfigStore::load_all(dir.path());

    let loaded = BasisTransportConfigStore::get::<LNLTransportConfig>(BasisNetworkStackRegistry::LITE_NET_LIB_ID);
    assert_fields_equal(&expected, &loaded, &["ConfigVersion"]);
    assert_eq!(loaded.config_version(), LNLTransportConfig::CURRENT_CONFIG_VERSION);
}

#[test]
#[serial(config_statics)]
fn load_all_partial_sidecar_keeps_value_and_heals_missing_fields() {
    BasisNetworkStackRegistry::ensure_initialized();
    let dir = TempDir::new();
    let sidecar = lnl_sidecar(dir.path());
    std::fs::create_dir_all(sidecar.parent().expect("parent")).expect("mkdir");
    std::fs::write(&sidecar, "<LNLTransportConfig><PingInterval>777</PingInterval></LNLTransportConfig>").expect("write");

    BasisTransportConfigStore::load_all(dir.path());

    let cfg = BasisTransportConfigStore::get::<LNLTransportConfig>(BasisNetworkStackRegistry::LITE_NET_LIB_ID);
    assert_eq!(cfg.ping_interval, 777);
    assert!(cfg.use_native_sockets);
    assert_eq!(cfg.nat_port_prediction_range, 32);
    assert_eq!(cfg.config_version(), LNLTransportConfig::CURRENT_CONFIG_VERSION);

    let healed = std::fs::read_to_string(&sidecar).expect("read");
    assert!(healed.contains("<PingInterval>777</PingInterval>"));
    assert!(healed.contains("MultiSocketCount"));
}

#[test]
#[serial(config_statics)]
fn load_all_corrupt_sidecar_recreates_defaults_without_panicking() {
    BasisNetworkStackRegistry::ensure_initialized();
    let dir = TempDir::new();
    let sidecar = lnl_sidecar(dir.path());
    std::fs::create_dir_all(sidecar.parent().expect("parent")).expect("mkdir");
    std::fs::write(&sidecar, "{ definitely not xml )").expect("write");

    BasisTransportConfigStore::load_all(dir.path());

    let cfg = BasisTransportConfigStore::get::<LNLTransportConfig>(BasisNetworkStackRegistry::LITE_NET_LIB_ID);
    assert_eq!(cfg.ping_interval, 1500);
    assert!(std::fs::read_to_string(&sidecar).expect("read").contains("UseNativeSockets"));
}

#[test]
#[serial(config_statics)]
fn get_unknown_id_creates_and_caches_one_instance() {
    let uid = new_stack_id();
    let first = BasisTransportConfigStore::get::<LNLTransportConfig>(&uid);
    let second = BasisTransportConfigStore::get::<LNLTransportConfig>(&uid);
    assert_fields_equal(&first, &second, &[]);
    assert!(BasisTransportConfigStore::get_object(&uid).is_some());
    // The cached instance is the one later reads see.
    BasisTransportConfigStore::with_mut::<LNLTransportConfig, ()>(&uid, |c| c.ping_interval = 999);
    assert_eq!(BasisTransportConfigStore::get::<LNLTransportConfig>(&uid).ping_interval, 999);
}

#[test]
#[serial(config_statics)]
fn get_empty_id_routes_to_default_stack() {
    BasisNetworkStackRegistry::ensure_initialized();
    let direct = BasisTransportConfigStore::get::<LNLTransportConfig>(BasisNetworkStackRegistry::DEFAULT_ID);
    assert_fields_equal(&direct, &BasisTransportConfigStore::get::<LNLTransportConfig>(""), &[]);
    assert!(BasisTransportConfigStore::get_object("").is_none());
}

#[test]
#[serial(config_statics)]
fn set_stores_instance_and_empty_ids_are_refused() {
    let uid = new_stack_id();
    let mine = LNLTransportConfig { ping_interval: 4242, ..Default::default() };
    BasisTransportConfigStore::set(&uid, mine);
    assert_eq!(BasisTransportConfigStore::get::<LNLTransportConfig>(&uid).ping_interval, 4242);

    // An empty stack id is a programming error: it must never create an entry under "".
    let _ = std::panic::catch_unwind(|| BasisTransportConfigStore::set("", LNLTransportConfig::default()));
    let _ = std::panic::catch_unwind(|| BasisTransportConfigStore::register_type::<LNLTransportConfig>(""));
    assert!(!BasisTransportConfigStore::registered_types().contains_key(""));
    assert!(BasisTransportConfigStore::get_object("").is_none());
}

#[test]
#[serial(config_statics)]
fn register_type_lists_type_and_re_registration_keeps_existing_config() {
    BasisNetworkStackRegistry::ensure_initialized();
    assert_eq!(BasisTransportConfigStore::registered_types().get(BasisNetworkStackRegistry::LITE_NET_LIB_ID).copied(), Some(std::any::type_name::<LNLTransportConfig>()));

    let uid = new_stack_id();
    BasisTransportConfigStore::register_type::<LNLTransportConfig>(&uid);
    assert!(BasisTransportConfigStore::registered_types().contains_key(&uid));
    assert!(BasisTransportConfigStore::is_type_registered(&uid));

    BasisTransportConfigStore::with_mut::<LNLTransportConfig, ()>(&uid, |c| c.ping_interval = 4343);
    BasisTransportConfigStore::register_type::<LNLTransportConfig>(&uid);
    assert_eq!(BasisTransportConfigStore::get::<LNLTransportConfig>(&uid).ping_interval, 4343);
}

// ── ConnectionTargetTests ── property-bag semantics.

#[test]
fn constructor_keeps_empty_ids_empty() {
    let target = ConnectionTarget::new("", "");
    assert_eq!(target.stack_id, "");
    assert_eq!(target.raw, "");
}

#[test]
fn set_and_get_keys_are_case_insensitive() {
    let mut target = ConnectionTarget::new("litenetlib", "raw");
    target.set("Address", "example.com");
    assert_eq!(target.get("ADDRESS").as_deref(), Some("example.com"));
    assert_eq!(target.get(ConnectionTargetKeys::ADDRESS).as_deref(), Some("example.com"));
    assert_eq!(target.try_get("address").as_deref(), Some("example.com"));
}

#[test]
fn set_empty_value_stores_empty_and_empty_key_is_ignored() {
    let mut target = ConnectionTarget::new("", "");
    target.set(ConnectionTargetKeys::PASSWORD, "");
    assert_eq!(target.get(ConnectionTargetKeys::PASSWORD).as_deref(), Some(""));

    target.set("", "value");
    assert_eq!(target.property_count(), 1);
    assert_eq!(target.properties().len(), 1);
}

#[test]
fn get_unknown_or_empty_key_returns_fallback() {
    let target = ConnectionTarget::new("", "");
    assert_eq!(target.get("missing"), None);
    assert_eq!(target.get_or("missing", Some("fallback")).as_deref(), Some("fallback"));
    assert_eq!(target.get_or("", Some("fallback")).as_deref(), Some("fallback"));
    assert_eq!(target.get_or("missing", None), None);
    assert_eq!(target.try_get("missing"), None);
    assert_eq!(target.try_get(""), None);
}

#[test]
fn key_constants_are_stable() {
    assert_eq!(ConnectionTargetKeys::ADDRESS, "address");
    assert_eq!(ConnectionTargetKeys::PORT, "port");
    assert_eq!(ConnectionTargetKeys::PASSWORD, "password");
    assert_eq!(ConnectionTargetKeys::LOBBY_ID, "lobbyId");
    assert_eq!(ConnectionTargetKeys::ENDPOINT_ID, "endpointId");
    assert_eq!(ConnectionTargetKeys::RELAY_URL, "relayUrl");
}

// ── LNLConnectionTargetParserTests ── host:port / [IPv6]:port / #password.

#[test]
fn try_parse_handles_hosts_ports_ipv6_and_passwords() {
    let cases: [(&str, &str, u16, bool, &str); 16] = [
        ("example.com:5000", "example.com", 5000, true, ""),
        ("example.com", "example.com", 4296, false, ""),
        ("192.168.1.5:4297", "192.168.1.5", 4297, true, ""),
        ("example.com:5000#hunter2", "example.com", 5000, true, "hunter2"),
        ("example.com#hunter2", "example.com", 4296, false, "hunter2"),
        ("[::1]:5001", "::1", 5001, true, ""),
        ("[2001:db8::1]", "2001:db8::1", 4296, false, ""),
        ("[2001:db8::1]:5002#pw", "2001:db8::1", 5002, true, "pw"),
        ("::1", "::1", 4296, false, ""),
        ("2001:db8::1", "2001:db8::1", 4296, false, ""),
        ("[::1", "[::1", 4296, false, ""),
        ("host:0", "host:0", 4296, false, ""),
        ("host:65536", "host:65536", 4296, false, ""),
        ("host:", "host:", 4296, false, ""),
        ("host:abc", "host:abc", 4296, false, ""),
        ("  example.com  ", "example.com", 4296, false, ""),
    ];
    for (raw, address, port, port_provided, password) in cases {
        let parsed = LNLConnectionTargetParser::try_parse_connection_string(raw).unwrap_or_else(|| panic!("{raw:?} must parse"));
        assert_eq!(parsed.address, address, "{raw:?}");
        assert_eq!(parsed.port, port, "{raw:?}");
        assert_eq!(parsed.port_provided, port_provided, "{raw:?}");
        assert_eq!(parsed.password, password, "{raw:?}");
    }
}

#[test]
fn try_parse_rejects_input_without_an_address() {
    for raw in ["", "#pw", "   "] {
        assert!(LNLConnectionTargetParser::try_parse_connection_string(raw).is_none(), "{raw:?}");
    }
}

#[test]
fn parse_populates_address_port_and_password() {
    let parser = LNLConnectionTargetParser;
    let mut target = ConnectionTarget::new(BasisNetworkStackRegistry::LITE_NET_LIB_ID, "example.com:5000#pw");
    parser.parse(&mut target);
    assert_eq!(target.get(ConnectionTargetKeys::ADDRESS).as_deref(), Some("example.com"));
    assert_eq!(target.get(ConnectionTargetKeys::PORT).as_deref(), Some("5000"));
    assert_eq!(target.get(ConnectionTargetKeys::PASSWORD).as_deref(), Some("pw"));
}

#[test]
fn parse_unparseable_raw_leaves_properties_unset() {
    let parser = LNLConnectionTargetParser;
    let mut target = ConnectionTarget::new(BasisNetworkStackRegistry::LITE_NET_LIB_ID, "");
    parser.parse(&mut target);
    assert_eq!(target.get(ConnectionTargetKeys::ADDRESS), None);
    assert_eq!(target.get(ConnectionTargetKeys::PORT), None);
}

#[test]
fn format_host_and_password() {
    let parser = LNLConnectionTargetParser;
    let mut target = ConnectionTarget::new("", "");
    target.set(ConnectionTargetKeys::ADDRESS, "example.com");
    target.set(ConnectionTargetKeys::PORT, "5000");
    assert_eq!(parser.format(&target), "example.com:5000");

    target.set(ConnectionTargetKeys::PASSWORD, "pw");
    assert_eq!(parser.format(&target), "example.com:5000#pw");
}

#[test]
fn format_brackets_ipv6_addresses() {
    let parser = LNLConnectionTargetParser;
    let mut target = ConnectionTarget::new("", "");
    target.set(ConnectionTargetKeys::ADDRESS, "::1");
    target.set(ConnectionTargetKeys::PORT, "5001");
    assert_eq!(parser.format(&target), "[::1]:5001");
}

#[test]
fn format_defaults_port_and_returns_empty_without_an_address() {
    let parser = LNLConnectionTargetParser;
    let mut target = ConnectionTarget::new("", "");
    target.set(ConnectionTargetKeys::ADDRESS, "127.0.0.1");
    assert_eq!(parser.format(&target), "127.0.0.1:4296");
    assert_eq!(parser.format(&ConnectionTarget::new("", "")), "");
}

#[test]
fn parse_format_parse_round_trips_address_port_and_password() {
    let parser = LNLConnectionTargetParser;
    for raw in ["example.com:5000#pw", "[2001:db8::1]:4296", "10.0.0.2:4297", "example.com"] {
        let mut first = ConnectionTarget::new(BasisNetworkStackRegistry::LITE_NET_LIB_ID, raw);
        parser.parse(&mut first);

        let mut second = ConnectionTarget::new(BasisNetworkStackRegistry::LITE_NET_LIB_ID, &parser.format(&first));
        parser.parse(&mut second);

        for key in [ConnectionTargetKeys::ADDRESS, ConnectionTargetKeys::PORT, ConnectionTargetKeys::PASSWORD] {
            assert_eq!(first.get(key), second.get(key), "{raw:?} {key}");
        }
    }
}

// ── NetworkStackRegistryTests ── all mutations use unique test stack ids; the active stack id is
// restored after each test that changes it.

struct RecordingParser;

impl IConnectionTargetParser for RecordingParser {
    fn parse(&self, _target: &mut ConnectionTarget) {}
    fn format(&self, _target: &ConnectionTarget) -> String {
        "custom".to_string()
    }
}

fn null_factory() -> basis_network_core::transport::basis_network_stack_registry::NetManagerFactory {
    Arc::new(|_: Arc<EventBasedNetListener>, _: &Configuration| None)
}

fn count_stack(id: &str) -> usize {
    BasisNetworkStackRegistry::stacks().iter().filter(|s| s.id.eq_ignore_ascii_case(id)).count()
}

#[test]
#[serial(stack_registry)]
fn default_stack_is_iroh_and_lite_net_lib_is_registered_too() {
    assert_eq!(BasisNetworkStackRegistry::LITE_NET_LIB_ID, "litenetlib");
    assert_eq!(BasisNetworkStackRegistry::IROH_ID, "iroh");
    assert_eq!(BasisNetworkStackRegistry::DEFAULT_ID, BasisNetworkStackRegistry::IROH_ID);
    assert!(BasisNetworkStackRegistry::is_registered(BasisNetworkStackRegistry::IROH_ID));
    assert!(BasisNetworkStackRegistry::is_registered(BasisNetworkStackRegistry::LITE_NET_LIB_ID));
    assert_eq!(BasisNetworkStackRegistry::get_display_name(BasisNetworkStackRegistry::LITE_NET_LIB_ID), "LiteNetLib");
    assert_eq!(count_stack(BasisNetworkStackRegistry::LITE_NET_LIB_ID), 1);
    assert_eq!(count_stack(BasisNetworkStackRegistry::IROH_ID), 1);
}

#[test]
#[serial(stack_registry)]
fn is_registered_handles_case_empty_and_unknown() {
    assert!(BasisNetworkStackRegistry::is_registered("LITENETLIB"));
    assert!(BasisNetworkStackRegistry::is_registered("IROH"));
    assert!(!BasisNetworkStackRegistry::is_registered(""));
    assert!(!BasisNetworkStackRegistry::is_registered(&format!("unknown-{}", uuid::Uuid::new_v4().simple())));
}

#[test]
#[serial(stack_registry)]
fn get_parser_falls_back_to_default_stack_parser() {
    let by_id = BasisNetworkStackRegistry::get_parser(BasisNetworkStackRegistry::DEFAULT_ID).expect("default parser");
    for id in ["", &format!("unknown-{}", uuid::Uuid::new_v4().simple())] {
        let fallback = BasisNetworkStackRegistry::get_parser(id).expect("fallback parser");
        assert!(Arc::ptr_eq(&by_id, &fallback), "{id:?}");
    }
    // The LiteNetLib stack has its own parser, distinct from the default's.
    let lnl = BasisNetworkStackRegistry::get_parser(BasisNetworkStackRegistry::LITE_NET_LIB_ID).expect("lnl parser");
    assert!(!Arc::ptr_eq(&by_id, &lnl));
}

#[test]
#[serial(stack_registry)]
fn register_duplicate_id_is_ignored() {
    BasisNetworkStackRegistry::register(BasisNetworkStackRegistry::LITE_NET_LIB_ID, "Imposter", null_factory());
    assert_eq!(BasisNetworkStackRegistry::get_display_name(BasisNetworkStackRegistry::LITE_NET_LIB_ID), "LiteNetLib");
    assert_eq!(count_stack(BasisNetworkStackRegistry::LITE_NET_LIB_ID), 1);
}

#[test]
#[serial(stack_registry)]
fn register_new_stack_supports_lookups_and_parser_registration() {
    let uid = new_stack_id();
    BasisNetworkStackRegistry::register(&uid, "Test Stack", null_factory());

    assert!(BasisNetworkStackRegistry::is_registered(&uid));
    assert_eq!(BasisNetworkStackRegistry::get_display_name(&uid), "Test Stack");
    assert!(BasisNetworkStackRegistry::stacks().iter().any(|s| s.id == uid && s.display_name == "Test Stack"));
    assert_eq!(BasisNetworkStackRegistry::canonical_id(&uid.to_uppercase()).as_deref(), Some(uid.as_str()));

    // no parser yet: falls back to the default stack's parser
    let default_parser = BasisNetworkStackRegistry::get_parser(BasisNetworkStackRegistry::DEFAULT_ID).expect("default parser");
    assert!(Arc::ptr_eq(&default_parser, &BasisNetworkStackRegistry::get_parser(&uid).expect("fallback")));

    let parser: Arc<dyn IConnectionTargetParser> = Arc::new(RecordingParser);
    BasisNetworkStackRegistry::register_parser(&uid, parser.clone());
    assert!(Arc::ptr_eq(&parser, &BasisNetworkStackRegistry::get_parser(&uid).expect("registered")));
}

#[test]
#[serial(stack_registry)]
fn get_display_name_falls_back_to_id_when_unknown_or_unnamed() {
    let uid = new_stack_id();
    BasisNetworkStackRegistry::register(&uid, "", null_factory());
    assert_eq!(BasisNetworkStackRegistry::get_display_name(&uid), uid);

    let unknown = format!("unknown-{}", uuid::Uuid::new_v4().simple());
    assert_eq!(BasisNetworkStackRegistry::get_display_name(&unknown), unknown);
    assert_eq!(BasisNetworkStackRegistry::get_display_name(""), BasisNetworkStackRegistry::get_display_name(BasisNetworkStackRegistry::DEFAULT_ID));
}

#[test]
#[serial(stack_registry)]
fn registration_with_an_empty_id_is_a_programming_error() {
    let before = BasisNetworkStackRegistry::stacks().len();
    let outcome = std::panic::catch_unwind(|| BasisNetworkStackRegistry::register("", "x", null_factory()));
    assert!(outcome.is_err(), "an empty stack id must be refused");
    let outcome = std::panic::catch_unwind(|| BasisNetworkStackRegistry::register_parser("", Arc::new(RecordingParser)));
    assert!(outcome.is_err());
    assert_eq!(BasisNetworkStackRegistry::stacks().len(), before);
    assert!(!BasisNetworkStackRegistry::is_registered(""));
}

#[test]
#[serial(stack_registry)]
fn registering_hooks_for_unknown_stack_warns_without_panicking() {
    let unknown = format!("unknown-{}", uuid::Uuid::new_v4().simple());
    BasisNetworkStackRegistry::register_parser(&unknown, Arc::new(RecordingParser));
    BasisNetworkStackRegistry::register_tick(&unknown, Arc::new(|| {}));
    BasisNetworkStackRegistry::register_probe(&unknown, Arc::new(|_, _| Box::pin(async { ServerProbeResult::default() })));
    assert!(!BasisNetworkStackRegistry::is_registered(&unknown));
}

#[test]
#[serial(stack_registry)]
fn set_active_stack_id_fires_event_only_on_real_change() {
    let original = BasisNetworkStackRegistry::active_stack_id();
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let handler: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |id| sink.lock().push(id.to_string()));
    BasisNetworkStackRegistry::subscribe_active_stack_changed(handler.clone());

    let uid = new_stack_id();
    BasisNetworkStackRegistry::set_active_stack_id(&uid);
    assert_eq!(BasisNetworkStackRegistry::active_stack_id(), uid);
    assert_eq!(*events.lock(), vec![uid.clone()]);

    BasisNetworkStackRegistry::set_active_stack_id(&uid);
    assert_eq!(events.lock().len(), 1);

    // case-only change is not a change
    BasisNetworkStackRegistry::set_active_stack_id(&uid.to_uppercase());
    assert_eq!(events.lock().len(), 1);
    assert_eq!(BasisNetworkStackRegistry::active_stack_id(), uid);

    BasisNetworkStackRegistry::unsubscribe_active_stack_changed(&handler);
    BasisNetworkStackRegistry::set_active_stack_id(&original);
    assert_eq!(events.lock().len(), 1, "an unsubscribed handler must not fire");
}

#[test]
#[serial(stack_registry)]
fn tick_active_invokes_registered_tick_for_the_active_stack_only() {
    let original = BasisNetworkStackRegistry::active_stack_id();
    let uid = new_stack_id();
    let ticks = Arc::new(AtomicUsize::new(0));
    BasisNetworkStackRegistry::register(&uid, "Tick Stack", null_factory());
    let counter = ticks.clone();
    BasisNetworkStackRegistry::register_tick(&uid, Arc::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    }));

    BasisNetworkStackRegistry::set_active_stack_id(&uid);
    BasisNetworkStackRegistry::tick_active();
    BasisNetworkStackRegistry::tick_active();
    assert_eq!(ticks.load(Ordering::Relaxed), 2);

    BasisNetworkStackRegistry::set_active_stack_id("");
    BasisNetworkStackRegistry::tick_active();
    assert_eq!(ticks.load(Ordering::Relaxed), 2);

    BasisNetworkStackRegistry::set_active_stack_id(&original);
}

struct StubIntroducer;

impl IPeerIntroducer for StubIntroducer {
    fn initialize(&self, _active_manager: &NetManagerRef) -> bool {
        true
    }
    fn introduce(&self, _a: &PeerIntroduction, _b: &PeerIntroduction, _token: &str) {}
    fn is_pair_offloaded(&self, _peer_id_a: i32, _peer_id_b: i32) -> bool {
        false
    }
    fn shutdown(&self) {}
}

#[test]
#[serial(stack_registry)]
fn create_introducer_uses_factory_registered_for_stack() {
    let uid = new_stack_id();
    let introducer: Arc<dyn IPeerIntroducer> = Arc::new(StubIntroducer);
    BasisNetworkStackRegistry::register(&uid, "Introducer Stack", null_factory());
    let handed_out = introducer.clone();
    BasisNetworkStackRegistry::register_introducer_factory(&uid, Arc::new(move |_| handed_out.clone()));

    let created = BasisNetworkStackRegistry::create_introducer(&uid, None).expect("introducer");
    assert!(Arc::ptr_eq(&introducer, &created));
    assert!(BasisNetworkStackRegistry::create_introducer(&new_stack_id(), None).is_none(), "a stack without a factory has no introducer");
}

#[test]
#[serial(stack_registry)]
fn probe_async_missing_target_reports_error() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let result = runtime.block_on(BasisNetworkStackRegistry::probe_async(None, 50));
    assert!(!result.reachable);
    assert_eq!(result.error, "Target is null");
}

#[test]
#[serial(stack_registry)]
fn probe_async_uses_probe_registered_for_stack() {
    let uid = new_stack_id();
    BasisNetworkStackRegistry::register(&uid, "Probe Stack", null_factory());
    BasisNetworkStackRegistry::register_probe(&uid, Arc::new(|_, timeout_ms| Box::pin(async move { ServerProbeResult { reachable: true, name: "probe-stub".to_string(), round_trip_ms: timeout_ms, ..Default::default() } })));

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let result = runtime.block_on(BasisNetworkStackRegistry::probe_async(Some(ConnectionTarget::new(&uid, "example.com")), 123));
    assert!(result.reachable);
    assert_eq!(result.name, "probe-stub");
    assert_eq!(result.round_trip_ms, 123);
}

#[test]
#[serial(stack_registry)]
fn probe_async_for_a_stack_without_a_probe_reports_error_rather_than_hanging() {
    let uid = new_stack_id();
    BasisNetworkStackRegistry::register(&uid, "Probeless Stack", null_factory());
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let result = runtime.block_on(BasisNetworkStackRegistry::probe_async(Some(ConnectionTarget::new(&uid, "example.com")), 50));
    assert!(!result.reachable);
    assert!(!result.error.is_empty());
}

// ── ServerMessageRegistryBindingTableTests ── read-only pins of the inbound binding table.

const EXPECTED_INBOUND_CHANNELS: [u8; 34] = [
    BasisNetworkCommons::AUTH_IDENTITY_CHANNEL,
    BasisNetworkCommons::VOICE_CHANNEL,
    BasisNetworkCommons::SHOUT_VOICE_CHANNEL,
    BasisNetworkCommons::AUDIO_RECIPIENTS_CHANNEL,
    BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL,
    BasisNetworkCommons::PLAYER_AVATAR_HIGH_ADDITIONAL_CHANNEL,
    BasisNetworkCommons::AVATAR_CHANGE_MESSAGE_CHANNEL,
    BasisNetworkCommons::AVATAR_CHANNEL,
    BasisNetworkCommons::CHAT_CHANNEL,
    BasisNetworkCommons::GET_CURRENT_OWNER_REQUEST_CHANNEL,
    BasisNetworkCommons::CHANGE_CURRENT_OWNER_REQUEST_CHANNEL,
    BasisNetworkCommons::REMOVE_CURRENT_OWNER_REQUEST_CHANNEL,
    BasisNetworkCommons::NET_ID_ASSIGN_CHANNEL,
    BasisNetworkCommons::SCENE_CHANNEL,
    BasisNetworkCommons::LOAD_RESOURCE_CHANNEL,
    BasisNetworkCommons::UNLOAD_RESOURCE_CHANNEL,
    BasisNetworkCommons::PRELOAD_READY_CHANNEL,
    BasisNetworkCommons::CONTENT_SHARE_CHANNEL,
    BasisNetworkCommons::DELTA_AVATAR_CHANNEL,
    BasisNetworkCommons::SERVER_BOUND_CHANNEL,
    BasisNetworkCommons::ADMIN_CHANNEL,
    BasisNetworkCommons::SERVER_STATISTICS_CHANNEL,
    BasisNetworkCommons::CAMERA_PIP_STATE_CHANNEL,
    BasisNetworkCommons::CAMERA_PIP_POSITION_CHANNEL,
    BasisNetworkCommons::EVENTS_CHANNEL,
    BasisNetworkCommons::AUDIO_RECIPIENTS_LARGE_CHANNEL,
    BasisNetworkCommons::AUDIO_RECIPIENTS_INVERTED_CHANNEL,
    BasisNetworkCommons::AUDIO_RECIPIENTS_INVERTED_LARGE_CHANNEL,
    BasisNetworkCommons::AUDIO_RECIPIENTS_BITFIELD_CHANNEL,
    BasisNetworkCommons::P2P_CHANNEL,
    BasisNetworkCommons::MODIFY_RESOURCE_CHANNEL,
    BasisNetworkCommons::DIRECT_SCENE_SERVER_CHANNEL,
    BasisNetworkCommons::DIRECT_AVATAR_SERVER_CHANNEL,
    BasisNetworkCommons::REGISTRY_CONTROL_CHANNEL,
];

#[test]
fn expected_inbound_channel_table_is_distinct_in_range_and_includes_uplink_delta() {
    let mut distinct = EXPECTED_INBOUND_CHANNELS.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), EXPECTED_INBOUND_CHANNELS.len());
    assert!(EXPECTED_INBOUND_CHANNELS.iter().all(|c| *c < BasisNetworkCommons::TOTAL_CHANNELS));
    assert!(EXPECTED_INBOUND_CHANNELS.contains(&BasisNetworkCommons::DELTA_AVATAR_CHANNEL));
}

#[test]
fn every_expected_inbound_channel_has_a_core_handler_bound() {
    BasisServerMessageRegistry::ensure_initialized();
    for channel in EXPECTED_INBOUND_CHANNELS {
        assert!(BasisServerMessageRegistry::resolve_core(channel).is_some(), "channel {channel} should have an inbound core handler bound");
    }
}

#[test]
fn avatar_movement_channels_share_one_handler() {
    BasisServerMessageRegistry::ensure_initialized();
    let high = BasisServerMessageRegistry::resolve_core(BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL).expect("high");
    let additional = BasisServerMessageRegistry::resolve_core(BasisNetworkCommons::PLAYER_AVATAR_HIGH_ADDITIONAL_CHANNEL).expect("additional");
    assert!(Arc::ptr_eq(&high, &additional));
}

#[test]
fn plugin_channels_have_no_core_binding_so_multiplex_dispatch_is_reachable() {
    BasisServerMessageRegistry::ensure_initialized();
    for channel in BasisNetworkCommons::PLUGIN_RELIABLE_CHANNEL..=BasisNetworkCommons::PLUGIN_UNRELIABLE_CHANNEL {
        assert!(BasisNetworkCommons::is_plugin_channel(channel));
        assert!(BasisServerMessageRegistry::resolve_core(channel).is_none());
    }
}

// ── ConfigXmlDocsTests ── doc-comment injection, version stamping, missing-field detection.

fn write_complete_config_file(path: &Path, cfg: &Configuration) {
    std::fs::write(path, BasisConfigXmlDocs::serialize(cfg).expect("serialize")).expect("write");
}

#[test]
fn serialize_injects_server_config_header_section_and_field_comments() {
    let xml = BasisConfigXmlDocs::serialize(&Configuration::default()).expect("serialize");
    assert!(xml.contains("<!--"));
    assert!(xml.contains("Basis dedicated-server configuration"));
    assert!(xml.contains("===== Networking / listener ====="));
    assert!(xml.contains("Maximum number of simultaneously connected peers"));
    assert!(xml.contains("<PeerLimit>65535</PeerLimit>"));
}

#[test]
fn serialize_injects_lnl_transport_doc_comments() {
    let xml = BasisConfigXmlDocs::serialize(&LNLTransportConfig::default()).expect("serialize");
    assert!(xml.contains("LiteNetLib transport tuning"));
    assert!(xml.contains("<MultiSocketCount>1</MultiSocketCount>"));
}

#[test]
fn serialize_then_deserialize_round_trips_through_the_doc_comments() {
    let mut expected = Configuration::default();
    mutate_all_fields(&mut expected);
    let xml = BasisConfigXmlDocs::serialize(&expected).expect("serialize");
    let loaded: Configuration = BasisConfigXmlDocs::deserialize(&xml).expect("deserialize");
    assert_fields_equal(&expected, &loaded, &[]);
}

#[test]
fn deserialize_refuses_malformed_input_and_the_wrong_root() {
    assert!(BasisConfigXmlDocs::deserialize::<Configuration>("not xml <<<>").is_err());
    assert!(BasisConfigXmlDocs::deserialize::<Configuration>("").is_err());
    assert!(BasisConfigXmlDocs::deserialize::<Configuration>("<LNLTransportConfig></LNLTransportConfig>").is_err());
    assert!(BasisConfigXmlDocs::deserialize::<Configuration>("<Configuration><PeerLimit>lots</PeerLimit></Configuration>").is_err());
}

basis_network_core::basis_xml_config! {
    /// A type with no registered docs, for the pass-through check.
    pub struct DocLessTestConfig ("DocLessTestConfig", 0) {
        pub value: i32 = 3 => "Value" [Int],
    }
}

#[test]
fn serialize_type_without_registered_docs_emits_no_comments() {
    let xml = BasisConfigXmlDocs::serialize(&DocLessTestConfig::default()).expect("serialize");
    assert!(!xml.contains("<!--"));
    assert!(xml.contains("<Value>3</Value>"));
    let back: DocLessTestConfig = BasisConfigXmlDocs::deserialize(&xml).expect("deserialize");
    assert_eq!(back.value, 3);
}

#[test]
fn stamp_version_sets_instance_to_type_current_version() {
    let mut server_cfg = Configuration::default();
    BasisConfigXmlDocs::stamp_version(&mut server_cfg);
    assert_eq!(server_cfg.config_version, Configuration::CURRENT_CONFIG_VERSION);
    assert_eq!(BasisConfigXmlDocs::read_version(&server_cfg), Configuration::CURRENT_CONFIG_VERSION);

    let mut lnl_cfg = LNLTransportConfig::default();
    BasisConfigXmlDocs::stamp_version(&mut lnl_cfg);
    assert_eq!(lnl_cfg.config_version(), LNLTransportConfig::CURRENT_CONFIG_VERSION);
}

#[test]
fn needs_upgrade_true_when_stamped_version_is_behind() {
    let cfg = Configuration::default(); // ConfigVersion 0 < CURRENT_CONFIG_VERSION
    assert!(BasisConfigXmlDocs::needs_upgrade(Path::new("/does/not/exist.xml"), &cfg));
}

#[test]
fn needs_upgrade_false_when_current_version_and_file_complete() {
    let dir = TempDir::new();
    let path = dir.file("config.xml");
    let mut cfg = Configuration::default();
    BasisConfigXmlDocs::stamp_version(&mut cfg);
    write_complete_config_file(&path, &cfg);

    assert!(!BasisConfigXmlDocs::needs_upgrade(&path, &cfg));
    assert!(!BasisConfigXmlDocs::is_missing_any_field::<Configuration>(&path));
}

#[test]
fn is_missing_any_field_detects_a_removed_element() {
    let dir = TempDir::new();
    let path = dir.file("config.xml");
    write_complete_config_file(&path, &Configuration::default());

    let xml = std::fs::read_to_string(&path).expect("read");
    let without_peer_limit: String = xml.lines().filter(|line| !line.contains("<PeerLimit>")).collect::<Vec<_>>().join("\n");
    assert_ne!(xml, without_peer_limit);
    std::fs::write(&path, without_peer_limit).expect("write");

    assert!(BasisConfigXmlDocs::is_missing_any_field::<Configuration>(&path));
}

#[test]
fn is_missing_any_field_missing_or_unreadable_file_reports_false() {
    let dir = TempDir::new();
    assert!(!BasisConfigXmlDocs::is_missing_any_field::<Configuration>(&dir.file("nope.xml")));

    let garbage = dir.file("garbage.xml");
    std::fs::write(&garbage, "not xml <<<>").expect("write");
    assert!(!BasisConfigXmlDocs::is_missing_any_field::<Configuration>(&garbage));
}
