use std::path::{Path, PathBuf};

use crate::BNL;
use crate::identity::BasisUserRestrictionMode;
use crate::transport::basis_network_stack_registry::BasisNetworkStackRegistry;

use super::basis_config_xml_docs::{BasisConfigXmlDocs, ConfigXmlError};
use super::basis_transport_config_store::BasisTransportConfigStore;
use super::{BasisXmlConfig, FieldKind};
use crate::basis_xml_config;

basis_xml_config! {
    pub struct Configuration ("Configuration", Configuration::CURRENT_CONFIG_VERSION) {
        pub config_version: i32 = 0 => "ConfigVersion" [Int],
        pub peer_limit: i32 = u16::MAX as i32 => "PeerLimit" [Int],
        pub set_port: u16 = 4296 => "SetPort" [UShort],
        pub server_name: String = "Basis Server".to_string() => "ServerName" [Str],
        pub server_motd: String = "".to_string() => "ServerMotd" [Str],
        pub enable_statistics: bool = true => "EnableStatistics" [Bool],
        pub has_file_support: bool = true => "HasFileSupport" [Bool],
        pub health_check_host: String = "localhost".to_string() => "HealthCheckHost" [Str],
        pub health_check_port: u16 = 10666 => "HealthCheckPort" [UShort],
        pub health_path: String = "/health".to_string() => "HealthPath" [Str],
        pub health_include_bsr_profiling: bool = false => "HealthIncludeBSRProfiling" [Bool],
        pub idle_memory_reclaim_enabled: bool = true => "IdleMemoryReclaimEnabled" [Bool],
        pub idle_memory_reclaim_settle_seconds: i32 = 30 => "IdleMemoryReclaimSettleSeconds" [Int],
        pub idle_memory_reclaim_minimum_peak: i32 = 8 => "IdleMemoryReclaimMinimumPeak" [Int],
        pub bsrs_millisecond_default_interval: i32 = 50 => "BSRSMillisecondDefaultInterval" [Int],
        pub bsr_base_multiplier: i32 = 1 => "BSRBaseMultiplier" [Int],
        pub bsrs_increase_rate: f32 = 0.005 => "BSRSIncreaseRate" [Float],
        pub bsr_slowest_send_rate: f32 = 2.55 => "BSRSlowestSendRate" [Float],
        pub distance_update_interval_ticks: i32 = 125 => "DistanceUpdateIntervalTicks" [Int],
        pub enable_compute_offload: bool = true => "EnableComputeOffload" [Bool],
        pub compute_device: String = "".to_string() => "ComputeDevice" [Str],
        pub compute_distance_update_interval_ticks: i32 = 32 => "ComputeDistanceUpdateIntervalTicks" [Int],
        pub high_quality_distance: f32 = 10.0 => "HighQualityDistance" [Float],
        pub medium_quality_distance: f32 = 20.0 => "MediumQualityDistance" [Float],
        pub low_quality_distance: f32 = 40.0 => "LowQualityDistance" [Float],
        pub override_auto_discovery_of_ipv: bool = false => "OverrideAutoDiscoveryOfIpv" [Bool],
        pub i_pv4_address: String = "0.0.0.0".to_string() => "IPv4Address" [Str],
        pub i_pv6_address: String = "::".to_string() => "IPv6Address" [Str],
        pub password: String = "default_password".to_string() => "Password" [Str],
        pub use_auth: bool = true => "UseAuth" [Bool],
        pub use_auth_identity: bool = true => "UseAuthIdentity" [Bool],
        pub network_stack_id: String = "".to_string() => "NetworkStackId" [Str],
        pub basis_user_restriction_mode: BasisUserRestrictionMode = "".to_string() => "BasisUserRestrictionMode" [RestrictionMode],
        pub how_many_duplicate_auth_can_exist: i32 = 2 => "HowManyDuplicateAuthCanExist" [Int],
        pub auth_validation_time_out_miliseconds: i32 = 9000 => "AuthValidationTimeOutMiliseconds" [Int],
        pub enable_console: bool = true => "EnableConsole" [Bool],
        pub enable_avatar_bundle_compression: bool = true => "EnableAvatarBundleCompression" [Bool],
        pub avatar_bundle_min_messages: i32 = 2 => "AvatarBundleMinMessages" [Int],
        pub avatar_bundle_min_bytes: i32 = 128 => "AvatarBundleMinBytes" [Int],
        pub enable_avatar_bundle_zstd: bool = true => "EnableAvatarBundleZstd" [Bool],
        pub avatar_bundle_zstd_delta_bundles: bool = false => "AvatarBundleZstdDeltaBundles" [Bool],
        pub avatar_bundle_zstd_level: i32 = -2 => "AvatarBundleZstdLevel" [Int],
        pub avatar_bundle_zstd_max_shed_tier: i32 = 1 => "AvatarBundleZstdMaxShedTier" [Int],
        pub enable_avatar_delta_compression: bool = true => "EnableAvatarDeltaCompression" [Bool],
        pub avatar_delta_keyframe_interval_ms: i32 = 500 => "AvatarDeltaKeyframeIntervalMs" [Int],
        pub avatar_delta_keyframe_max_interval_ms: i32 = 2000 => "AvatarDeltaKeyframeMaxIntervalMs" [Int],
        pub strip_additional_data_at_low_quality: bool = true => "StripAdditionalDataAtLowQuality" [Bool],
        pub enable_uplink_avatar_delta: bool = true => "EnableUplinkAvatarDelta" [Bool],
        pub image_cache_enabled: bool = true => "ImageCacheEnabled" [Bool],
        pub image_cache_max_megabytes: i32 = 512 => "ImageCacheMaxMegabytes" [Int],
        pub image_cache_minimum_per_owner_megabytes: i32 = 32 => "ImageCacheMinimumPerOwnerMegabytes" [Int],
        pub image_share_egress_megabits_per_second: i32 = 200 => "ImageShareEgressMegabitsPerSecond" [Int],
        pub image_share_download_megabits_per_second: i32 = 200 => "ImageShareDownloadMegabitsPerSecond" [Int],
        pub image_share_egress_enforcement_percent: i32 = 150 => "ImageShareEgressEnforcementPercent" [Int],
        pub image_pickup_range_meters: f32 = 64.0 => "ImagePickupRangeMeters" [Float],
        pub enable_bsr_profiling: bool = false => "EnableBSRProfiling" [Bool],
        pub log_connection_handshake: bool = false => "LogConnectionHandshake" [Bool],
        pub bsr_max_degree_of_parallelism: i32 = 0 => "BSRMaxDegreeOfParallelism" [Int],
        pub bsr_send_phase_budget_percent: i32 = 0 => "BSRSendPhaseBudgetPercent" [Int],
        pub bsr_max_slice_count: i32 = 0 => "BSRMaxSliceCount" [Int],
        pub voice_frame_duration_ms: i32 = 20 => "VoiceFrameDurationMs" [Int],
        pub disallow_headless: bool = false => "DisallowHeadless" [Bool],
        pub avatars_locked: bool = false => "AvatarsLocked" [Bool],
        pub props_locked: bool = false => "PropsLocked" [Bool],
        pub worlds_locked: bool = true => "WorldsLocked" [Bool],
        pub servers_locked: bool = false => "ServersLocked" [Bool],
        pub third_person_disabled: bool = false => "ThirdPersonDisabled" [Bool],
        pub additional_avatar_data_lock: bool = false => "AdditionalAvatarDataLock" [Bool],
        pub camera_metadata_disallow_mask: u8 = 0 => "CameraMetadataDisallowMask" [Byte],
        pub crash_reporting_enabled: bool = true => "CrashReportingEnabled" [Bool],
        pub max_microphone_range_meters: f32 = 25.0 => "MaxMicrophoneRangeMeters" [Float],
        pub max_hearing_range_meters: f32 = 25.0 => "MaxHearingRangeMeters" [Float],
        pub min_avatar_eye_height_meters: f32 = 0.1 => "MinAvatarEyeHeightMeters" [Float],
        pub max_avatar_eye_height_meters: f32 = 100.0 => "MaxAvatarEyeHeightMeters" [Float],
        pub max_content_spheres_per_player: i32 = 32 => "MaxContentSpheresPerPlayer" [Int],
        pub max_network_ids_per_player: i32 = 32768 => "MaxNetworkIdsPerPlayer" [Int],
        pub max_loaded_resources_per_player: i32 = 16384 => "MaxLoadedResourcesPerPlayer" [Int],
        pub max_scene_relay_megabits_per_second_per_player: i32 = 0 => "MaxSceneRelayMegabitsPerSecondPerPlayer" [Int],
        pub playspace_mover_locked: bool = false => "PlayspaceMoverLocked" [Bool],
        pub direct_connect_locked: bool = false => "DirectConnectLocked" [Bool],
        pub cilbox_locked: bool = false => "CilboxLocked" [Bool],
        pub images_locked: bool = false => "ImagesLocked" [Bool],
        pub end_effector_ik_disabled: bool = false => "EndEffectorIKDisabled" [Bool],
        pub text_chat_locked: bool = false => "TextChatLocked" [Bool],
        pub voice_chat_locked: bool = false => "VoiceChatLocked" [Bool],
        pub media_player_locked: bool = false => "MediaPlayerLocked" [Bool],
        pub camera_capture_locked: bool = false => "CameraCaptureLocked" [Bool],
        pub prop_grabbing_locked: bool = false => "PropGrabbingLocked" [Bool],
        pub safe_display_names_forced: bool = false => "SafeDisplayNamesForced" [Bool],
        pub api_enabled: bool = false => "ApiEnabled" [Bool],
        pub api_host: String = "localhost".to_string() => "ApiHost" [Str],
        pub api_port: u16 = 10667 => "ApiPort" [UShort],
        pub api_key: String = "".to_string() => "ApiKey" [Str],
    }
}
impl Configuration {
    pub const CONFIG_FOLDER_NAME: &'static str = "config";
    pub const LOGS_FOLDER_NAME: &'static str = "logs";
    pub const INITIAL_RESOURCES_FOLDER_NAME: &'static str = "initialresources";
    pub const DEFAULT_LIBRARY_FOLDER_NAME: &'static str = "defaultlibrary";

    /// Bump when config changes should force existing files to be rewritten (e.g. to refresh doc
    /// comments). Newly-added settings are healed automatically regardless.
    // 13: LogConnectionHandshake added - the per-connection auth chatter is now off by default.
    pub const CURRENT_CONFIG_VERSION: i32 = 13;

    /// Read config from file. If no file is found create a default config file at `file_path`.
    /// Also loads per-transport config sidecars from `{configDir}/transports/{stackId}.xml`.
    pub fn load_from_xml(file_path: &Path) -> Result<Configuration, ConfigXmlError> {
        BasisNetworkStackRegistry::ensure_initialized();

        let result = if file_path.exists() {
            let xml = std::fs::read_to_string(file_path)?;
            let mut result: Configuration = BasisConfigXmlDocs::deserialize(&xml)?;
            // Heal an older config: if it predates the current schema version or is missing any
            // setting we now write, re-save it so the new settings are added without disturbing
            // the values already present.
            if BasisConfigXmlDocs::needs_upgrade(file_path, &result) {
                BNL::log(format!("{} is from an older version; adding missing settings.", file_path.display()));
                result.write_xml(file_path)?;
            }
            result
        } else {
            BNL::log(format!("{} not found, creating with default values", file_path.display()));
            let mut result = Configuration::default();
            result.write_xml(file_path)?;
            result
        };

        let config_dir = file_path.parent().map(Path::to_path_buf).unwrap_or_default();
        BasisTransportConfigStore::load_all(&config_dir);
        Ok(result)
    }

    /// Persist this configuration back to `file_path`. Writes via a sibling temp file + atomic
    /// move so a crash mid-write doesn't corrupt the live config.
    pub fn save_to_xml(&mut self, file_path: &Path) -> Result<(), ConfigXmlError> {
        self.write_xml(file_path)?;
        let config_dir = file_path.parent().map(Path::to_path_buf).unwrap_or_default();
        BasisTransportConfigStore::save_all(&config_dir);
        Ok(())
    }

    /// Atomically write just this config.xml (temp file + replace), stamping the current schema
    /// version and injecting doc comments. Does not touch the transport sidecars.
    fn write_xml(&mut self, file_path: &Path) -> Result<(), ConfigXmlError> {
        self.config_version = Self::CURRENT_CONFIG_VERSION;
        if let Some(dir) = file_path.parent()
            && !dir.as_os_str().is_empty()
        {
            std::fs::create_dir_all(dir)?;
        }
        let xml = BasisConfigXmlDocs::serialize(self);
        let temp_path = PathBuf::from(format!("{}.tmp", file_path.display()));
        std::fs::write(&temp_path, xml)?;
        std::fs::rename(&temp_path, file_path)?;
        Ok(())
    }

    /// Resolve the canonical config.xml path under `{BaseDirectory}/config/config.xml` — the
    /// path the bootstrappers read on startup.
    pub fn get_default_path() -> PathBuf {
        Self::base_directory().join(Self::CONFIG_FOLDER_NAME).join("config.xml")
    }

    /// The C# `AppDomain.CurrentDomain.BaseDirectory`: the folder the executable lives in.
    pub fn base_directory() -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
    }

    /// Overrides what is written in the config.xml with any environment variable that has the
    /// same name as a public config field. Intended to let Linux admins override defaults during
    /// launch, e.g. `PeerLimit=256 ./basis_network_console`.
    pub fn process_environmental_overrides(&mut self) {
        Self::apply_environmental_overrides_to(self);
    }

    /// Settings established once during boot — socket binds, the transport stack, the health and
    /// API listeners, the console, and disk support. Editing one persists and takes effect on
    /// the next start; everything else is re-applied live by `NetworkServer::apply_live_configuration`.
    const RESTART_ONLY_FIELDS: [&'static str; 15] = [
        "SetPort",
        "IPv4Address",
        "IPv6Address",
        "OverrideAutoDiscoveryOfIpv",
        "NetworkStackId",
        "HasFileSupport",
        "EnableStatistics",
        "EnableConsole",
        "HealthCheckHost",
        "HealthCheckPort",
        "HealthPath",
        "ApiEnabled",
        "ApiHost",
        "ApiPort",
        "ApiKey",
    ];

    /// Whether a field only takes effect after a restart.
    pub fn requires_restart(field_name: &str) -> bool {
        Self::RESTART_ONLY_FIELDS.contains(&field_name)
    }

    /// Settings a connected client is told about at join time only.
    pub fn applies_to_new_joins_only(field_name: &str) -> bool {
        field_name == "BSRSlowestSendRate"
    }

    /// Field names whose values must never reach the log.
    pub fn is_secret_field_name(field_name: &str) -> bool {
        if field_name.is_empty() {
            return false;
        }
        let lower = field_name.to_ascii_lowercase();
        lower.contains("password") || lower.contains("apikey") || lower.contains("secret") || lower.contains("token")
    }

    /// Applies environment overrides to any field-table config; the C# recursed into nested
    /// objects, which no config type has.
    pub fn apply_environmental_overrides_to<T: BasisXmlConfig>(target: &mut T) {
        for name in T::field_names() {
            let Ok(value) = std::env::var(name) else {
                continue;
            };
            BNL::log(format!(
                "Applying Environmental Override with Field:{name} Value:{}",
                if Self::is_secret_field_name(name) { "<redacted>".to_string() } else { value.clone() }
            ));
            match T::field_kind(name) {
                Some(FieldKind::Int) => {
                    if target.set_field(name, &value).is_err() {
                        BNL::log_warning("Could not cast to int. Failed Override");
                    }
                }
                Some(FieldKind::UShort) => {
                    if target.set_field(name, &value).is_err() {
                        BNL::log_warning("Could not cast to ushort. Failed Override.");
                    }
                }
                Some(FieldKind::Float) => {
                    if target.set_field(name, &value).is_err() {
                        BNL::log_warning("Could not cast to float. Failed Override.");
                    }
                }
                Some(FieldKind::Str) => {
                    let _ = target.set_field(name, &value);
                }
                Some(FieldKind::Bool) => {
                    if target.set_field(name, &value).is_err() {
                        BNL::log_warning(format!("Could not parse '{value}' as bool for field {name}. Failed Override"));
                    }
                }
                _ => BNL::log_warning(format!(
                    "Environmental variable type could not be processed for Config Field:{name} Value:{value}"
                )),
            }
        }
    }

    /// The C# enum field, typed. Kept as a method so call sites read like the property did.
    pub fn restriction_mode(&self) -> BasisUserRestrictionMode {
        self.basis_user_restriction_mode
    }
}
