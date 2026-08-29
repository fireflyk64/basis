//! Port of `Networking/BasisNetworkContentShare.cs`: server-side management of content share
//! spheres. Tracks all active spheres and handles broadcasting to clients.

use std::sync::LazyLock;

use basis_network_core::SerializableBasis::{
    ContentShareCleanupMessage, ContentShareMessage, ContentShareType, PlayerIdMessage, ServerContentShareCleanupMessage,
    ServerContentShareMessage,
};
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetPacketReader, NetPeerRef};
use dashmap::DashMap;

use crate::NetworkServer;
use crate::networking::BasisSavedState;
use crate::security::{BasisGlobalLockManager, BasisPlayerModeration, BasisResourceLimitManager, PermNodes, PermissionIntegration};

/// All active content share spheres keyed by SphereNetID. Value is the full message including
/// creator player ID.
static ACTIVE_SPHERES: LazyLock<DashMap<String, ServerContentShareMessage>> = LazyLock::new(DashMap::new);

pub struct BasisNetworkContentShare;

impl BasisNetworkContentShare {
    pub fn active_spheres() -> &'static DashMap<String, ServerContentShareMessage> {
        &ACTIVE_SPHERES
    }

    /// Handles a content share drop from a client: stores the sphere and broadcasts to all
    /// clients.
    pub fn handle_content_share_drop(mut reader: NetPacketReader, peer: &NetPeerRef) {
        let mut msg = ContentShareMessage::default();
        if let Err(e) = msg.deserialize(&mut reader) {
            BNL::log_error(format!("Malformed content share from peer {}: {e}", peer.id()));
            return;
        }

        if !PermissionIntegration::has_valid_requirement(peer, PermNodes::CONTENT_SHARE_CREATE) {
            return;
        }

        // Global lock check based on content type: blocked when the content's lock is on AND the
        // peer lacks the matching lockbypass permission.
        let (blocked, content_name) = match msg.content_type {
            ContentShareType::Avatar => (
                BasisGlobalLockManager::avatars_locked() && !PermissionIntegration::has_valid_requirement(peer, PermNodes::RESOURCE_LOCK_BYPASS_AVATAR),
                "Avatar",
            ),
            ContentShareType::Prop => (
                BasisGlobalLockManager::props_locked() && !PermissionIntegration::has_valid_requirement(peer, PermNodes::RESOURCE_LOCK_BYPASS_PROP),
                "Prop",
            ),
            ContentShareType::World => (
                BasisGlobalLockManager::worlds_locked() && !PermissionIntegration::has_valid_requirement(peer, PermNodes::RESOURCE_LOCK_BYPASS_WORLD),
                "World",
            ),
            // ContentURL carries the connection string (address[:port][#password]). UnlockPassword
            // is intentionally unused — receivers parse the URL directly.
            ContentShareType::Server => (
                BasisGlobalLockManager::servers_locked() && !PermissionIntegration::has_valid_requirement(peer, PermNodes::RESOURCE_LOCK_BYPASS_SERVER),
                "Server share",
            ),
        };
        if blocked {
            BNL::log(format!("{content_name} content sharing is globally disabled. Rejected from peer {}", peer.id()));
            BasisPlayerModeration::send_back_message(peer, &format!("{content_name} loading is currently disabled by an admin."));
            return;
        }

        let owned = ACTIVE_SPHERES.iter().filter(|e| e.value().player_id_message.player_id == peer.id() as u16).count();
        if owned >= usize::try_from(BasisResourceLimitManager::max_content_spheres_per_player()).unwrap_or(0) {
            BNL::log_error(format!("Peer {} reached content sphere limit.", peer.id()));
            return;
        }

        let (sharer_uuid, sharer_display_name) = BasisSavedState::get_last_player_meta_data(peer)
            .map(|meta| (meta.player_uuid, meta.player_display_name))
            .unwrap_or_default();

        let sphere_net_id = msg.sphere_net_id.clone();
        let content_type = msg.content_type;
        let mut server_msg = ServerContentShareMessage {
            player_id_message: PlayerIdMessage::new(peer.id() as u16),
            sharer_uuid,
            sharer_display_name,
            content_share_message: msg,
        };

        match ACTIVE_SPHERES.entry(sphere_net_id.clone()) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                BNL::log_error(format!("Content sphere already exists: {sphere_net_id}"));
            }
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(server_msg.clone());
                BNL::log(format!("Content sphere dropped: {sphere_net_id} type={content_type:?}"));

                let mut writer = NetworkServer::rent_writer();
                writer.put_byte(BasisNetworkCommons::CONTENT_SHARE_SUB_DROP);
                if server_msg.serialize(&mut writer).is_ok() {
                    // Broadcast to all clients including sender
                    NetworkServer::broadcast_message_to_clients(
                        &writer,
                        BasisNetworkCommons::CONTENT_SHARE_CHANNEL,
                        &NetworkServer::peer_snapshot(),
                        DeliveryMethod::ReliableOrdered,
                    );
                }
                NetworkServer::return_writer(writer);
            }
        }
    }

    /// Handles a content share cleanup from a client: removes the sphere and broadcasts removal
    /// to all clients.
    pub fn handle_content_share_cleanup(mut reader: NetPacketReader, peer: &NetPeerRef) {
        let mut msg = ContentShareCleanupMessage::default();
        if let Err(e) = msg.deserialize(&mut reader) {
            BNL::log_error(format!("Malformed content share cleanup from peer {}: {e}", peer.id()));
            return;
        }

        let Some(existing_owner) = ACTIVE_SPHERES.get(&msg.sphere_net_id).map(|e| e.player_id_message.player_id) else {
            BNL::log_error(format!("Trying to remove content sphere that does not exist: {}", msg.sphere_net_id));
            return;
        };
        if !PermissionIntegration::has_valid_requirement(peer, PermNodes::CONTENT_SHARE_DELETE) {
            return;
        }
        // ContentShareDelete is default-granted, so the sharer check is what stops one player
        // deleting everyone else's orbs.
        if existing_owner != peer.id() as u16 && !PermissionIntegration::has_valid_requirement(peer, PermNodes::PROTECTION) {
            BNL::log_error(format!("Peer {} tried to remove content sphere {} they did not share.", peer.id(), msg.sphere_net_id));
            return;
        }
        if ACTIVE_SPHERES.remove(&msg.sphere_net_id).is_some() {
            BNL::log(format!("Content sphere removed: {}", msg.sphere_net_id));
            Self::broadcast_cleanup(peer.id() as u16, msg);
        }
    }

    fn broadcast_cleanup(player_id: u16, cleanup: ContentShareCleanupMessage) {
        let mut server_msg =
            ServerContentShareCleanupMessage { player_id_message: PlayerIdMessage::new(player_id), content_share_cleanup_message: cleanup };
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(BasisNetworkCommons::CONTENT_SHARE_SUB_CLEANUP);
        if server_msg.serialize(&mut writer).is_ok() {
            NetworkServer::broadcast_message_to_clients(
                &writer,
                BasisNetworkCommons::CONTENT_SHARE_CHANNEL,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        }
        NetworkServer::return_writer(writer);
    }

    /// Sends all active content share spheres to a newly connected peer.
    pub fn send_all_spheres_to_peer(new_connection: &NetPeerRef) {
        let spheres: Vec<ServerContentShareMessage> = ACTIVE_SPHERES.iter().map(|e| e.value().clone()).collect();
        if spheres.is_empty() {
            return;
        }
        let mut writer = NetworkServer::rent_writer();
        for mut sphere in spheres {
            writer.reset();
            writer.put_byte(BasisNetworkCommons::CONTENT_SHARE_SUB_DROP);
            if sphere.serialize(&mut writer).is_ok() {
                NetworkServer::try_send(new_connection, &writer, BasisNetworkCommons::CONTENT_SHARE_CHANNEL, DeliveryMethod::ReliableOrdered);
            }
        }
        NetworkServer::return_writer(writer);
    }

    /// Removes all spheres created by a disconnecting player.
    pub fn remove_player_spheres(peer_id: i32) {
        let player_id = peer_id as u16;
        let to_remove: Vec<String> = ACTIVE_SPHERES
            .iter()
            .filter(|e| e.value().player_id_message.player_id == player_id)
            .map(|e| e.key().clone())
            .collect();
        for sphere_id in to_remove {
            if ACTIVE_SPHERES.remove(&sphere_id).is_some() {
                Self::broadcast_cleanup(player_id, ContentShareCleanupMessage { sphere_net_id: sphere_id });
            }
        }
    }

    /// Clears all non-persistent spheres (called when server empties).
    pub fn reset() {
        ACTIVE_SPHERES.clear();
    }
}
