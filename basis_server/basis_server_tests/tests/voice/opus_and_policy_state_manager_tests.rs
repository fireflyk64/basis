//! The server-wide policy state managers: Opus frame duration and FEC packet loss, per-user and
//! global Opus bitrate, headless connection and audio policy, crash reporting, audio range and
//! avatar scale ceilings — each with its admin-channel state push.

use basis_network_core::BasisNetworkCommons;
use basis_network_core::SerializableBasis::{AdminRequestMode, ClientMetaDataMessage};
use basis_network_core::configuration::Configuration;
use basis_network_core::transport::DeliveryMethod;
use basis_network_core::NetDataReader;
use basis_network_server::NetworkServer;
use basis_network_server::security::{BasisAudioRangeLimitManager, BasisAvatarScaleLimitManager, BasisCrashReportStateManager, BasisHeadlessAudioStateManager, BasisHeadlessConnectionPolicyManager, BasisOpusFrameDurationStateManager, BasisOpusPacketLossStateManager, BasisUserOpusBitrateStateManager};
use basis_server_tests::support::FakePeer;
use serial_test::serial;

fn single_send(peer: &FakePeer) -> (Vec<u8>, u8, DeliveryMethod) {
    let sent = peer.sent.lock();
    assert_eq!(sent.len(), 1, "expected exactly one send");
    (sent[0].data.clone(), sent[0].channel, sent[0].delivery)
}

// ── Opus frame duration: only 20 or 40 ms are accepted, anything else falls back to 20 ──

#[test]
fn is_accepted_duration_only_accepts_20_and_40() {
    for (ms, expected) in [(20, true), (40, true), (0, false), (-20, false), (10, false), (25, false), (60, false)] {
        assert_eq!(BasisOpusFrameDurationStateManager::is_accepted_duration(ms), expected, "{ms}");
    }
}

#[test]
#[serial(policy_statics)]
fn set_frame_duration_ms_switches_between_20_and_40_and_reports_changes() {
    BasisOpusFrameDurationStateManager::set_frame_duration_ms(20);
    assert!(BasisOpusFrameDurationStateManager::set_frame_duration_ms(40));
    assert_eq!(BasisOpusFrameDurationStateManager::frame_duration_ms(), 40);
    assert!(!BasisOpusFrameDurationStateManager::set_frame_duration_ms(40));
    assert!(BasisOpusFrameDurationStateManager::set_frame_duration_ms(20));
    assert_eq!(BasisOpusFrameDurationStateManager::frame_duration_ms(), 20);
    BasisOpusFrameDurationStateManager::set_frame_duration_ms(BasisOpusFrameDurationStateManager::DEFAULT_MS);
}

#[test]
#[serial(policy_statics)]
fn set_frame_duration_ms_rejected_durations_fall_back_to_the_default() {
    BasisOpusFrameDurationStateManager::set_frame_duration_ms(40);
    assert!(BasisOpusFrameDurationStateManager::set_frame_duration_ms(60)); // rejected -> default, changed from 40
    assert_eq!(BasisOpusFrameDurationStateManager::frame_duration_ms(), BasisOpusFrameDurationStateManager::DEFAULT_MS);
    assert!(!BasisOpusFrameDurationStateManager::set_frame_duration_ms(10)); // rejected -> default, already there
    assert_eq!(BasisOpusFrameDurationStateManager::frame_duration_ms(), 20);
    BasisOpusFrameDurationStateManager::set_frame_duration_ms(BasisOpusFrameDurationStateManager::DEFAULT_MS);
}

#[test]
#[serial(policy_statics)]
fn frame_duration_send_state_to_peer_writes_mode_byte_then_duration_byte() {
    BasisOpusFrameDurationStateManager::set_frame_duration_ms(40);
    let peer = FakePeer::new(10);
    BasisOpusFrameDurationStateManager::send_state_to_peer(&peer.as_ref());

    let (data, channel, delivery) = single_send(&peer);
    assert_eq!(data, vec![AdminRequestMode::GlobalGetOpusFrameDurationState as u8, 40]);
    assert_eq!(channel, BasisNetworkCommons::ADMIN_CHANNEL);
    assert_eq!(delivery, DeliveryMethod::ReliableOrdered);

    BasisOpusFrameDurationStateManager::broadcast_state(); // zero connected peers: must be a safe no-op
    BasisOpusFrameDurationStateManager::set_frame_duration_ms(BasisOpusFrameDurationStateManager::DEFAULT_MS);
}

// ── Opus FEC packet-loss percentage: clamped into 0..100 ──

#[test]
#[serial(policy_statics)]
fn set_packet_loss_percent_clamps_into_0_to_100() {
    for (requested, expected) in [(-5, 0), (0, 0), (55, 55), (100, 100), (150, 100)] {
        BasisOpusPacketLossStateManager::set_packet_loss_percent(requested);
        assert_eq!(BasisOpusPacketLossStateManager::packet_loss_percent(), expected, "{requested}");
    }
    BasisOpusPacketLossStateManager::set_packet_loss_percent(10);
}

#[test]
#[serial(policy_statics)]
fn set_packet_loss_percent_reports_only_real_changes() {
    BasisOpusPacketLossStateManager::set_packet_loss_percent(10);
    assert!(BasisOpusPacketLossStateManager::set_packet_loss_percent(33));
    assert!(!BasisOpusPacketLossStateManager::set_packet_loss_percent(33));
    assert!(BasisOpusPacketLossStateManager::set_packet_loss_percent(100));
    assert!(!BasisOpusPacketLossStateManager::set_packet_loss_percent(133)); // clamps onto the value already stored
    assert!(BasisOpusPacketLossStateManager::set_packet_loss_percent(10));
}

#[test]
#[serial(policy_statics)]
fn packet_loss_send_state_to_peer_writes_mode_byte_then_percent_byte() {
    BasisOpusPacketLossStateManager::set_packet_loss_percent(37);
    let peer = FakePeer::new(11);
    BasisOpusPacketLossStateManager::send_state_to_peer(&peer.as_ref());

    let (data, channel, delivery) = single_send(&peer);
    assert_eq!(data, vec![AdminRequestMode::GlobalGetOpusPacketLossState as u8, 37]);
    assert_eq!(channel, BasisNetworkCommons::ADMIN_CHANNEL);
    assert_eq!(delivery, DeliveryMethod::ReliableOrdered);

    BasisOpusPacketLossStateManager::broadcast_state();
    BasisOpusPacketLossStateManager::set_packet_loss_percent(10);
}

// ── Per-user Opus bitrate overrides keyed by net id plus the session-wide global value ──

#[test]
#[serial(policy_statics)]
fn set_bitrate_clamps_into_the_voice_range() {
    const MIN: i32 = BasisUserOpusBitrateStateManager::MIN_BITRATE;
    const MAX: i32 = BasisUserOpusBitrateStateManager::MAX_BITRATE;
    for (requested, expected) in [(MIN, MIN), (1, MIN), (5999, MIN), (240000, 240000), (MAX, MAX), (i32::MAX, MAX)] {
        let net_id = 701000 + requested % 97;
        assert_eq!(BasisUserOpusBitrateStateManager::set_bitrate(net_id, requested), expected, "{requested}");
        assert_eq!(BasisUserOpusBitrateStateManager::try_get_bitrate(net_id), Some(expected));
        BasisUserOpusBitrateStateManager::clear_for_peer(net_id);
    }
}

#[test]
#[serial(policy_statics)]
fn set_bitrate_zero_or_negative_clears_the_override() {
    const NET_ID: i32 = 702001;
    BasisUserOpusBitrateStateManager::set_bitrate(NET_ID, 32000);
    assert_eq!(BasisUserOpusBitrateStateManager::set_bitrate(NET_ID, 0), 0);
    assert!(BasisUserOpusBitrateStateManager::try_get_bitrate(NET_ID).is_none());

    BasisUserOpusBitrateStateManager::set_bitrate(NET_ID, 32000);
    assert_eq!(BasisUserOpusBitrateStateManager::set_bitrate(NET_ID, -5000), 0);
    assert!(BasisUserOpusBitrateStateManager::try_get_bitrate(NET_ID).is_none());
}

#[test]
#[serial(policy_statics)]
fn clear_for_peer_removes_the_override() {
    const NET_ID: i32 = 702002;
    BasisUserOpusBitrateStateManager::set_bitrate(NET_ID, 48000);
    assert!(BasisUserOpusBitrateStateManager::try_get_bitrate(NET_ID).is_some());
    BasisUserOpusBitrateStateManager::clear_for_peer(NET_ID);
    assert!(BasisUserOpusBitrateStateManager::try_get_bitrate(NET_ID).is_none());
}

#[test]
#[serial(policy_statics)]
fn set_global_bitrate_clamps_and_zero_clears() {
    assert_eq!(BasisUserOpusBitrateStateManager::set_global_bitrate(48000), 48000);
    assert_eq!(BasisUserOpusBitrateStateManager::global_bitrate(), 48000);
    assert_eq!(BasisUserOpusBitrateStateManager::set_global_bitrate(100), BasisUserOpusBitrateStateManager::MIN_BITRATE);
    assert_eq!(BasisUserOpusBitrateStateManager::set_global_bitrate(i32::MAX), BasisUserOpusBitrateStateManager::MAX_BITRATE);
    assert_eq!(BasisUserOpusBitrateStateManager::set_global_bitrate(-1), 0);
    assert_eq!(BasisUserOpusBitrateStateManager::global_bitrate(), 0);
}

#[test]
#[serial(policy_statics)]
fn effective_bitrate_per_user_override_wins_over_the_global() {
    const OVERRIDDEN: i32 = 703001;
    const PLAIN: i32 = 703002;
    BasisUserOpusBitrateStateManager::set_global_bitrate(0);
    assert_eq!(BasisUserOpusBitrateStateManager::effective_bitrate_for(PLAIN), 0);

    BasisUserOpusBitrateStateManager::set_global_bitrate(64000);
    assert_eq!(BasisUserOpusBitrateStateManager::effective_bitrate_for(PLAIN), 64000);

    BasisUserOpusBitrateStateManager::set_bitrate(OVERRIDDEN, 24000);
    assert_eq!(BasisUserOpusBitrateStateManager::effective_bitrate_for(OVERRIDDEN), 24000);
    assert_eq!(BasisUserOpusBitrateStateManager::effective_bitrate_for(PLAIN), 64000);

    BasisUserOpusBitrateStateManager::set_bitrate(OVERRIDDEN, 0);
    assert_eq!(BasisUserOpusBitrateStateManager::effective_bitrate_for(OVERRIDDEN), 64000);

    BasisUserOpusBitrateStateManager::clear_for_peer(OVERRIDDEN);
    BasisUserOpusBitrateStateManager::clear_for_peer(PLAIN);
    BasisUserOpusBitrateStateManager::set_global_bitrate(0);
}

#[test]
#[serial(policy_statics)]
fn parallel_set_query_clear_storm_leaves_consistent_state() {
    const BASE_ID: i32 = 704000;
    std::thread::scope(|scope| {
        for worker in 0..8 {
            scope.spawn(move || {
                for step in 0..512 {
                    let i = worker * 512 + step;
                    let net_id = BASE_ID + (i & 63);
                    match i % 3 {
                        0 => {
                            BasisUserOpusBitrateStateManager::set_bitrate(net_id, 6000 + (i % 1000) * 100);
                        }
                        1 => {
                            if let Some(seen) = BasisUserOpusBitrateStateManager::try_get_bitrate(net_id) {
                                assert!((BasisUserOpusBitrateStateManager::MIN_BITRATE..=BasisUserOpusBitrateStateManager::MAX_BITRATE).contains(&seen));
                            }
                        }
                        _ => BasisUserOpusBitrateStateManager::clear_for_peer(net_id),
                    }
                }
            });
        }
    });
    for offset in 0..64 {
        let net_id = BASE_ID + offset;
        assert_eq!(BasisUserOpusBitrateStateManager::set_bitrate(net_id, 32000), 32000);
        assert_eq!(BasisUserOpusBitrateStateManager::try_get_bitrate(net_id), Some(32000));
        BasisUserOpusBitrateStateManager::clear_for_peer(net_id);
    }
}

#[test]
#[serial(policy_statics)]
fn send_state_to_peer_pushes_that_peers_effective_bitrate() {
    let peer = FakePeer::new(705001);
    BasisUserOpusBitrateStateManager::set_global_bitrate(48000);
    BasisUserOpusBitrateStateManager::set_bitrate(705001, 24000);
    BasisUserOpusBitrateStateManager::send_state_to_peer(&peer.as_ref());

    let (data, channel, _) = single_send(&peer);
    let mut reader = NetDataReader::from_slice(&data);
    assert_eq!(reader.get_byte().expect("mode"), AdminRequestMode::UserOpusBitrateOverride as u8);
    assert_eq!(reader.get_int().expect("bitrate"), 24000);
    assert_eq!(reader.available_bytes(), 0);
    assert_eq!(channel, BasisNetworkCommons::ADMIN_CHANNEL);

    peer.clear_sent();
    BasisUserOpusBitrateStateManager::clear_for_peer(705001);
    BasisUserOpusBitrateStateManager::send_state_to_peer(&peer.as_ref());

    let (data, _, _) = single_send(&peer);
    let mut reader = NetDataReader::from_slice(&data);
    assert_eq!(reader.get_byte().expect("mode"), AdminRequestMode::UserOpusBitrateOverride as u8);
    assert_eq!(reader.get_int().expect("bitrate"), 48000); // no per-user override left: falls back to the global

    BasisUserOpusBitrateStateManager::clear_for_peer(705001);
    BasisUserOpusBitrateStateManager::set_global_bitrate(0);
}

#[test]
#[serial(policy_statics)]
fn send_global_state_to_peer_writes_mode_byte_then_global_bitrate() {
    let peer = FakePeer::new(705002);
    BasisUserOpusBitrateStateManager::set_global_bitrate(96000);
    BasisUserOpusBitrateStateManager::send_global_state_to_peer(&peer.as_ref());

    let (data, _, _) = single_send(&peer);
    let mut reader = NetDataReader::from_slice(&data);
    assert_eq!(reader.get_byte().expect("mode"), AdminRequestMode::GlobalGetOpusBitrateState as u8);
    assert_eq!(reader.get_int().expect("bitrate"), 96000);
    assert_eq!(reader.available_bytes(), 0);

    BasisUserOpusBitrateStateManager::broadcast_global_state();
    BasisUserOpusBitrateStateManager::set_global_bitrate(0);
}

#[test]
#[serial(network_statics)]
fn push_effective_to_all_peers_with_no_connected_peers_does_nothing() {
    assert!(NetworkServer::peer_snapshot().is_empty());
    BasisUserOpusBitrateStateManager::push_effective_to_all_peers();
}

// ── Headless connection policy ──

#[test]
fn is_headless_platform_matches_the_four_server_platform_ids() {
    for (platform, expected) in [
        ("", false),
        ("   ", false),
        ("Headless", true),
        ("headless", true),
        ("HEADLESS", true),
        ("WindowsServer", true),
        ("windowsserver", true),
        ("LinuxServer", true),
        ("OSXServer", true),
        ("Headless ", false), // exact match, no trimming
        ("Windows", false),
        ("Android", false),
        ("Server", false),
    ] {
        assert_eq!(BasisHeadlessConnectionPolicyManager::is_headless_platform(platform), expected, "{platform:?}");
    }
}

#[test]
fn is_headless_client_reads_the_platform_field_of_the_meta_data() {
    let headless = ClientMetaDataMessage { player_platform: "LinuxServer".into(), ..Default::default() };
    assert!(BasisHeadlessConnectionPolicyManager::is_headless_client(&headless));
    let desktop = ClientMetaDataMessage { player_platform: "Windows".into(), ..Default::default() };
    assert!(!BasisHeadlessConnectionPolicyManager::is_headless_client(&desktop));
}

#[test]
#[serial(policy_statics)]
fn headless_initialize_from_config_and_set_disallow_headless_track_changes() {
    BasisHeadlessConnectionPolicyManager::initialize_from_config(false);
    assert!(!BasisHeadlessConnectionPolicyManager::headless_disallowed());

    assert!(BasisHeadlessConnectionPolicyManager::set_disallow_headless(true));
    assert!(BasisHeadlessConnectionPolicyManager::headless_disallowed());
    assert!(!BasisHeadlessConnectionPolicyManager::set_disallow_headless(true));
    assert!(BasisHeadlessConnectionPolicyManager::set_disallow_headless(false));
    assert!(!BasisHeadlessConnectionPolicyManager::headless_disallowed());

    BasisHeadlessConnectionPolicyManager::initialize_from_config(true);
    assert!(BasisHeadlessConnectionPolicyManager::headless_disallowed());
    BasisHeadlessConnectionPolicyManager::initialize_from_config(false);
}

#[test]
#[serial(network_statics)]
fn disconnect_connected_headless_peers_with_no_peers_is_a_no_op() {
    assert!(NetworkServer::peer_snapshot().is_empty());
    BasisHeadlessConnectionPolicyManager::disconnect_connected_headless_peers();
}

#[test]
#[serial(policy_statics)]
fn headless_send_state_to_peer_writes_mode_byte_then_disallow_flag() {
    BasisHeadlessConnectionPolicyManager::set_disallow_headless(true);
    let peer = FakePeer::new(12);
    BasisHeadlessConnectionPolicyManager::send_state_to_peer(&peer.as_ref());

    let (data, channel, _) = single_send(&peer);
    assert_eq!(data, vec![AdminRequestMode::GlobalGetHeadlessDisallowState as u8, 1]);
    assert_eq!(channel, BasisNetworkCommons::ADMIN_CHANNEL);

    BasisHeadlessConnectionPolicyManager::broadcast_state();
    BasisHeadlessConnectionPolicyManager::set_disallow_headless(false);
}

// ── Headless audio playback toggle ──

#[test]
#[serial(policy_statics)]
fn set_headless_audio_toggles_and_reports_only_real_changes() {
    BasisHeadlessAudioStateManager::set_headless_audio(false);
    assert!(!BasisHeadlessAudioStateManager::headless_audio_off());

    assert!(BasisHeadlessAudioStateManager::set_headless_audio(true));
    assert!(BasisHeadlessAudioStateManager::headless_audio_off());
    assert!(!BasisHeadlessAudioStateManager::set_headless_audio(true));
    assert!(BasisHeadlessAudioStateManager::set_headless_audio(false));
    assert!(!BasisHeadlessAudioStateManager::headless_audio_off());
}

#[test]
#[serial(policy_statics)]
fn headless_audio_send_state_to_peer_writes_mode_byte_then_off_flag() {
    BasisHeadlessAudioStateManager::set_headless_audio(true);
    let peer = FakePeer::new(13);
    BasisHeadlessAudioStateManager::send_state_to_peer(&peer.as_ref());

    let (data, channel, delivery) = single_send(&peer);
    assert_eq!(data, vec![AdminRequestMode::GlobalGetHeadlessAudioState as u8, 1]);
    assert_eq!(channel, BasisNetworkCommons::ADMIN_CHANNEL);
    assert_eq!(delivery, DeliveryMethod::ReliableOrdered);

    BasisHeadlessAudioStateManager::broadcast_state();
    BasisHeadlessAudioStateManager::set_headless_audio(false);
}

// ── Client crash/error reporting toggle ──

#[test]
#[serial(policy_statics)]
fn crash_report_initialize_from_config_seeds_from_crash_reporting_enabled() {
    BasisCrashReportStateManager::initialize_from_config(&Configuration::default()); // default config ships with reporting on
    assert!(BasisCrashReportStateManager::enabled());
    BasisCrashReportStateManager::initialize_from_config(&Configuration { crash_reporting_enabled: false, ..Configuration::default() });
    assert!(!BasisCrashReportStateManager::enabled());
    BasisCrashReportStateManager::set_enabled(true);
}

#[test]
#[serial(policy_statics)]
fn crash_report_set_enabled_reports_only_real_changes() {
    BasisCrashReportStateManager::set_enabled(true);
    assert!(BasisCrashReportStateManager::set_enabled(false));
    assert!(!BasisCrashReportStateManager::enabled());
    assert!(!BasisCrashReportStateManager::set_enabled(false));
    assert!(BasisCrashReportStateManager::set_enabled(true));
    assert!(BasisCrashReportStateManager::enabled());
}

#[test]
#[serial(policy_statics)]
fn crash_report_send_state_to_peer_writes_mode_byte_then_enabled_flag() {
    BasisCrashReportStateManager::set_enabled(false);
    let peer = FakePeer::new(14);
    BasisCrashReportStateManager::send_state_to_peer(&peer.as_ref());

    let (data, channel, _) = single_send(&peer);
    assert_eq!(data, vec![AdminRequestMode::GlobalGetCrashReportState as u8, 0]);
    assert_eq!(channel, BasisNetworkCommons::ADMIN_CHANNEL);

    BasisCrashReportStateManager::broadcast_state();
    BasisCrashReportStateManager::set_enabled(true);
}

// ── Microphone / hearing range ceilings ──

#[test]
#[serial(policy_statics)]
fn audio_range_set_limits_replaces_non_positive_values_with_the_default() {
    for (mic, hearing, expected_mic, expected_hearing) in [(10.0f32, 20.0f32, 10.0f32, 20.0f32), (0.0, 5.0, 25.0, 5.0), (5.0, 0.0, 5.0, 25.0), (-1.0, -2.0, 25.0, 25.0), (0.5, 4000.0, 0.5, 4000.0)] {
        BasisAudioRangeLimitManager::set_limits(mic, hearing);
        assert_eq!(BasisAudioRangeLimitManager::max_microphone_range_meters(), expected_mic);
        assert_eq!(BasisAudioRangeLimitManager::max_hearing_range_meters(), expected_hearing);
    }
    BasisAudioRangeLimitManager::set_limits(25.0, 25.0);
}

#[test]
#[serial(policy_statics)]
fn audio_range_set_limits_reports_only_real_changes() {
    BasisAudioRangeLimitManager::set_limits(25.0, 25.0);
    assert!(!BasisAudioRangeLimitManager::set_limits(25.0, 25.0));
    assert!(!BasisAudioRangeLimitManager::set_limits(0.0, -1.0)); // sanitized straight back to the 25 m default
    assert!(BasisAudioRangeLimitManager::set_limits(12.0, 25.0));
    assert!(BasisAudioRangeLimitManager::set_limits(25.0, 25.0));
}

#[test]
#[serial(policy_statics)]
fn audio_range_initialize_from_config_applies_the_configured_ceilings() {
    BasisAudioRangeLimitManager::initialize_from_config(&Configuration { max_microphone_range_meters: 12.0, max_hearing_range_meters: 34.0, ..Configuration::default() });
    assert_eq!(BasisAudioRangeLimitManager::max_microphone_range_meters(), 12.0);
    assert_eq!(BasisAudioRangeLimitManager::max_hearing_range_meters(), 34.0);

    BasisAudioRangeLimitManager::initialize_from_config(&Configuration::default());
    assert_eq!(BasisAudioRangeLimitManager::max_microphone_range_meters(), 25.0);
    assert_eq!(BasisAudioRangeLimitManager::max_hearing_range_meters(), 25.0);
    BasisAudioRangeLimitManager::set_limits(25.0, 25.0);
}

#[test]
#[serial(policy_statics)]
fn audio_range_send_state_to_peer_writes_mode_byte_then_both_ranges() {
    BasisAudioRangeLimitManager::set_limits(12.5, 40.25);
    let peer = FakePeer::new(15);
    BasisAudioRangeLimitManager::send_state_to_peer(&peer.as_ref());

    let (data, channel, _) = single_send(&peer);
    let mut reader = NetDataReader::from_slice(&data);
    assert_eq!(reader.get_byte().expect("mode"), AdminRequestMode::GlobalGetAudioRangeLimits as u8);
    assert_eq!(reader.get_float().expect("mic"), 12.5);
    assert_eq!(reader.get_float().expect("hearing"), 40.25);
    assert_eq!(reader.available_bytes(), 0);
    assert_eq!(channel, BasisNetworkCommons::ADMIN_CHANNEL);

    BasisAudioRangeLimitManager::broadcast_state();
    BasisAudioRangeLimitManager::set_limits(25.0, 25.0);
}

// ── Avatar eye-height scale limits ──

#[test]
#[serial(policy_statics)]
fn avatar_scale_set_limits_sanitizes_and_keeps_min_at_or_below_max() {
    for (min, max, expected_min, expected_max) in [
        (0.5f32, 2.0f32, 0.5f32, 2.0f32),
        (f32::NAN, 2.0, 0.1, 2.0),
        (0.5, f32::NAN, 0.5, 100.0),
        (f32::INFINITY, 50.0, 0.1, 50.0),
        (0.5, f32::NEG_INFINITY, 0.5, 100.0),
        (0.0, 0.0, 0.1, 100.0),
        (-3.0, -3.0, 0.1, 100.0),
        (0.005, 5.0, 0.01, 5.0),
        (1.0, 5000.0, 1.0, 1000.0),
        (5.0, 2.0, 5.0, 5.0),
    ] {
        BasisAvatarScaleLimitManager::set_limits(min, max);
        assert_eq!(BasisAvatarScaleLimitManager::min_meters(), expected_min, "{min} {max}");
        assert_eq!(BasisAvatarScaleLimitManager::max_meters(), expected_max, "{min} {max}");
    }
    BasisAvatarScaleLimitManager::set_limits(0.1, 100.0);
}

#[test]
#[serial(policy_statics)]
fn avatar_scale_set_limits_reports_only_real_changes() {
    BasisAvatarScaleLimitManager::set_limits(0.25, 3.0);
    assert!(!BasisAvatarScaleLimitManager::set_limits(0.25, 3.0));
    assert!(BasisAvatarScaleLimitManager::set_limits(0.25, 4.0));
    assert!(BasisAvatarScaleLimitManager::set_limits(0.1, 100.0));
    assert!(!BasisAvatarScaleLimitManager::set_limits(f32::NAN, f32::NAN)); // sanitizes onto the defaults already stored
}

#[test]
#[serial(policy_statics)]
fn avatar_scale_initialize_from_config_uses_the_configured_eye_height_range() {
    BasisAvatarScaleLimitManager::initialize_from_config(&Configuration { min_avatar_eye_height_meters: 0.5, max_avatar_eye_height_meters: 3.0, ..Configuration::default() });
    assert_eq!(BasisAvatarScaleLimitManager::min_meters(), 0.5);
    assert_eq!(BasisAvatarScaleLimitManager::max_meters(), 3.0);

    BasisAvatarScaleLimitManager::initialize_from_config(&Configuration::default());
    assert_eq!(BasisAvatarScaleLimitManager::min_meters(), 0.1);
    assert_eq!(BasisAvatarScaleLimitManager::max_meters(), 100.0);
    BasisAvatarScaleLimitManager::set_limits(0.1, 100.0);
}

#[test]
#[serial(policy_statics)]
fn avatar_scale_send_state_to_peer_writes_mode_byte_then_min_and_max() {
    BasisAvatarScaleLimitManager::set_limits(0.25, 8.0);
    let peer = FakePeer::new(16);
    BasisAvatarScaleLimitManager::send_state_to_peer(&peer.as_ref());

    let (data, channel, _) = single_send(&peer);
    let mut reader = NetDataReader::from_slice(&data);
    assert_eq!(reader.get_byte().expect("mode"), AdminRequestMode::GlobalGetAvatarScaleLimits as u8);
    assert_eq!(reader.get_float().expect("min"), 0.25);
    assert_eq!(reader.get_float().expect("max"), 8.0);
    assert_eq!(reader.available_bytes(), 0);
    assert_eq!(channel, BasisNetworkCommons::ADMIN_CHANNEL);

    BasisAvatarScaleLimitManager::broadcast_state();
    BasisAvatarScaleLimitManager::set_limits(0.1, 100.0);
}
