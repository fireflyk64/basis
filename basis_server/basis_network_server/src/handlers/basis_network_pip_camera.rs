//! Port of `Handlers/BasisNetworkPIPCamera.cs`: picture-in-picture camera state relay.

use std::sync::{Arc, LazyLock};

use basis_network_core::SerializableBasis::{CameraPIPPositionMessage, CameraPIPStateMessage, ClientCameraPIPPositionMessage, ClientCameraPIPStateMessage};
use basis_network_core::mathematics::Vector3;
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetPacketReader, NetPeerRef};
use dashmap::DashMap;
use parking_lot::Mutex;

use crate::NetworkServer;
use crate::reduction::BasisServerReductionSystemEvents;

#[derive(Clone, Debug, Default)]
pub struct CameraPIPPose {
    pub is_active: bool,
    pub position_x: f32,
    pub position_y: f32,
    pub position_z: f32,
    pub rotation_x: f32,
    pub rotation_y: f32,
    pub rotation_z: f32,
    pub rotation_w: f32,
    pub has_new_data: bool,
}

/// One player's PIP camera. The pose is written from the network receive thread and read from
/// the reduction tick; `last_sent_times` is written from the tick and cleared/removed from the
/// receive thread, so both are independently locked.
#[derive(Default)]
pub struct CameraPIPState {
    pub pose: Mutex<CameraPIPPose>,
    pub last_sent_times: DashMap<i32, i64>,
}

static PIP_STATES: LazyLock<DashMap<i32, Arc<CameraPIPState>>> = LazyLock::new(DashMap::new);

pub struct BasisNetworkPIPCamera;

impl BasisNetworkPIPCamera {
    pub fn pip_states() -> &'static DashMap<i32, Arc<CameraPIPState>> {
        &PIP_STATES
    }

    /// Client says their PIP camera was created or destroyed.
    pub fn handle_pip_state_change(mut reader: NetPacketReader, peer: &NetPeerRef) {
        let mut client_msg = ClientCameraPIPStateMessage::default();
        if client_msg.deserialize(&mut reader).is_err() {
            return;
        }
        let peer_id = peer.id() as u16;
        if client_msg.is_active {
            let state = PIP_STATES.entry(peer.id()).or_default().clone();
            let mut pose = state.pose.lock();
            pose.is_active = true;
            pose.position_x = client_msg.position_x;
            pose.position_y = client_msg.position_y;
            pose.position_z = client_msg.position_z;
            pose.rotation_x = client_msg.rotation_x;
            pose.rotation_y = client_msg.rotation_y;
            pose.rotation_z = client_msg.rotation_z;
            pose.rotation_w = client_msg.rotation_w;
            pose.has_new_data = true;
            drop(pose);
            BNL::log(format!("PIP camera created for player {peer_id}"));
        } else {
            if let Some(state) = PIP_STATES.get(&peer.id()).map(|s| s.clone()) {
                let mut pose = state.pose.lock();
                pose.is_active = false;
                pose.has_new_data = false;
                drop(pose);
                state.last_sent_times.clear();
            }
            BNL::log(format!("PIP camera destroyed for player {peer_id}"));
        }

        // Broadcast state to all peers
        let mut out_msg = CameraPIPStateMessage {
            player_id: peer_id,
            is_active: client_msg.is_active,
            position_x: client_msg.position_x,
            position_y: client_msg.position_y,
            position_z: client_msg.position_z,
            rotation_x: client_msg.rotation_x,
            rotation_y: client_msg.rotation_y,
            rotation_z: client_msg.rotation_z,
            rotation_w: client_msg.rotation_w,
        };
        let mut writer = NetworkServer::rent_writer();
        if out_msg.serialize(&mut writer).is_ok() {
            NetworkServer::broadcast_message_to_clients_excluding(
                &writer,
                BasisNetworkCommons::CAMERA_PIP_STATE_CHANNEL,
                peer,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        }
        NetworkServer::return_writer(writer);
    }

    /// Client sends a position update for their PIP camera.
    pub fn handle_pip_position_update(mut reader: NetPacketReader, peer: &NetPeerRef) {
        let mut client_msg = ClientCameraPIPPositionMessage::default();
        if client_msg.deserialize(&mut reader).is_err() {
            return;
        }
        let Some(state) = PIP_STATES.get(&peer.id()).map(|s| s.clone()) else {
            return; // ignore position updates for non-existent PIPs
        };
        let mut pose = state.pose.lock();
        if !pose.is_active {
            return;
        }
        pose.position_x = client_msg.position_x;
        pose.position_y = client_msg.position_y;
        pose.position_z = client_msg.position_z;
        pose.rotation_x = client_msg.rotation_x;
        pose.rotation_y = client_msg.rotation_y;
        pose.rotation_z = client_msg.rotation_z;
        pose.rotation_w = client_msg.rotation_w;
        pose.has_new_data = true;
    }

    /// Called from the reduction system tick loop. Sends PIP position updates to recipients
    /// using the same distance-based interval as avatar movement. `now_ms` is the tick clock in
    /// milliseconds.
    pub fn update_pip_positions(now_ms: i64) {
        let peers = NetworkServer::peer_snapshot();
        let states: Vec<(i32, Arc<CameraPIPState>)> = PIP_STATES.iter().map(|e| (*e.key(), e.value().clone())).collect();
        for (owner_id, pip_state) in states {
            let pose = pip_state.pose.lock().clone();
            if !pose.is_active || !pose.has_new_data {
                continue;
            }
            // Get the PIP owner's player position for distance calc
            let Some(owner_position) = BasisServerReductionSystemEvents::try_get_active_position(owner_id) else {
                continue;
            };
            // Build the outbound message once
            let mut pos_msg = CameraPIPPositionMessage {
                player_id: owner_id as u16,
                position_x: pose.position_x,
                position_y: pose.position_y,
                position_z: pose.position_z,
                rotation_x: pose.rotation_x,
                rotation_y: pose.rotation_y,
                rotation_z: pose.rotation_z,
                rotation_w: pose.rotation_w,
            };
            let mut writer = NetworkServer::rent_writer();
            if pos_msg.serialize(&mut writer).is_ok() {
                for recipient in peers.iter() {
                    let recipient_id = recipient.id();
                    if recipient_id == owner_id {
                        continue;
                    }
                    let Some(recipient_position) = BasisServerReductionSystemEvents::try_get_active_position(recipient_id) else {
                        continue;
                    };
                    // Distance between recipient and PIP owner
                    let dist_sq = Self::distance_squared(recipient_position, owner_position);
                    let actual_interval = Self::calculate_interval_from_distance_sq(dist_sq);
                    let last_sent = pip_state.last_sent_times.get(&recipient_id).map(|v| *v).unwrap_or(0);
                    let elapsed = (now_ms - last_sent).max(0);
                    if elapsed >= i64::from(actual_interval) {
                        NetworkServer::try_send(recipient, &writer, BasisNetworkCommons::CAMERA_PIP_POSITION_CHANNEL, DeliveryMethod::Sequenced);
                        pip_state.last_sent_times.insert(recipient_id, now_ms);
                    }
                }
            }
            NetworkServer::return_writer(writer);
        }
    }

    /// Send all active PIP camera states to a newly joined peer.
    pub fn send_pip_state_to_peer(new_peer: &NetPeerRef) {
        let mut writer = NetworkServer::rent_writer();
        for entry in PIP_STATES.iter() {
            let pose = entry.value().pose.lock().clone();
            if !pose.is_active {
                continue;
            }
            writer.reset();
            let mut msg = CameraPIPStateMessage {
                player_id: *entry.key() as u16,
                is_active: true,
                position_x: pose.position_x,
                position_y: pose.position_y,
                position_z: pose.position_z,
                rotation_x: pose.rotation_x,
                rotation_y: pose.rotation_y,
                rotation_z: pose.rotation_z,
                rotation_w: pose.rotation_w,
            };
            if msg.serialize(&mut writer).is_ok() {
                NetworkServer::try_send(new_peer, &writer, BasisNetworkCommons::CAMERA_PIP_STATE_CHANNEL, DeliveryMethod::ReliableOrdered);
            }
        }
        NetworkServer::return_writer(writer);
    }

    /// On disconnect: if this player had an active PIP, broadcast destroy to all.
    pub fn remove_player(peer_id: i32) {
        if let Some((_, state)) = PIP_STATES.remove(&peer_id)
            && state.pose.lock().is_active
        {
            let mut destroy_msg = CameraPIPStateMessage { player_id: peer_id as u16, is_active: false, ..Default::default() };
            let mut writer = NetworkServer::rent_writer();
            if destroy_msg.serialize(&mut writer).is_ok() {
                NetworkServer::broadcast_message_to_clients(
                    &writer,
                    BasisNetworkCommons::CAMERA_PIP_STATE_CHANNEL,
                    &NetworkServer::peer_snapshot(),
                    DeliveryMethod::ReliableOrdered,
                );
            }
            NetworkServer::return_writer(writer);
            BNL::log(format!("PIP camera auto-destroyed for disconnected player {peer_id}"));
        }
        for entry in PIP_STATES.iter() {
            entry.value().last_sent_times.remove(&peer_id);
        }
    }

    pub fn reset() {
        PIP_STATES.clear();
    }

    fn distance_squared(a: Vector3, b: Vector3) -> f32 {
        let dx = a.x - b.x;
        let dy = a.y - b.y;
        let dz = a.z - b.z;
        dx * dx + dy * dy + dz * dz
    }

    fn calculate_interval_from_distance_sq(distance_sq: f32) -> i32 {
        let default_interval = BasisServerReductionSystemEvents::bsrs_millisecond_default_interval();
        let raw_interval = (default_interval as f32
            * (BasisServerReductionSystemEvents::bsr_base_multiplier() + distance_sq * BasisServerReductionSystemEvents::bsrs_increase_rate()))
            as i32;
        let encoded_interval = raw_interval - default_interval;
        let offset_byte = encoded_interval.clamp(0, i32::from(u8::MAX));
        offset_byte + default_interval
    }
}
