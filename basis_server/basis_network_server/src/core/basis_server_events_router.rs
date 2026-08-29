//! Port of `Core/BasisServerEventsRouter.cs`: dispatches EventsChannel packets by their leading
//! event-type byte.

use basis_network_core::SerializableBasis::{CameraCountdownMessage, CameraShutterSoundMessage, ClientCameraCountdownMessage};
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetPacketReader, NetPeerRef};

use crate::NetworkServer;
use crate::handlers::{
    BasisNetworkHandleChatTyping, BasisNetworkHandleErrorReport, BasisNetworkHandleJiggleGrab, BasisNetworkHandleTempBlock,
    BasisNetworkHandleVoiceRecord,
};

pub struct BasisServerEventsRouter;

impl BasisServerEventsRouter {
    pub fn handle_event(mut reader: NetPacketReader, peer: &NetPeerRef) {
        let Ok(event_type) = reader.get_byte() else {
            return;
        };
        match event_type {
            BasisNetworkCommons::EVENT_TYPE_CAMERA_SHUTTER_SOUND => Self::handle_camera_shutter_sound(peer, event_type),
            BasisNetworkCommons::EVENT_TYPE_CAMERA_COUNTDOWN => Self::handle_camera_countdown(reader, peer, event_type),
            BasisNetworkCommons::EVENT_TYPE_PLAYER_TEMP_BLOCK => BasisNetworkHandleTempBlock::handle_event(reader, peer, event_type),
            BasisNetworkCommons::EVENT_TYPE_AVATAR_RATE_CHANGE => Self::handle_avatar_rate_change(reader, peer, event_type),
            BasisNetworkCommons::EVENT_TYPE_PLAYER_CHAT_TYPING => BasisNetworkHandleChatTyping::handle_event(reader, peer, event_type),
            BasisNetworkCommons::EVENT_TYPE_TALK_MODE_CHANGED => Self::handle_talk_mode_changed(reader, peer, event_type),
            BasisNetworkCommons::EVENT_TYPE_MUTE_STATE_CHANGED => Self::handle_mute_state_changed(reader, peer, event_type),
            BasisNetworkCommons::EVENT_TYPE_ERROR_REPORT => BasisNetworkHandleErrorReport::handle_event(reader, peer, event_type),
            BasisNetworkCommons::EVENT_TYPE_VOICE_RECORD_REQUEST | BasisNetworkCommons::EVENT_TYPE_VOICE_RECORD_CONSENT => {
                BasisNetworkHandleVoiceRecord::handle_event(reader, peer, event_type)
            }
            BasisNetworkCommons::EVENT_TYPE_JIGGLE_GRAB => BasisNetworkHandleJiggleGrab::handle_event(reader, peer, event_type),
            _ => BNL::log_error(format!("Unknown EventsChannel event type: {event_type}")),
        }
    }

    fn broadcast_excluding(writer: &basis_network_core::NetDataWriter, peer: &NetPeerRef, delivery: DeliveryMethod) {
        NetworkServer::broadcast_message_to_clients_excluding(writer, BasisNetworkCommons::EVENTS_CHANNEL, peer, &NetworkServer::peer_snapshot(), delivery);
    }

    fn handle_camera_shutter_sound(peer: &NetPeerRef, event_type: u8) {
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(event_type);
        let mut out_msg = CameraShutterSoundMessage { player_id: peer.id() as u16 };
        if out_msg.serialize(&mut writer).is_ok() {
            Self::broadcast_excluding(&writer, peer, DeliveryMethod::Sequenced);
        }
        NetworkServer::return_writer(writer);
    }

    /// Wire (in): `[eventType:1][intervalMs:2]`; (out): `[eventType:1][senderId:2][intervalMs:2]`.
    fn handle_avatar_rate_change(mut reader: NetPacketReader, peer: &NetPeerRef, event_type: u8) {
        let Ok(interval_ms) = reader.get_ushort() else {
            return;
        };
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(event_type);
        writer.put_ushort(peer.id() as u16);
        writer.put_ushort(interval_ms);
        Self::broadcast_excluding(&writer, peer, DeliveryMethod::ReliableOrdered);
        NetworkServer::return_writer(writer);
    }

    /// Wire (in): `[eventType:1][modeByte:1]`; (out): `[eventType:1][senderId:2][modeByte:1]`.
    fn handle_talk_mode_changed(mut reader: NetPacketReader, peer: &NetPeerRef, event_type: u8) {
        let Ok(mode) = reader.get_byte() else {
            return;
        };
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(event_type);
        writer.put_ushort(peer.id() as u16);
        writer.put_byte(mode);
        Self::broadcast_excluding(&writer, peer, DeliveryMethod::ReliableOrdered);
        NetworkServer::return_writer(writer);
    }

    /// Wire (in): `[eventType:1][muted:1]`; (out): `[eventType:1][senderId:2][muted:1]`.
    fn handle_mute_state_changed(mut reader: NetPacketReader, peer: &NetPeerRef, event_type: u8) {
        let Ok(muted) = reader.get_byte() else {
            return;
        };
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(event_type);
        writer.put_ushort(peer.id() as u16);
        writer.put_byte(muted);
        Self::broadcast_excluding(&writer, peer, DeliveryMethod::ReliableOrdered);
        NetworkServer::return_writer(writer);
    }

    fn handle_camera_countdown(mut reader: NetPacketReader, peer: &NetPeerRef, event_type: u8) {
        let mut client_msg = ClientCameraCountdownMessage::default();
        if client_msg.deserialize(&mut reader).is_err() {
            return;
        }
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(event_type);
        let mut out_msg = CameraCountdownMessage { player_id: peer.id() as u16, seconds: client_msg.seconds };
        if out_msg.serialize(&mut writer).is_ok() {
            Self::broadcast_excluding(&writer, peer, DeliveryMethod::Sequenced);
        }
        NetworkServer::return_writer(writer);
    }
}
