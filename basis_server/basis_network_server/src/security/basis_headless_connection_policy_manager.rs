//! Port of `Security/BasisHeadlessConnectionPolicyManager.cs`.

use std::sync::atomic::{AtomicBool, Ordering};

use basis_network_core::SerializableBasis::{AdminRequestMode, ClientMetaDataMessage};
use basis_network_core::NetPeerRef;

use super::{broadcast_admin_state, send_admin_state_to_peer};
use crate::NetworkServer;
use crate::core::basis_server_handle_events::BasisServerHandleEvents;
use crate::networking::BasisSavedState;

/// Runtime-only server policy controlling whether headless clients may stay connected.
pub struct BasisHeadlessConnectionPolicyManager;

static HEADLESS_DISALLOWED: AtomicBool = AtomicBool::new(false);

impl BasisHeadlessConnectionPolicyManager {
    pub const DISALLOWED_REASON: &'static str = "Headless client disallowed by server.";

    pub fn headless_disallowed() -> bool {
        HEADLESS_DISALLOWED.load(Ordering::Acquire)
    }

    pub fn initialize_from_config(disallow_headless: bool) {
        HEADLESS_DISALLOWED.store(disallow_headless, Ordering::Release);
    }

    pub fn set_disallow_headless(disallow_headless: bool) -> bool {
        HEADLESS_DISALLOWED.swap(disallow_headless, Ordering::AcqRel) != disallow_headless
    }

    pub fn is_headless_client(meta_data: &ClientMetaDataMessage) -> bool {
        Self::is_headless_platform(&meta_data.player_platform)
    }

    /// An exact (case-insensitive) match on the platform id; a padded id is not a server platform.
    pub fn is_headless_platform(player_platform: &str) -> bool {
        if player_platform.is_empty() {
            return false;
        }
        ["Headless", "WindowsServer", "LinuxServer", "OSXServer"].iter().any(|known| known.eq_ignore_ascii_case(player_platform))
    }

    pub fn disconnect_connected_headless_peers() {
        for peer in NetworkServer::peer_snapshot().iter() {
            let Some(meta_data) = BasisSavedState::get_last_player_meta_data(peer) else {
                continue;
            };
            if !Self::is_headless_client(&meta_data) {
                continue;
            }
            BasisServerHandleEvents::reject_with_reason(peer, Self::DISALLOWED_REASON);
        }
    }

    pub fn send_state_to_peer(peer: &NetPeerRef) {
        send_admin_state_to_peer(peer, AdminRequestMode::GlobalGetHeadlessDisallowState, |writer| {
            writer.put_bool(Self::headless_disallowed());
            Ok(())
        });
    }

    pub fn broadcast_state() {
        broadcast_admin_state(AdminRequestMode::GlobalGetHeadlessDisallowState, |writer| {
            writer.put_bool(Self::headless_disallowed());
            Ok(())
        });
    }
}
