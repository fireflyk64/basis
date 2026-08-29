//! Port of `Security/BasisPlayerModeration.cs`: bans, kicks, the admin-channel entry point and
//! every admin-driven server mutation.

use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};

use basis_error::{BasisError, BasisResult, ErrorCode, ResultExt};
use basis_network_core::SerializableBasis::{AdminRequest, AdminRequestMode};
use basis_network_core::compression::BasisAvatarBundleZstd;
use basis_network_core::configuration::Configuration;
use basis_network_core::identity::BasisUserRestrictionMode;
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetDataWriter, NetPacketReader, NetPeerRef, NetResult};
use dashmap::DashMap;

use crate::NetworkServer;
use crate::core::basis_server_handle_events::BasisServerHandleEvents;
use crate::networking::{BasisDefaultLibraryConfiguration, BasisDefaultLibraryLoader, BasisSavedState};
use crate::reduction::BasisServerReductionSystemEvents;
use crate::resources::BasisNetworkServerLibrary;
use crate::security::{
    BasisAudioRangeLimitManager, BasisAvatarScaleLimitManager, BasisCrashReportStateManager, BasisGlobalLockManager,
    BasisHeadlessAudioStateManager, BasisHeadlessConnectionPolicyManager, BasisOpusFrameDurationStateManager,
    BasisOpusPacketLossStateManager, BasisRejoinLockManager, BasisResourceLimitManager, BasisServerLogBundleService,
    BasisUserOpusBitrateStateManager, PermNodes, PermissionIntegration,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BannedPlayer {
    pub uuid: String,
    pub banned_ip: String,
    pub reason: String,
    pub has_banned_ip: bool,
    pub time_of_ban: String,
}

static BANNED_PLAYERS: LazyLock<DashMap<String, BannedPlayer>> = LazyLock::new(DashMap::new);
static BANNED_UUIDS: LazyLock<DashMap<String, ()>> = LazyLock::new(DashMap::new);
static USE_FILE_ON_DISC: AtomicBool = AtomicBool::new(true);

pub struct BasisPlayerModeration;

impl BasisPlayerModeration {
    pub fn ban_file_path() -> PathBuf {
        NetworkServer::config_directory().join("banned_players.xml")
    }

    pub fn use_file_on_disc() -> bool {
        USE_FILE_ON_DISC.load(Ordering::Acquire)
    }

    pub fn set_use_file_on_disc(value: bool) {
        USE_FILE_ON_DISC.store(value, Ordering::Release);
    }

    // =========================
    // Core Ban Logic
    // =========================

    fn now_stamp() -> String {
        crate::util::utc_now_stamp()
    }

    pub fn ban(uuid: &str, reason: &str) -> String {
        let peer = match Self::validate_target(uuid, reason) {
            Ok(peer) => peer,
            Err(error) => return error,
        };
        if Self::is_protected(uuid) {
            return "Target is protected".to_string();
        }
        peer.disconnect_with(reason.as_bytes());

        let banned = BannedPlayer {
            uuid: uuid.to_string(),
            reason: reason.to_string(),
            has_banned_ip: false,
            time_of_ban: Self::now_stamp(),
            banned_ip: String::new(),
        };
        BANNED_PLAYERS.insert(uuid.to_string(), banned);
        BANNED_UUIDS.insert(uuid.to_string(), ());
        match Self::save_banned_players() {
            Ok(()) => format!("Player {uuid} banned."),
            Err(e) => format!("Player {uuid} banned, but the ban list could not be saved: {e}"),
        }
    }

    pub fn ip_ban(uuid: &str, reason: &str) -> String {
        let peer = match Self::validate_target(uuid, reason) {
            Ok(peer) => peer,
            Err(error) => return error,
        };
        if Self::is_protected(uuid) {
            return "Target is protected".to_string();
        }
        let ip = peer.address().to_string();
        peer.disconnect_with(reason.as_bytes());

        let banned = BannedPlayer {
            uuid: uuid.to_string(),
            banned_ip: ip.clone(),
            reason: reason.to_string(),
            has_banned_ip: true,
            time_of_ban: Self::now_stamp(),
        };
        BANNED_PLAYERS.insert(uuid.to_string(), banned);
        BANNED_UUIDS.insert(uuid.to_string(), ());
        match Self::save_banned_players() {
            Ok(()) => format!("Player {uuid} and IP {ip} banned."),
            Err(e) => format!("Player {uuid} and IP {ip} banned, but the ban list could not be saved: {e}"),
        }
    }

    pub fn kick(uuid: &str, reason: &str) -> String {
        let peer = match Self::validate_target(uuid, reason) {
            Ok(peer) => peer,
            Err(error) => return error,
        };
        if Self::is_protected(uuid) {
            return "Target is protected".to_string();
        }
        peer.disconnect_with(reason.as_bytes());
        format!("Player {uuid} kicked.")
    }

    fn validate_target(uuid: &str, reason: &str) -> Result<NetPeerRef, String> {
        if uuid.is_empty() {
            return Err("UUID invalid".to_string());
        }
        if reason.is_empty() {
            return Err("Reason invalid".to_string());
        }
        NetworkServer::uuid_to_net_id(uuid)
            .and_then(|id| NetworkServer::authenticated_peers().get(&id).map(|p| p.value().clone()))
            .ok_or_else(|| "Player not found".to_string())
    }

    fn is_protected(uuid: &str) -> bool {
        PermissionIntegration::manager().has(uuid, PermNodes::PROTECTION)
    }

    // =========================
    // Ban Storage
    // =========================

    pub fn banned_players() -> Vec<BannedPlayer> {
        let mut list: Vec<BannedPlayer> = BANNED_PLAYERS.iter().map(|e| e.value().clone()).collect();
        list.sort_by(|a, b| a.uuid.cmp(&b.uuid));
        list
    }

    /// The `List<BannedPlayer>` XmlSerializer document.
    pub fn to_xml(players: &[BannedPlayer]) -> String {
        fn esc(v: &str) -> String {
            quick_xml::escape::escape(v).into_owned()
        }
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ArrayOfBannedPlayer xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\">\n",
        );
        for p in players {
            xml.push_str("  <BannedPlayer>\n");
            xml.push_str(&format!("    <UUID>{}</UUID>\n", esc(&p.uuid)));
            xml.push_str(&format!("    <BannedIp>{}</BannedIp>\n", esc(&p.banned_ip)));
            xml.push_str(&format!("    <Reason>{}</Reason>\n", esc(&p.reason)));
            xml.push_str(&format!("    <HasBannedIp>{}</HasBannedIp>\n", p.has_banned_ip));
            xml.push_str(&format!("    <TimeOfBan>{}</TimeOfBan>\n", esc(&p.time_of_ban)));
            xml.push_str("  </BannedPlayer>\n");
        }
        xml.push_str("</ArrayOfBannedPlayer>");
        xml
    }

    pub fn parse_xml(xml: &str) -> BasisResult<Vec<BannedPlayer>> {
        use quick_xml::Reader;
        use quick_xml::events::Event;
        let malformed = |e: String| BasisError::permanent(ErrorCode::Serialization, format!("banned players: {e}"));
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut players = Vec::new();
        let mut current: Option<BannedPlayer> = None;
        let mut depth = 0usize;
        loop {
            match reader.read_event_into(&mut buf).map_err(|e| malformed(e.to_string()))? {
                Event::Start(e) => {
                    depth += 1;
                    let name = e.name().as_ref().to_owned();
                    match name.as_str() {
                        "ArrayOfBannedPlayer" => {}
                        "BannedPlayer" => current = Some(BannedPlayer::default()),
                        _ => {
                            let end = e.to_end().into_owned();
                            let text = reader.read_text(end.name()).map_err(|e| malformed(e.to_string()))?;
                            let text = quick_xml::escape::unescape(&text).map(|c| c.into_owned()).unwrap_or_else(|_| text.to_string());
                            depth -= 1;
                            if let Some(p) = current.as_mut() {
                                match name.as_str() {
                                    "UUID" => p.uuid = text,
                                    "BannedIp" => p.banned_ip = text,
                                    "Reason" => p.reason = text,
                                    "HasBannedIp" => p.has_banned_ip = text.trim().eq_ignore_ascii_case("true"),
                                    "TimeOfBan" => p.time_of_ban = text,
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                Event::Empty(e) => {
                    if e.name().as_ref() == "BannedPlayer" {
                        players.push(BannedPlayer::default());
                    }
                }
                Event::End(e) => {
                    depth = depth.saturating_sub(1);
                    if e.name().as_ref() == "BannedPlayer"
                        && let Some(p) = current.take()
                    {
                        players.push(p);
                    }
                }
                Event::Eof => {
                    if depth > 0 {
                        return Err(malformed("unexpected end of document".to_string()));
                    }
                    break;
                }
                _ => {}
            }
            buf.clear();
        }
        Ok(players)
    }

    /// Writes the ban list. A failure is logged (the admin already got their reply) and returned
    /// for callers that care.
    pub fn save_banned_players() -> BasisResult<()> {
        if !Self::use_file_on_disc() {
            return Ok(());
        }
        let path = Self::ban_file_path();
        let result = (|| -> BasisResult<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).with_context(|| format!("creating '{}'", dir.display()))?;
            }
            std::fs::write(&path, Self::to_xml(&Self::banned_players())).with_context(|| format!("writing '{}'", path.display()))
        })();
        if let Err(e) = &result {
            BNL::log_error(format!("Save banned failed: {e}"));
        }
        result
    }

    /// Loads the ban list; a missing file is created empty. A corrupt file is an error and
    /// leaves the in-memory list untouched.
    pub fn load_banned_players() -> BasisResult<()> {
        let path = Self::ban_file_path();
        if !path.exists() {
            return Self::save_banned_players();
        }
        let xml = std::fs::read_to_string(&path).with_context(|| format!("reading '{}'", path.display()))?;
        let list = Self::parse_xml(&xml).with_context(|| format!("parsing '{}'", path.display()))?;
        BANNED_PLAYERS.clear();
        BANNED_UUIDS.clear();
        for p in list {
            BANNED_UUIDS.insert(p.uuid.clone(), ());
            BANNED_PLAYERS.insert(p.uuid.clone(), p);
        }
        Ok(())
    }

    pub fn is_banned(uuid: &str) -> bool {
        BANNED_UUIDS.contains_key(uuid)
    }

    pub fn unban(uuid: &str) -> bool {
        if !BANNED_UUIDS.contains_key(uuid) {
            return false;
        }
        BANNED_PLAYERS.remove(uuid);
        BANNED_UUIDS.remove(uuid);
        let _ = Self::save_banned_players();
        true
    }

    pub fn unban_ip(ip: &str) -> bool {
        let matching: Vec<String> =
            BANNED_PLAYERS.iter().filter(|e| e.has_banned_ip && e.banned_ip == ip).map(|e| e.uuid.clone()).collect();
        if matching.is_empty() {
            return false;
        }
        for uuid in matching {
            BANNED_PLAYERS.remove(&uuid);
            BANNED_UUIDS.remove(&uuid);
        }
        let _ = Self::save_banned_players();
        true
    }

    /// Drops every ban from memory (not from disk). Tests.
    pub fn reset_for_tests() {
        BANNED_PLAYERS.clear();
        BANNED_UUIDS.clear();
    }

    // =========================
    // Admin Entry Point
    // =========================

    pub fn on_admin_message(peer: &NetPeerRef, mut reader: NetPacketReader) {
        if NetworkServer::net_id_to_uuid(peer).is_none() {
            Self::send_back_message(peer, "UUID not found");
            return;
        }
        let mut req = AdminRequest::default();
        req.deserialize(&mut reader);
        let Some(mode) = req.get_admin_request_mode() else {
            BNL::log_warning(format!("Unknown admin request mode {} from peer {}", req.message_index(), peer.id()));
            return;
        };
        if let Err(e) = Self::dispatch(peer, &mut reader, mode) {
            BNL::log_error(format!("Malformed admin request {mode:?} from peer {}: {e}", peer.id()));
            Self::send_back_message(peer, &format!("Malformed {mode:?} request."));
        }
    }

    fn require(peer: &NetPeerRef, perm: &str, action: impl FnOnce() -> NetResult<()>) -> NetResult<()> {
        if !PermissionIntegration::has_valid_requirement(peer, perm) {
            Self::send_back_message(peer, &format!("No permission: {perm}"));
            return Ok(());
        }
        action()
    }

    fn dispatch(peer: &NetPeerRef, reader: &mut NetPacketReader, mode: AdminRequestMode) -> NetResult<()> {
        use AdminRequestMode as M;
        // ===== VIEW PERMISSIONS =====
        if mode == M::GetPermissions {
            if !PermissionIntegration::has_valid_requirement(peer, PermNodes::PERMISSIONS_VIEW) {
                Self::send_back_message(peer, "No permission: view");
                return Ok(());
            }
            Self::handle_get_permissions(peer);
            return Ok(());
        }

        match mode {
            M::Ban => Self::require(peer, PermNodes::MODERATION_BAN, || {
                let uuid = reader.get_string()?;
                let reason = reader.get_string()?;
                Self::send_back_message(peer, &Self::ban(&uuid, &reason));
                Ok(())
            }),
            M::Kick => Self::require(peer, PermNodes::MODERATION_KICK, || {
                let uuid = reader.get_string()?;
                let reason = reader.get_string()?;
                Self::send_back_message(peer, &Self::kick(&uuid, &reason));
                Ok(())
            }),
            M::IpAndBan => Self::require(peer, PermNodes::MODERATION_IP_BAN, || {
                let uuid = reader.get_string()?;
                let reason = reader.get_string()?;
                Self::send_back_message(peer, &Self::ip_ban(&uuid, &reason));
                Ok(())
            }),
            M::UnBan => Self::require(peer, PermNodes::MODERATION_UNBAN, || {
                let uuid = reader.get_string()?;
                Self::send_back_message(peer, if Self::unban(&uuid) { "Unbanned" } else { "Failed" });
                Ok(())
            }),
            M::UnBanIP => Self::require(peer, PermNodes::MODERATION_UNBAN_IP, || {
                let ip = reader.get_string()?;
                Self::send_back_message(peer, if Self::unban_ip(&ip) { "Unbanned" } else { "Failed" });
                Ok(())
            }),
            M::Message => Self::require(peer, PermNodes::MODERATION_MESSAGE, || {
                let id = reader.get_ushort()?;
                let message = reader.get_string()?;
                if let Some(target) = NetworkServer::authenticated_peers().get(&i32::from(id)) {
                    Self::send_back_message(target.value(), &message);
                }
                Ok(())
            }),
            M::MessageAll => Self::require(peer, PermNodes::MODERATION_MESSAGE_ALL, || {
                let message = reader.get_string()?;
                Self::broadcast_admin_message_excluding(peer, M::MessageAll, |w| w.put_string(&message))
            }),
            M::TeleportAll => Self::require(peer, PermNodes::MODERATION_TELEPORT, || {
                let target = reader.get_ushort()?;
                Self::broadcast_admin_message_excluding(peer, mode, |w| {
                    w.put_ushort(target);
                    Ok(())
                })
            }),
            M::TeleportPlayer => Self::require(peer, PermNodes::MODERATION_TELEPORT, || {
                let target_id = reader.get_ushort()?;
                let Some(target_peer) = NetworkServer::authenticated_peers().get(&i32::from(target_id)).map(|p| p.value().clone()) else {
                    return Ok(());
                };
                Self::send_admin_message(&target_peer, mode, |w| {
                    w.put_ushort(peer.id() as u16);
                    Ok(())
                })
            }),
            M::EnableShoutMode | M::DisableShoutMode => {
                Self::require(peer, PermNodes::MODERATION_SHOUT, || Self::handle_shout_mode(peer, reader, mode == M::EnableShoutMode))
            }
            M::SetFullQualityBroadcast => {
                Self::require(peer, PermNodes::MODERATION_FULL_QUALITY_BROADCAST, || Self::handle_full_quality_broadcast(peer, reader))
            }
            M::ForceAvatar => Self::require(peer, PermNodes::MODERATION_FORCE_AVATAR, || Self::handle_force_avatar(peer, reader)),
            M::ForceAvatarAll => Self::require(peer, PermNodes::MODERATION_FORCE_AVATAR, || Self::handle_force_avatar_all(peer, reader)),
            M::SetLocomotionOverride => {
                Self::require(peer, PermNodes::MODERATION_LOCOMOTION, || Self::handle_set_locomotion_override(peer, reader))
            }
            M::SetLocomotionOverrideAll => {
                Self::require(peer, PermNodes::MODERATION_LOCOMOTION, || Self::handle_set_locomotion_override_all(peer, reader))
            }

            // ===== GLOBAL LOCK =====
            M::GlobalToggleAvatars => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_toggle(peer, "Avatar", BasisGlobalLockManager::toggle_avatars());
                Ok(())
            }),
            M::GlobalToggleProps => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_toggle(peer, "Prop", BasisGlobalLockManager::toggle_props());
                Ok(())
            }),
            M::GlobalToggleWorlds => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_toggle(peer, "World", BasisGlobalLockManager::toggle_worlds());
                Ok(())
            }),
            M::GlobalToggleServers => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_toggle(peer, "Server share", BasisGlobalLockManager::toggle_servers());
                Ok(())
            }),
            M::GlobalToggleThirdPerson => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_feature_toggle(peer, "The third-person camera", BasisGlobalLockManager::toggle_third_person());
                Ok(())
            }),
            M::GlobalToggleAdditionalAvatarDataLock => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                // Not handle_global_toggle: its "loading ENABLED/DISABLED" template reads inverted
                // for this flag (it said DISABLED at the moment stripping turned ON), which misled
                // admins into leaving face tracking stripped server-wide.
                let now_stripping = BasisGlobalLockManager::toggle_additional_avatar_data_lock();
                Self::handle_global_state_notification(
                    peer,
                    if now_stripping {
                        "Additional avatar data (face tracking, avatar behaviour params) is now STRIPPED for everyone."
                    } else {
                        "Additional avatar data (face tracking, avatar behaviour params) now flows normally."
                    },
                );
                Ok(())
            }),
            M::SetGlobalCameraPolicy => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || Self::handle_camera_policy_set(peer, reader)),
            M::SetGlobalHeadlessAudio => {
                Self::require(peer, PermNodes::MODERATION_HEADLESS_AUDIO, || Self::handle_headless_audio_set(peer, reader))
            }
            M::SetGlobalCrashReporting => {
                Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || Self::handle_crash_reporting_set(peer, reader))
            }
            M::SetGlobalAudioRangeLimits => {
                Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || Self::handle_audio_range_limits_set(peer, reader))
            }
            M::GlobalTogglePlayspaceMover => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_playspace_mover_toggle(peer);
                Ok(())
            }),
            M::GlobalToggleDirectConnect => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_direct_connect_toggle(peer);
                Ok(())
            }),
            M::GlobalToggleCilbox => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_feature_toggle(peer, "Avatar Cilbox code", BasisGlobalLockManager::toggle_cilbox());
                Ok(())
            }),
            M::GlobalToggleImages => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_toggle(peer, "Shared image", BasisGlobalLockManager::toggle_images());
                Ok(())
            }),
            M::GlobalToggleEndEffectorIK => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_feature_toggle(peer, "Remote end-effector IK", BasisGlobalLockManager::toggle_end_effector_ik());
                Ok(())
            }),
            M::GlobalToggleTextChat => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_feature_toggle(peer, "Text chat", BasisGlobalLockManager::toggle_text_chat());
                Ok(())
            }),
            M::GlobalToggleVoiceChat => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_feature_toggle(peer, "Voice chat", BasisGlobalLockManager::toggle_voice_chat());
                Ok(())
            }),
            M::GlobalToggleMediaPlayer => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_feature_toggle(peer, "Media players", BasisGlobalLockManager::toggle_media_player());
                Ok(())
            }),
            M::GlobalToggleCameraCapture => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_feature_toggle(peer, "Camera capture", BasisGlobalLockManager::toggle_camera_capture());
                Ok(())
            }),
            M::GlobalTogglePropGrabbing => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_feature_toggle(peer, "Prop grabbing", BasisGlobalLockManager::toggle_prop_grabbing());
                Ok(())
            }),
            M::GlobalToggleSafeDisplayNames => Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || {
                Self::handle_global_protection_toggle(peer, "Safe display names", BasisGlobalLockManager::toggle_safe_display_names());
                Ok(())
            }),
            M::SetGlobalAvatarScaleLimits => {
                Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || Self::handle_avatar_scale_limits_set(peer, reader))
            }
            M::SetGlobalResourceLimits => {
                Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || Self::handle_resource_limits_set(peer, reader))
            }
            M::SetGlobalReductionSettings => {
                Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || Self::handle_reduction_settings_set(peer, reader))
            }
            M::SetGlobalImageBandwidth => {
                Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || Self::handle_image_bandwidth_set(peer, reader))
            }
            M::RequestAllLogs => Self::require(peer, PermNodes::ADMIN_LOGS, || {
                BasisServerLogBundleService::send_all_logs_to_peer(peer);
                Ok(())
            }),
            M::DeleteAllLogs => Self::require(peer, PermNodes::ADMIN_LOGS, || {
                BasisServerLogBundleService::delete_all_logs_for_peer(peer);
                Ok(())
            }),
            M::SetGlobalHeadlessDisallow => {
                Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || Self::handle_headless_disallow_set(peer, reader))
            }
            M::SetGlobalOpusPacketLoss => {
                Self::require(peer, PermNodes::MODERATION_GLOBAL_LOCK, || Self::handle_opus_packet_loss_set(peer, reader))
            }
            M::SetUserOpusBitrate => Self::require(peer, PermNodes::MODERATION_OPUS_BITRATE, || Self::handle_user_opus_bitrate_set(peer, reader)),
            M::SetGlobalOpusFrameDuration => {
                Self::require(peer, PermNodes::MODERATION_OPUS_BITRATE, || Self::handle_opus_frame_duration_set(peer, reader))
            }
            M::SetGlobalOpusBitrate => {
                Self::require(peer, PermNodes::MODERATION_OPUS_BITRATE, || Self::handle_global_opus_bitrate_set(peer, reader))
            }

            // ===== PERMISSION EDIT =====
            M::SetUserGroup | M::SetUserNode | M::SetGroupNode | M::CreateGroup | M::DeleteGroup | M::SetGroupParent => {
                Self::require(peer, PermNodes::PERMISSIONS_EDIT, || Self::handle_permission_edit(mode, peer, reader))
            }

            // ===== SERVER CONFIG =====
            M::SetServerName => Self::require(peer, PermNodes::CONFIGURATION_EDITOR, || {
                let name = reader.get_string()?;
                Self::send_back_message(peer, &Self::apply_server_name(&name));
                Ok(())
            }),
            M::SetServerMotd => Self::require(peer, PermNodes::CONFIGURATION_EDITOR, || {
                let motd = reader.get_string()?;
                Self::send_back_message(peer, &Self::apply_server_motd(&motd));
                Ok(())
            }),
            M::SetAllowlistMode => Self::require(peer, PermNodes::CONFIGURATION_EDITOR, || {
                let mode = reader.get_byte()?;
                Self::send_back_message(peer, &Self::apply_allowlist_mode(mode));
                Ok(())
            }),
            M::SetGlobalPeerLimit => Self::require(peer, PermNodes::CONFIGURATION_EDITOR, || Self::handle_peer_limit_set(peer, reader)),
            M::AddAllowlist => Self::require(peer, PermNodes::MODERATION_ALLOWLIST, || {
                let uuid = reader.get_string()?;
                Self::send_back_message(peer, &Self::apply_allowlist_add(&uuid));
                Ok(())
            }),
            M::RemoveAllowlist => Self::require(peer, PermNodes::MODERATION_ALLOWLIST, || {
                let uuid = reader.get_string()?;
                Self::send_back_message(peer, &Self::apply_allowlist_remove(&uuid));
                Ok(())
            }),
            M::AddDefaultLibraryItem => Self::require(peer, PermNodes::CONFIGURATION_EDITOR, || {
                let item_mode = reader.get_byte()?;
                let item_url = reader.get_string()?;
                let item_password = reader.get_string()?;
                Self::send_back_message(peer, &Self::apply_add_default_library_item(item_mode, &item_url, &item_password));
                Ok(())
            }),
            M::RemoveDefaultLibraryItem => Self::require(peer, PermNodes::CONFIGURATION_EDITOR, || {
                let remove_url = reader.get_string()?;
                Self::send_back_message(peer, &Self::apply_remove_default_library_item(&remove_url));
                Ok(())
            }),
            _ => Ok(()),
        }
    }

    // =========================
    // Server-config admin operations
    // =========================
    // Each mutation updates the live Configuration field (read on the next info-query response,
    // ServerMetaDataMessage, or connection check) and then persists the current state of
    // Configuration to config/config.xml so the change survives a restart.

    fn truncate_chars(value: &str, max: usize) -> String {
        value.chars().take(max).collect()
    }

    pub fn apply_server_name(new_name: &str) -> String {
        let new_name = Self::truncate_chars(new_name, BasisNetworkCommons::SERVER_INFO_NAME_MAX_LENGTH);
        NetworkServer::update_configuration(|c| c.server_name = new_name.clone());
        Self::save_config();
        format!("Server name set to '{new_name}'.")
    }

    pub fn apply_server_motd(new_motd: &str) -> String {
        let new_motd = Self::truncate_chars(new_motd, BasisNetworkCommons::SERVER_INFO_MOTD_MAX_LENGTH);
        NetworkServer::update_configuration(|c| c.server_motd = new_motd.clone());
        Self::save_config();
        "Server MOTD updated.".to_string()
    }

    pub fn apply_allowlist_mode(mode: u8) -> String {
        if mode > BasisUserRestrictionMode::RejoinOnly as u8 {
            return format!("Unknown restriction mode value {mode}.");
        }
        let parsed = BasisUserRestrictionMode::from_byte(mode);
        NetworkServer::update_configuration(|c| c.basis_user_restriction_mode = parsed);
        if parsed == BasisUserRestrictionMode::RejoinOnly {
            BasisRejoinLockManager::capture_current_population();
        } else {
            BasisRejoinLockManager::clear();
        }
        Self::save_config();
        // Restriction mode rides on the lock-state payload; push it so connected clients refresh.
        BasisGlobalLockManager::broadcast_lock_state();
        format!("Restriction mode set to {parsed}.")
    }

    pub fn apply_allowlist_add(uuid: &str) -> String {
        if uuid.trim().is_empty() {
            return "UUID was empty.".to_string();
        }
        let Some(allow_list) = NetworkServer::allow_list() else {
            return "AllowList not initialized.".to_string();
        };
        if let Err(e) = allow_list.add_to_allowlist(uuid) {
            BNL::log_error(format!("Failed to add {uuid} to the allowlist: {e}"));
            return format!("Failed to add {uuid} to allowlist — see server log.");
        }
        format!("Added {uuid} to allowlist.")
    }

    pub fn apply_allowlist_remove(uuid: &str) -> String {
        if uuid.trim().is_empty() {
            return "UUID was empty.".to_string();
        }
        let Some(allow_list) = NetworkServer::allow_list() else {
            return "AllowList not initialized.".to_string();
        };
        if let Err(e) = allow_list.remove_from_allowlist(uuid) {
            BNL::log_error(format!("Failed to remove {uuid} from the allowlist: {e}"));
            return format!("Failed to remove {uuid} from allowlist — see server log.");
        }
        format!("Removed {uuid} from allowlist.")
    }

    pub fn apply_add_default_library_item(mode: u8, url: &str, password: &str) -> String {
        if url.trim().is_empty() {
            return "URL was empty.".to_string();
        }
        // Mode is the client's BundledContentHolder.Mode: 0=Avatar, 1=World, 2=Prop.
        if mode > 2 {
            return format!("Unknown library mode {mode} (expected 0=Avatar, 1=World, 2=Prop).");
        }
        // Defensive split of `url#fragment` — if the admin pasted a copy-able share string with
        // the password baked into the URL fragment, peel it off here so the password lands in
        // the Password field instead of the URL field.
        let mut url = url.to_string();
        let mut password = password.to_string();
        if let Some(hash_index) = url.find('#') {
            let fragment = url[hash_index + 1..].to_string();
            url.truncate(hash_index);
            if password.is_empty() && !fragment.is_empty() {
                use base64::Engine;
                // A fragment that is not valid base64 leaves the password empty rather than
                // storing the raw fragment bytes.
                if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(fragment.as_bytes())
                    && let Ok(text) = String::from_utf8(decoded)
                {
                    password = text;
                }
            }
        }
        let config = BasisDefaultLibraryConfiguration { mode, url, password };
        let written = BasisDefaultLibraryLoader::save_item(Configuration::DEFAULT_LIBRARY_FOLDER_NAME, &config);
        if written.is_empty() {
            return "Failed to persist default library entry — see server log.".to_string();
        }
        // Push the updated list to every connected client so the new entry shows up in their
        // library immediately, not just on next connect.
        BasisNetworkServerLibrary::broadcast_library_to_all();
        let file_name = std::path::Path::new(&written).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or(written);
        format!("Default library entry added ({file_name}).")
    }

    pub fn apply_remove_default_library_item(url: &str) -> String {
        if url.trim().is_empty() {
            return "URL was empty.".to_string();
        }
        let removed = BasisDefaultLibraryLoader::remove_item(Configuration::DEFAULT_LIBRARY_FOLDER_NAME, url);
        if removed <= 0 {
            return format!("No default library entry matched URL '{url}'.");
        }
        BasisNetworkServerLibrary::broadcast_library_to_all();
        format!("Removed {removed} default library entry(ies) for URL '{url}'.")
    }

    fn save_config() {
        let mut configuration = (*NetworkServer::configuration_or_default()).clone();
        if let Err(e) = configuration.save_to_xml(&Configuration::get_default_path()) {
            BNL::log_error(format!("Failed to persist server configuration: {e}"));
        }
    }

    // =========================
    // Helpers
    // =========================

    fn handle_permission_edit(mode: AdminRequestMode, peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        use AdminRequestMode as M;
        let manager = PermissionIntegration::manager();
        // SetUserGroup/SetUserNode/SetGroupNode/SetGroupParent all carry a trailing `add` bool.
        let result = match mode {
            M::SetUserGroup => {
                let uuid = reader.get_string()?;
                let group = reader.get_string()?;
                let add = reader.get_bool()?;
                if add {
                    manager.add_user_to_group(&uuid, &group);
                } else {
                    manager.remove_user_from_group(&uuid, &group);
                }
                format!("{} {uuid} {} group '{group}'.", if add { "Added" } else { "Removed" }, if add { "to" } else { "from" })
            }
            M::SetUserNode => {
                let uuid = reader.get_string()?;
                let node = reader.get_string()?;
                let add = reader.get_bool()?;
                if add {
                    manager.add_user_node(&uuid, &node);
                } else {
                    manager.remove_user_node(&uuid, &node);
                }
                format!("{} node '{node}' {} user {uuid}.", if add { "Added" } else { "Removed" }, if add { "to" } else { "from" })
            }
            M::SetGroupNode => {
                let group = reader.get_string()?;
                let node = reader.get_string()?;
                let add = reader.get_bool()?;
                if add {
                    manager.add_group_node(&group, &node);
                } else {
                    manager.remove_group_node(&group, &node);
                }
                format!("{} node '{node}' {} group '{group}'.", if add { "Added" } else { "Removed" }, if add { "to" } else { "from" })
            }
            M::CreateGroup => {
                let group = reader.get_string()?;
                manager.get_or_create_group(&group);
                format!("Group '{group}' created.")
            }
            M::DeleteGroup => {
                let group = reader.get_string()?;
                if manager.delete_group(&group) { format!("Group '{group}' deleted.") } else { format!("No group named '{group}'.") }
            }
            M::SetGroupParent => {
                let group = reader.get_string()?;
                let parent = reader.get_string()?;
                let add = reader.get_bool()?;
                if add {
                    manager.add_group_parent(&group, &parent);
                } else {
                    manager.remove_group_parent(&group, &parent);
                }
                format!("Group '{group}' {} '{parent}'.", if add { "now inherits" } else { "no longer inherits" })
            }
            _ => "Permission updated".to_string(),
        };
        Self::send_back_message(peer, &result);
        Ok(())
    }

    fn handle_get_permissions(peer: &NetPeerRef) {
        let snap = PermissionIntegration::manager().snapshot();
        let mut writer = NetworkServer::rent_writer();
        let written = AdminRequest::default().serialize(&mut writer, AdminRequestMode::GetPermissions).and_then(|_| {
            writer.put_int(i32::try_from(snap.groups.len()).unwrap_or(i32::MAX));
            for (_, g) in snap.groups.iter() {
                writer.put_string(&g.name)?;
                writer.put_int(i32::try_from(g.nodes.len()).unwrap_or(i32::MAX));
                for n in g.nodes.iter() {
                    writer.put_string(n)?;
                }
                writer.put_int(i32::try_from(g.parents.len()).unwrap_or(i32::MAX));
                for p in g.parents.iter() {
                    writer.put_string(p)?;
                }
            }
            writer.put_int(i32::try_from(snap.users.len()).unwrap_or(i32::MAX));
            for (_, u) in snap.users.iter() {
                writer.put_string(&u.uuid)?;
                writer.put_int(i32::try_from(u.groups.len()).unwrap_or(i32::MAX));
                for g in u.groups.iter() {
                    writer.put_string(g)?;
                }
                writer.put_int(i32::try_from(u.nodes.len()).unwrap_or(i32::MAX));
                for n in u.nodes.iter() {
                    writer.put_string(n)?;
                }
            }
            Ok(())
        });
        if written.is_ok() {
            NetworkServer::try_send(peer, &writer, BasisNetworkCommons::ADMIN_CHANNEL, DeliveryMethod::ReliableOrdered);
        }
        NetworkServer::return_writer(writer);
    }

    fn handle_shout_mode(peer: &NetPeerRef, reader: &mut NetPacketReader, enable: bool) -> NetResult<()> {
        let id = reader.get_ushort()?;
        BasisSavedState::set_shout_mode(i32::from(id), enable);
        BasisServerHandleEvents::broadcast_shout_mode_state(id, enable, peer.id() as u16);
        Ok(())
    }

    fn handle_full_quality_broadcast(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        let id = reader.get_ushort()?;
        let enable = reader.get_bool()?;
        BasisServerReductionSystemEvents::set_bypass_reduction(id, enable);
        Self::send_back_message(peer, &format!("Full-quality broadcast {} for player {id}.", if enable { "ENABLED" } else { "DISABLED" }));
        Ok(())
    }

    fn target_is_protected(target: &NetPeerRef) -> bool {
        NetworkServer::net_id_to_uuid(target).is_some_and(|uuid| Self::is_protected(&uuid))
    }

    fn handle_set_locomotion_override_all(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        let fields = reader.get_byte()?;
        let jump_height = reader.get_float()?;
        let walk_speed = reader.get_float()?;
        let run_speed = reader.get_float()?;
        let gravity = reader.get_float()?;
        let movement_mode = reader.get_byte()?;

        let mut writer = NetworkServer::rent_writer();
        AdminRequest::default().serialize(&mut writer, AdminRequestMode::LocomotionOverrideApply)?;
        writer.put_ushort(peer.id() as u16);
        writer.put_byte(fields);
        writer.put_float(jump_height);
        writer.put_float(walk_speed);
        writer.put_float(run_speed);
        writer.put_float(gravity);
        writer.put_byte(movement_mode);

        let (sent, protected_skipped) = Self::send_to_all_unprotected(peer, &writer);
        NetworkServer::return_writer(writer);

        let verb = if fields == 0 { "cleared on" } else { "applied to" };
        Self::send_back_message(
            peer,
            &if protected_skipped > 0 {
                format!("Locomotion override {verb} {sent} player(s); {protected_skipped} protected player(s) skipped.")
            } else {
                format!("Locomotion override {verb} {sent} player(s).")
            },
        );
        Ok(())
    }

    /// Sends `writer` to every peer but the sender and protection holders. Returns `(sent, skipped)`.
    fn send_to_all_unprotected(peer: &NetPeerRef, writer: &NetDataWriter) -> (usize, usize) {
        let mut sent = 0;
        let mut protected_skipped = 0;
        for target in NetworkServer::peer_snapshot().iter() {
            if target.id() == peer.id() {
                continue;
            }
            if Self::target_is_protected(target) {
                protected_skipped += 1;
                continue;
            }
            NetworkServer::try_send(target, writer, BasisNetworkCommons::ADMIN_CHANNEL, DeliveryMethod::ReliableOrdered);
            sent += 1;
        }
        (sent, protected_skipped)
    }

    fn handle_set_locomotion_override(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        let target_id = reader.get_ushort()?;
        let fields = reader.get_byte()?;
        let jump_height = reader.get_float()?;
        let walk_speed = reader.get_float()?;
        let run_speed = reader.get_float()?;
        let gravity = reader.get_float()?;
        let movement_mode = reader.get_byte()?;

        let Some(target_peer) = NetworkServer::authenticated_peers().get(&i32::from(target_id)).map(|p| p.value().clone()) else {
            Self::send_back_message(peer, "Player not found");
            return Ok(());
        };
        if Self::target_is_protected(&target_peer) {
            Self::send_back_message(peer, "Target is protected");
            return Ok(());
        }
        Self::send_admin_message(&target_peer, AdminRequestMode::LocomotionOverrideApply, |w| {
            w.put_ushort(peer.id() as u16);
            w.put_byte(fields);
            w.put_float(jump_height);
            w.put_float(walk_speed);
            w.put_float(run_speed);
            w.put_float(gravity);
            w.put_byte(movement_mode);
            Ok(())
        })?;
        Self::send_back_message(
            peer,
            &if fields == 0 {
                format!("Locomotion override cleared on player {target_id}.")
            } else {
                format!("Locomotion override applied to player {target_id}.")
            },
        );
        Ok(())
    }

    /// Relays a moderator's avatar choice to the one peer it targets. The server never loads or
    /// validates the bundle itself — it only decides who is allowed to be told to wear it.
    fn handle_force_avatar(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        let target_id = reader.get_ushort()?;
        let url = reader.get_string()?;
        let password = reader.get_string()?;
        let embedded_source = reader.get_byte()?;

        if url.is_empty() {
            Self::send_back_message(peer, "Avatar url invalid");
            return Ok(());
        }
        let Some(target_peer) = NetworkServer::authenticated_peers().get(&i32::from(target_id)).map(|p| p.value().clone()) else {
            Self::send_back_message(peer, "Player not found");
            return Ok(());
        };
        // Same courtesy Kick/Ban extend: a protected user can't be dressed by another moderator.
        if Self::target_is_protected(&target_peer) {
            Self::send_back_message(peer, "Target is protected");
            return Ok(());
        }
        Self::send_admin_message(&target_peer, AdminRequestMode::ForceAvatarApply, |w| {
            w.put_ushort(peer.id() as u16);
            w.put_string(&url)?;
            w.put_string(&password)?;
            w.put_byte(embedded_source);
            Ok(())
        })?;
        Self::send_back_message(peer, &format!("Avatar forced on player {target_id}."));
        Ok(())
    }

    /// The crowd version of [`handle_force_avatar`](Self::handle_force_avatar) — one avatar, every
    /// peer. Sent peer by peer rather than through a blanket broadcast because a broadcast has no
    /// way to exempt protection holders.
    fn handle_force_avatar_all(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        let url = reader.get_string()?;
        let password = reader.get_string()?;
        let embedded_source = reader.get_byte()?;

        if url.is_empty() {
            Self::send_back_message(peer, "Avatar url invalid");
            return Ok(());
        }
        let mut writer = NetworkServer::rent_writer();
        let written = AdminRequest::default().serialize(&mut writer, AdminRequestMode::ForceAvatarApply).and_then(|_| {
            writer.put_ushort(peer.id() as u16);
            writer.put_string(&url)?;
            writer.put_string(&password)?;
            writer.put_byte(embedded_source);
            Ok(())
        });
        if let Err(e) = written {
            NetworkServer::return_writer(writer);
            return Err(e);
        }
        let (sent, protected_skipped) = Self::send_to_all_unprotected(peer, &writer);
        NetworkServer::return_writer(writer);
        Self::send_back_message(
            peer,
            &if protected_skipped > 0 {
                format!("Avatar forced on {sent} player(s); {protected_skipped} protected player(s) skipped.")
            } else {
                format!("Avatar forced on {sent} player(s).")
            },
        );
        Ok(())
    }

    fn handle_crash_reporting_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        let enabled = reader.get_bool()?;
        NetworkServer::update_configuration(|c| c.crash_reporting_enabled = enabled);
        Self::save_config();
        BasisCrashReportStateManager::set_enabled(enabled);
        BasisCrashReportStateManager::broadcast_state();
        Self::send_back_message(peer, &format!("Crash reporting {}.", if enabled { "ENABLED" } else { "DISABLED" }));
        Ok(())
    }

    fn handle_audio_range_limits_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        let microphone_meters = reader.get_float()?;
        let hearing_meters = reader.get_float()?;
        BasisAudioRangeLimitManager::set_limits(microphone_meters, hearing_meters);
        let mic = BasisAudioRangeLimitManager::max_microphone_range_meters();
        let hearing = BasisAudioRangeLimitManager::max_hearing_range_meters();
        NetworkServer::update_configuration(|c| {
            c.max_microphone_range_meters = mic;
            c.max_hearing_range_meters = hearing;
        });
        Self::save_config();
        BasisAudioRangeLimitManager::broadcast_state();
        Self::send_back_message(peer, &format!("Audio range limits set: microphone {mic} m, hearing {hearing} m."));
        Ok(())
    }

    fn handle_playspace_mover_toggle(peer: &NetPeerRef) {
        let locked = BasisGlobalLockManager::toggle_playspace_mover();
        let state = if locked { "DISABLED" } else { "ENABLED" };
        Self::broadcast_global_lock_notice(
            peer,
            &format!("Playspace mover is now {state}."),
            &format!("The playspace mover has been globally {state} for non-admins by an admin."),
        );
    }

    fn handle_direct_connect_toggle(peer: &NetPeerRef) {
        let locked = BasisGlobalLockManager::toggle_direct_connect();
        let state = if locked { "DISABLED" } else { "ENABLED" };
        Self::broadcast_global_lock_notice(
            peer,
            &format!("Direct connections are now {state}."),
            &format!("Direct (peer-to-peer) connections have been globally {state} for non-admins by an admin."),
        );
    }

    fn handle_avatar_scale_limits_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        let min_meters = reader.get_float()?;
        let max_meters = reader.get_float()?;
        BasisAvatarScaleLimitManager::set_limits(min_meters, max_meters);
        let min = BasisAvatarScaleLimitManager::min_meters();
        let max = BasisAvatarScaleLimitManager::max_meters();
        NetworkServer::update_configuration(|c| {
            c.min_avatar_eye_height_meters = min;
            c.max_avatar_eye_height_meters = max;
        });
        Self::save_config();
        BasisAvatarScaleLimitManager::broadcast_state();
        Self::send_back_message(peer, &format!("Avatar scale limits set: {min} m .. {max} m."));
        Ok(())
    }

    fn handle_resource_limits_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        let max_content_spheres_per_player = reader.get_int()?;
        BasisResourceLimitManager::set_limits(max_content_spheres_per_player);
        let spheres = BasisResourceLimitManager::max_content_spheres_per_player();
        NetworkServer::update_configuration(|c| c.max_content_spheres_per_player = spheres);
        Self::save_config();
        BasisResourceLimitManager::broadcast_state();
        Self::send_back_message(peer, &format!("Resource limits set: spheres/player {spheres}."));
        Ok(())
    }

    fn handle_reduction_settings_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        const MAX_QUALITY_DISTANCE_METERS: f32 = 1000.0;
        let interval = reader.get_int()?;
        let base_multiplier = reader.get_int()?;
        let increase_rate = reader.get_float()?;
        let slowest = reader.get_float()?;
        let high = reader.get_float()?;
        let medium = reader.get_float()?;
        let low = reader.get_float()?;
        let bundle = reader.get_bool()?;
        let bundle_min_messages = reader.get_int()?;
        let bundle_min_bytes = reader.get_int()?;
        let profiling = reader.get_bool()?;
        let zstd = reader.get_bool()?;
        let zstd_delta = reader.get_bool()?;
        let zstd_level = reader.get_int()?;
        let zstd_max_shed_tier = reader.get_int()?;

        NetworkServer::update_configuration(|config| {
            config.bsrs_millisecond_default_interval = interval.max(1);
            config.bsr_base_multiplier = base_multiplier.max(1);
            config.bsrs_increase_rate = increase_rate.max(0.0);
            config.bsr_slowest_send_rate = slowest.max(0.0);
            // Upper bounds matter as much as lower ones here: the value is persisted to config.xml
            // and an unbounded distance pins every peer to the High avatar tier permanently.
            config.high_quality_distance = high.clamp(0.0, MAX_QUALITY_DISTANCE_METERS);
            config.medium_quality_distance = medium.clamp(0.0, MAX_QUALITY_DISTANCE_METERS);
            config.low_quality_distance = low.clamp(0.0, MAX_QUALITY_DISTANCE_METERS);
            config.enable_avatar_bundle_compression = bundle;
            config.avatar_bundle_min_messages = bundle_min_messages.max(1);
            config.avatar_bundle_min_bytes = bundle_min_bytes.max(0);
            config.enable_bsr_profiling = profiling;
            config.enable_avatar_bundle_zstd = zstd;
            config.avatar_bundle_zstd_delta_bundles = zstd_delta;
            // Clamp to the range zstd actually accepts — this value arrives from an admin client
            // rather than from config.xml.
            config.avatar_bundle_zstd_level = zstd_level.clamp(BasisAvatarBundleZstd::min_level(), BasisAvatarBundleZstd::max_level());
            config.avatar_bundle_zstd_max_shed_tier = zstd_max_shed_tier;
        });

        NetworkServer::initialize_pulse_settings();
        Self::save_config();
        Self::broadcast_reduction_settings();
        let config = NetworkServer::configuration_or_default();
        Self::send_back_message(
            peer,
            &format!(
                "Reduction settings set: interval {}ms, base x{}, rate {}, slowest {}, distances {}/{}/{}m, bundle {} (min {}msg/{}B), profiling {}. SlowestSendRate applies to new joins only.",
                config.bsrs_millisecond_default_interval,
                config.bsr_base_multiplier,
                config.bsrs_increase_rate,
                config.bsr_slowest_send_rate,
                config.high_quality_distance,
                config.medium_quality_distance,
                config.low_quality_distance,
                config.enable_avatar_bundle_compression,
                config.avatar_bundle_min_messages,
                config.avatar_bundle_min_bytes,
                config.enable_bsr_profiling
            ),
        );
        Ok(())
    }

    /// Applies the image/gif bandwidth budgets from an admin. The upload figure is what the
    /// server enforces from the next packet onward and what new joiners are told to pace to; it
    /// is NOT re-advertised to players already connected (it rides ServerMetaDataMessage).
    fn handle_image_bandwidth_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        let egress = reader.get_int()?;
        let download = reader.get_int()?;
        let percent = reader.get_int()?;
        NetworkServer::update_configuration(|config| {
            // 0 is meaningful on both rates — "unmetered" for download, "client keeps its own
            // conservative default" for upload — so only negatives are corrected.
            config.image_share_egress_megabits_per_second = egress.max(0);
            config.image_share_download_megabits_per_second = download.max(0);
            // Enforcing below what was advertised would drop honest clients doing exactly what
            // they were told, which is the one outcome this feature must never produce.
            config.image_share_egress_enforcement_percent = percent.clamp(100, 1000);
        });
        Self::save_config();
        Self::broadcast_image_bandwidth();
        let config = NetworkServer::configuration_or_default();
        Self::send_back_message(
            peer,
            &format!(
                "Image bandwidth set: upload {} Mb/s per sharer (enforced at {}%), download {} Mb/s per joining player. Upload applies live as a limit; the advertised figure reaches existing players on their next join or permission refresh.",
                config.image_share_egress_megabits_per_second,
                config.image_share_egress_enforcement_percent,
                config.image_share_download_megabits_per_second
            ),
        );
        Ok(())
    }

    fn write_image_bandwidth(writer: &mut NetDataWriter) -> NetResult<()> {
        let config = NetworkServer::configuration_or_default();
        AdminRequest::default().serialize(writer, AdminRequestMode::GlobalGetImageBandwidth)?;
        writer.put_int(config.image_share_egress_megabits_per_second);
        writer.put_int(config.image_share_download_megabits_per_second);
        writer.put_int(config.image_share_egress_enforcement_percent);
        Ok(())
    }

    fn broadcast_image_bandwidth() {
        Self::broadcast_written(Self::write_image_bandwidth);
    }

    pub fn send_image_bandwidth_to_peer(peer: &NetPeerRef) {
        Self::send_written(peer, Self::write_image_bandwidth);
    }

    /// Applies the maximum player count from an admin. The gate that reads it runs per
    /// connection request, so the new cap binds from the next join onward. Setting it below the
    /// current population is allowed and drops nobody.
    fn handle_peer_limit_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        let requested = reader.get_int()?;
        // 0 or negative would seal the instance shut, and player ids are u16 on the wire, so a
        // cap past u16::MAX could never be reached anyway.
        let peer_limit = requested.clamp(1, i32::from(u16::MAX));
        NetworkServer::update_configuration(|c| c.peer_limit = peer_limit);
        Self::save_config();
        Self::broadcast_peer_limit();
        Self::send_back_message(peer, &format!("Max players set to {peer_limit}. Applies from the next join; nobody connected now is disconnected."));
        Ok(())
    }

    fn write_peer_limit(writer: &mut NetDataWriter) -> NetResult<()> {
        AdminRequest::default().serialize(writer, AdminRequestMode::GlobalGetPeerLimit)?;
        writer.put_int(NetworkServer::configuration_or_default().peer_limit);
        Ok(())
    }

    fn broadcast_peer_limit() {
        Self::broadcast_written(Self::write_peer_limit);
    }

    pub fn send_peer_limit_to_peer(peer: &NetPeerRef) {
        Self::send_written(peer, Self::write_peer_limit);
    }

    fn write_reduction_settings(writer: &mut NetDataWriter) -> NetResult<()> {
        let config = NetworkServer::configuration_or_default();
        AdminRequest::default().serialize(writer, AdminRequestMode::GlobalGetReductionSettings)?;
        writer.put_int(config.bsrs_millisecond_default_interval);
        writer.put_int(config.bsr_base_multiplier);
        writer.put_float(config.bsrs_increase_rate);
        writer.put_float(config.bsr_slowest_send_rate);
        writer.put_float(config.high_quality_distance);
        writer.put_float(config.medium_quality_distance);
        writer.put_float(config.low_quality_distance);
        writer.put_bool(config.enable_avatar_bundle_compression);
        writer.put_int(config.avatar_bundle_min_messages);
        writer.put_int(config.avatar_bundle_min_bytes);
        writer.put_bool(config.enable_bsr_profiling);
        writer.put_bool(config.enable_avatar_bundle_zstd);
        writer.put_bool(config.avatar_bundle_zstd_delta_bundles);
        writer.put_int(config.avatar_bundle_zstd_level);
        writer.put_int(config.avatar_bundle_zstd_max_shed_tier);
        Ok(())
    }

    fn broadcast_reduction_settings() {
        Self::broadcast_written(Self::write_reduction_settings);
    }

    pub fn send_reduction_settings_to_peer(peer: &NetPeerRef) {
        Self::send_written(peer, Self::write_reduction_settings);
    }

    fn broadcast_written(write: impl FnOnce(&mut NetDataWriter) -> NetResult<()>) {
        let mut writer = NetworkServer::rent_writer();
        if write(&mut writer).is_ok() {
            NetworkServer::broadcast_message_to_clients(
                &writer,
                BasisNetworkCommons::ADMIN_CHANNEL,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        }
        NetworkServer::return_writer(writer);
    }

    fn send_written(peer: &NetPeerRef, write: impl FnOnce(&mut NetDataWriter) -> NetResult<()>) {
        let mut writer = NetworkServer::rent_writer();
        if write(&mut writer).is_ok() {
            NetworkServer::try_send(peer, &writer, BasisNetworkCommons::ADMIN_CHANNEL, DeliveryMethod::ReliableOrdered);
        }
        NetworkServer::return_writer(writer);
    }

    /// `[AdminRequest mode][body]` to one peer.
    fn send_admin_message(peer: &NetPeerRef, mode: AdminRequestMode, body: impl FnOnce(&mut NetDataWriter) -> NetResult<()>) -> NetResult<()> {
        let mut writer = NetworkServer::rent_writer();
        let written = AdminRequest::default().serialize(&mut writer, mode).and_then(|_| body(&mut writer));
        if written.is_ok() {
            NetworkServer::try_send(peer, &writer, BasisNetworkCommons::ADMIN_CHANNEL, DeliveryMethod::ReliableOrdered);
        }
        NetworkServer::return_writer(writer);
        written
    }

    /// `[AdminRequest mode][body]` to everyone but `sender`.
    fn broadcast_admin_message_excluding(sender: &NetPeerRef, mode: AdminRequestMode, body: impl FnOnce(&mut NetDataWriter) -> NetResult<()>) -> NetResult<()> {
        let mut writer = NetworkServer::rent_writer();
        let written = AdminRequest::default().serialize(&mut writer, mode).and_then(|_| body(&mut writer));
        if written.is_ok() {
            NetworkServer::broadcast_message_to_clients_excluding(
                &writer,
                BasisNetworkCommons::ADMIN_CHANNEL,
                sender,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        }
        NetworkServer::return_writer(writer);
        written
    }

    /// Reply to the toggling admin, broadcast a one-line notice to everyone, then push the
    /// refreshed lock-state payload.
    fn broadcast_global_lock_notice(peer: &NetPeerRef, admin_reply: &str, broadcast_notice: &str) {
        BNL::log(broadcast_notice);
        Self::send_back_message(peer, admin_reply);
        Self::broadcast_written(|w| {
            AdminRequest::default().serialize(w, AdminRequestMode::MessageAll)?;
            w.put_string(broadcast_notice)
        });
        Self::persist_global_lock_state();
        BasisGlobalLockManager::broadcast_lock_state();
    }

    fn handle_global_toggle(peer: &NetPeerRef, content_type: &str, now_locked: bool) {
        let state = if now_locked { "DISABLED" } else { "ENABLED" };
        Self::handle_global_state_notification(peer, &format!("{content_type} loading has been globally {state} by an admin."));
    }

    /// Notification for locks over a live feature rather than content loading (chat, voice,
    /// grabbing, ...): a plain "X has been ... DISABLED".
    fn handle_global_feature_toggle(peer: &NetPeerRef, feature_name: &str, now_locked: bool) {
        let state = if now_locked { "DISABLED" } else { "ENABLED" };
        Self::handle_global_state_notification(peer, &format!("{feature_name} has been globally {state} by an admin."));
    }

    /// Notification for a protection that is ENABLED when its flag is set — the opposite sense to
    /// every lock above.
    fn handle_global_protection_toggle(peer: &NetPeerRef, protection_name: &str, now_enforced: bool) {
        let state = if now_enforced { "ENABLED" } else { "DISABLED" };
        Self::handle_global_state_notification(peer, &format!("{protection_name} has been globally {state} by an admin."));
    }

    /// Notifies the toggling admin + all clients with a pre-composed, unambiguous message and
    /// rebroadcasts the lock state.
    fn handle_global_state_notification(peer: &NetPeerRef, notification: &str) {
        BNL::log(notification);
        // Notify the admin who toggled it
        Self::send_back_message(peer, notification);
        // Notify all clients about the change
        Self::broadcast_written(|w| {
            AdminRequest::default().serialize(w, AdminRequestMode::MessageAll)?;
            w.put_string(notification)
        });
        // Broadcast updated lock state so clients track it
        Self::persist_global_lock_state();
        BasisGlobalLockManager::broadcast_lock_state();
    }

    /// Mirrors the live global lock state onto Configuration and writes config.xml. Every lock
    /// seeds itself from config at boot, so a toggle that isn't persisted here silently reverts
    /// on the next restart.
    fn persist_global_lock_state() {
        NetworkServer::update_configuration(BasisGlobalLockManager::write_to_config);
        Self::save_config();
    }

    fn handle_headless_audio_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        if reader.available_bytes() < 1 {
            Self::send_back_message(peer, "Failed to set headless audio clip playback: missing state value.");
            return Ok(());
        }
        let headless_audio_off = reader.get_bool()?;
        let changed = BasisHeadlessAudioStateManager::set_headless_audio(headless_audio_off);
        let state = if headless_audio_off { "OFF" } else { "ON" };
        let notification = if changed {
            format!("Headless audio clip playback is now {state}.")
        } else {
            format!("Headless audio clip playback was already {state}.")
        };
        BNL::log(&notification);
        Self::send_back_message(peer, &notification);
        BasisHeadlessAudioStateManager::broadcast_state();
        Ok(())
    }

    fn handle_headless_disallow_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        if reader.available_bytes() < 1 {
            Self::send_back_message(peer, "Failed to set headless connection policy: missing state value.");
            return Ok(());
        }
        let disallow_headless = reader.get_bool()?;
        let changed = BasisHeadlessConnectionPolicyManager::set_disallow_headless(disallow_headless);
        let state = if disallow_headless { "DISALLOWED" } else { "ALLOWED" };
        let notification =
            if changed { format!("Headless clients are now {state}.") } else { format!("Headless clients were already {state}.") };
        BNL::log(&notification);
        Self::send_back_message(peer, &notification);
        if disallow_headless {
            BasisHeadlessConnectionPolicyManager::disconnect_connected_headless_peers();
        }
        // Seeded from Configuration.DisallowHeadless at boot — persist or it reverts on restart.
        let disallowed = BasisHeadlessConnectionPolicyManager::headless_disallowed();
        NetworkServer::update_configuration(|c| c.disallow_headless = disallowed);
        Self::save_config();
        BasisHeadlessConnectionPolicyManager::broadcast_state();
        Ok(())
    }

    fn handle_opus_packet_loss_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        if reader.available_bytes() < 1 {
            Self::send_back_message(peer, "Failed to set Opus packet loss: missing value byte.");
            return Ok(());
        }
        let percent = i32::from(reader.get_byte()?);
        let changed = BasisOpusPacketLossStateManager::set_packet_loss_percent(percent);
        let applied = BasisOpusPacketLossStateManager::packet_loss_percent();
        let notification =
            if changed { format!("Opus FEC packet-loss % is now {applied}.") } else { format!("Opus FEC packet-loss % was already {applied}.") };
        BNL::log(&notification);
        Self::send_back_message(peer, &notification);
        BasisOpusPacketLossStateManager::broadcast_state();
        Ok(())
    }

    fn handle_camera_policy_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        if reader.available_bytes() < 1 {
            Self::send_back_message(peer, "Failed to set camera metadata policy: missing mask byte.");
            return Ok(());
        }
        let mask = reader.get_byte()?;
        BasisGlobalLockManager::set_camera_metadata_disallow_mask(mask);
        BNL::log(format!("Camera photo-metadata disallow mask set to {mask}."));
        Self::send_back_message(peer, &format!("Camera metadata policy updated (mask {mask})."));
        Self::persist_global_lock_state();
        BasisGlobalLockManager::broadcast_lock_state();
        Ok(())
    }

    fn handle_user_opus_bitrate_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        if reader.available_bytes() < 6 {
            // ushort + int
            Self::send_back_message(peer, "Failed to set user Opus bitrate: missing payload.");
            return Ok(());
        }
        let target_id = reader.get_ushort()?;
        let requested = reader.get_int()?;
        let applied = BasisUserOpusBitrateStateManager::set_bitrate(i32::from(target_id), requested);
        if let Some(target_peer) = NetworkServer::authenticated_peers().get(&i32::from(target_id)) {
            BasisUserOpusBitrateStateManager::send_override_to_peer(
                target_peer.value(),
                BasisUserOpusBitrateStateManager::effective_bitrate_for(i32::from(target_id)),
            );
        }
        let notification = if applied == 0 {
            format!("Cleared Opus bitrate override for player {target_id}.")
        } else {
            format!("Opus bitrate override for player {target_id} is now {applied} bps.")
        };
        BNL::log(&notification);
        Self::send_back_message(peer, &notification);
        Ok(())
    }

    fn handle_global_opus_bitrate_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        if reader.available_bytes() < 4 {
            Self::send_back_message(peer, "Failed to set global Opus bitrate: missing payload.");
            return Ok(());
        }
        let requested = reader.get_int()?;
        let applied = BasisUserOpusBitrateStateManager::set_global_bitrate(requested);
        let notification = if applied == 0 {
            "Cleared the global Opus bitrate; clients use their default (or their per-user override).".to_string()
        } else {
            format!("Global Opus bitrate is now {applied} bps (per-user overrides still win).")
        };
        BNL::log(&notification);
        Self::send_back_message(peer, &notification);
        BasisUserOpusBitrateStateManager::push_effective_to_all_peers();
        BasisUserOpusBitrateStateManager::broadcast_global_state();
        Ok(())
    }

    fn handle_opus_frame_duration_set(peer: &NetPeerRef, reader: &mut NetPacketReader) -> NetResult<()> {
        if reader.available_bytes() < 1 {
            Self::send_back_message(peer, "Failed to set Opus frame duration: missing value byte.");
            return Ok(());
        }
        let requested = i32::from(reader.get_byte()?);
        if !BasisOpusFrameDurationStateManager::is_accepted_duration(requested) {
            Self::send_back_message(peer, &format!("Failed to set Opus frame duration: only 20 or 40 ms are accepted (got {requested})."));
            return Ok(());
        }
        let changed = BasisOpusFrameDurationStateManager::set_frame_duration_ms(requested);
        let applied = BasisOpusFrameDurationStateManager::frame_duration_ms();
        let notification =
            if changed { format!("Opus frame duration is now {applied} ms.") } else { format!("Opus frame duration was already {applied} ms.") };
        BNL::log(&notification);
        Self::send_back_message(peer, &notification);
        BasisOpusFrameDurationStateManager::broadcast_state();
        Ok(())
    }

    pub fn send_back_message(peer: &NetPeerRef, msg: &str) {
        if msg.is_empty() {
            return;
        }
        Self::send_written(peer, |w| {
            AdminRequest::default().serialize(w, AdminRequestMode::Message)?;
            w.put_string(msg)
        });
    }

    pub fn get_banned_reason(uuid: &str) -> Option<String> {
        BANNED_PLAYERS.get(uuid).map(|p| p.reason.clone())
    }

    pub fn is_ip_banned(ip: &str) -> bool {
        if ip.trim().is_empty() {
            return false;
        }
        BANNED_PLAYERS.iter().any(|p| p.has_banned_ip && p.banned_ip == ip)
    }
}
