//! Port of `BasisNetworkServer/Security`.
pub mod basis_allow_list;
pub mod basis_audio_range_limit_manager;
pub mod basis_avatar_scale_limit_manager;
pub mod basis_ban_list;
pub mod basis_crash_report_state_manager;
pub mod basis_did_auth_identity;
pub mod basis_global_lock_manager;
pub mod basis_headless_audio_state_manager;
pub mod basis_headless_connection_policy_manager;
pub mod basis_opus_frame_duration_state_manager;
pub mod basis_opus_packet_loss_state_manager;
pub mod basis_player_moderation;
pub mod basis_rejoin_lock_manager;
pub mod basis_resource_limit_manager;
pub mod basis_server_log_bundle_service;
pub mod basis_user_opus_bitrate_state_manager;
pub mod permission_manager;

pub use basis_allow_list::BasisAllowList;
pub use basis_audio_range_limit_manager::BasisAudioRangeLimitManager;
pub use basis_avatar_scale_limit_manager::BasisAvatarScaleLimitManager;
pub use basis_ban_list::BasisBanList;
pub use basis_crash_report_state_manager::BasisCrashReportStateManager;
pub use basis_did_auth_identity::BasisDIDAuthIdentity;
pub use basis_global_lock_manager::BasisGlobalLockManager;
pub use basis_headless_audio_state_manager::BasisHeadlessAudioStateManager;
pub use basis_headless_connection_policy_manager::BasisHeadlessConnectionPolicyManager;
pub use basis_opus_frame_duration_state_manager::BasisOpusFrameDurationStateManager;
pub use basis_opus_packet_loss_state_manager::BasisOpusPacketLossStateManager;
pub use basis_player_moderation::BasisPlayerModeration;
pub use basis_rejoin_lock_manager::BasisRejoinLockManager;
pub use basis_resource_limit_manager::BasisResourceLimitManager;
pub use basis_server_log_bundle_service::BasisServerLogBundleService;
pub use basis_user_opus_bitrate_state_manager::BasisUserOpusBitrateStateManager;
pub use permission_manager::{EffectivePermissions, PermNodes, PermissionGroup, PermissionIntegration, PermissionManager, PermissionStore, PermissionUser, PermissionXml};

use basis_network_core::SerializableBasis::{AdminRequest, AdminRequestMode};
use basis_network_core::{BasisNetworkCommons, DeliveryMethod, NetDataWriter, NetPeerRef, NetResult};

use crate::NetworkServer;

/// The shape every state manager's `send_state_to_peer` / `broadcast_state` share: rent a
/// writer, write `[AdminRequest mode][state]`, send, return. `write` fills the state after the
/// mode byte.
pub(crate) fn send_admin_state_to_peer(peer: &NetPeerRef, mode: AdminRequestMode, write: impl FnOnce(&mut NetDataWriter) -> NetResult<()>) {
    let mut writer = NetworkServer::rent_writer();
    let ok = AdminRequest::default().serialize(&mut writer, mode).and_then(|_| write(&mut writer)).is_ok();
    if ok {
        NetworkServer::try_send(peer, &writer, BasisNetworkCommons::ADMIN_CHANNEL, DeliveryMethod::ReliableOrdered);
    }
    NetworkServer::return_writer(writer);
}

pub(crate) fn broadcast_admin_state(mode: AdminRequestMode, write: impl FnOnce(&mut NetDataWriter) -> NetResult<()>) {
    let mut writer = NetworkServer::rent_writer();
    let ok = AdminRequest::default().serialize(&mut writer, mode).and_then(|_| write(&mut writer)).is_ok();
    if ok {
        NetworkServer::broadcast_message_to_clients(
            &writer,
            BasisNetworkCommons::ADMIN_CHANNEL,
            &NetworkServer::peer_snapshot(),
            DeliveryMethod::ReliableOrdered,
        );
    }
    NetworkServer::return_writer(writer);
}
