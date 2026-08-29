//! Port of `Networking/BasisSavedState.cs`: the per-player state the server replays to late
//! joiners and consults on every voice packet.

use std::sync::{Arc, LazyLock};

use basis_network_core::SerializableBasis::{ClientAvatarChangeMessage, ClientBodyFitMessage, ClientMetaDataMessage, ReadyMessage, VoiceReceiversMessage};
use basis_network_core::NetPeerRef;
use dashmap::DashMap;
use parking_lot::Mutex;

use crate::NetworkServer;

pub type ResolvedPeers = Arc<Mutex<Vec<NetPeerRef>>>;

static AVATAR_CHANGE_STATES: LazyLock<DashMap<i32, ClientAvatarChangeMessage>> = LazyLock::new(DashMap::new);
static PLAYER_META_DATA_MESSAGES: LazyLock<DashMap<i32, ClientMetaDataMessage>> = LazyLock::new(DashMap::new);
static RESOLVED_VOICE_PEERS: LazyLock<DashMap<i32, ResolvedPeers>> = LazyLock::new(DashMap::new);
static SHOUT_MODE_STATES: LazyLock<DashMap<i32, ()>> = LazyLock::new(DashMap::new);

pub struct BasisSavedState;

impl BasisSavedState {
    /// Removes all state data for a specific player and purges them from every other player's
    /// cached voice-peer list.
    pub fn remove_player(id: i32) {
        AVATAR_CHANGE_STATES.remove(&id);
        PLAYER_META_DATA_MESSAGES.remove(&id);
        RESOLVED_VOICE_PEERS.remove(&id);
        SHOUT_MODE_STATES.remove(&id);

        // Purge the disconnected peer from all other players' cached lists so voice packets
        // aren't sent to a dead peer until the next recipient update.
        for entry in RESOLVED_VOICE_PEERS.iter() {
            entry.value().lock().retain(|p| p.id() != id);
        }
    }

    /// Adds or updates the ReadyMessage for a player.
    pub fn add_last_ready_message(client: &NetPeerRef, ready_message: &ReadyMessage) {
        let id = client.id();
        AVATAR_CHANGE_STATES.insert(id, ready_message.client_avatar_change_message.clone());
        PLAYER_META_DATA_MESSAGES.insert(id, ready_message.player_meta_data_message.clone());
    }

    /// Resolves a VoiceReceiversMessage into the cached peer list.
    pub fn add_last_voice_receivers(client: &NetPeerRef, voice_receivers_message: &mut VoiceReceiversMessage) {
        let peers = Self::get_or_create_resolved_list(client.id());
        if let Some(users) = voice_receivers_message.users.as_ref() {
            let mut peers = peers.lock();
            peers.clear();
            for user in users.iter().take(voice_receivers_message.users_length) {
                if let Some(found) = NetworkServer::authenticated_peers().get(&i32::from(*user)) {
                    peers.push(found.value().clone());
                }
            }
            drop(peers);
            voice_receivers_message.return_pool();
        }
    }

    /// Adds or updates the ClientAvatarChangeMessage for a player.
    pub fn add_last_avatar_change(client: &NetPeerRef, avatar_change_message: ClientAvatarChangeMessage) {
        AVATAR_CHANGE_STATES.insert(client.id(), avatar_change_message);
    }

    /// Merges a body-fit update into a player's saved avatar record, leaving the avatar itself
    /// untouched. Stored on the same record so the late-join replay carries the wearer's current
    /// proportions; if the fit lands before any avatar change (a recalibration during load), it is
    /// held on a byte_array-less placeholder that a later avatar change fills in.
    pub fn update_body_fit(client: &NetPeerRef, body_fit: &ClientBodyFitMessage) {
        let mut entry = AVATAR_CHANGE_STATES.entry(client.id()).or_insert_with(|| ClientAvatarChangeMessage {
            load_mode: 0,
            byte_array: None,
            local_avatar_index: 0,
            ..Default::default()
        });
        entry.arm_scale = body_fit.arm_scale;
        entry.leg_scale = body_fit.leg_scale;
        entry.torso_scale = body_fit.torso_scale;
    }

    /// Retrieves the last ClientAvatarChangeMessage for a player.
    pub fn get_last_avatar_change_state(client: &NetPeerRef) -> Option<ClientAvatarChangeMessage> {
        AVATAR_CHANGE_STATES.get(&client.id()).map(|m| m.clone())
    }

    /// Retrieves the last PlayerMetaDataMessage for a player.
    pub fn get_last_player_meta_data(client: &NetPeerRef) -> Option<ClientMetaDataMessage> {
        PLAYER_META_DATA_MESSAGES.get(&client.id()).map(|m| m.clone())
    }

    /// The cached resolved peer list for a player's voice receivers. Rebuilt each time the voice
    /// receivers message is updated, not per voice packet.
    pub fn get_resolved_voice_peers(client: &NetPeerRef) -> Option<ResolvedPeers> {
        RESOLVED_VOICE_PEERS.get(&client.id()).map(|p| p.clone())
    }

    /// The resolved voice peer list for a player, created empty on first use. Used by the
    /// inverted-list and bitfield modes which resolve peers during deserialization.
    pub fn get_or_create_resolved_list(client_id: i32) -> ResolvedPeers {
        RESOLVED_VOICE_PEERS.entry(client_id).or_insert_with(|| Arc::new(Mutex::new(Vec::with_capacity(64)))).clone()
    }

    pub fn set_shout_mode(peer_id: i32, enabled: bool) {
        if enabled {
            SHOUT_MODE_STATES.insert(peer_id, ());
        } else {
            SHOUT_MODE_STATES.remove(&peer_id);
        }
    }

    pub fn is_in_shout_mode(peer_id: i32) -> bool {
        SHOUT_MODE_STATES.contains_key(&peer_id)
    }

    pub fn get_all_shout_mode_players() -> Vec<i32> {
        SHOUT_MODE_STATES.iter().map(|e| *e.key()).collect()
    }

    /// Drops everything. Used when the server stops and by tests.
    pub fn reset() {
        AVATAR_CHANGE_STATES.clear();
        PLAYER_META_DATA_MESSAGES.clear();
        RESOLVED_VOICE_PEERS.clear();
        SHOUT_MODE_STATES.clear();
    }
}
