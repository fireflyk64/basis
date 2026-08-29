//! Port of `Handlers/BasisNetworkHandleVoiceRecord.cs`: relays voice-record consent messages.
//!
//! A recorder asks a recordee for permission to capture their voice; the recordee replies with a
//! consent decision. The server does not evaluate consent — it only rewrites the payload with the
//! sender's peer id and forwards it to the single target peer.

use basis_network_core::{BasisNetworkCommons, DeliveryMethod, NetPacketReader, NetPeerRef};

use crate::NetworkServer;

pub struct BasisNetworkHandleVoiceRecord;

impl BasisNetworkHandleVoiceRecord {
    /// Wire (in): `[byte eventType][ushort targetID]{[byte state] when consent}[byte purpose]`;
    /// (out): `[byte eventType][ushort senderID]{[byte state] when consent}[byte purpose]`.
    pub fn handle_event(mut reader: NetPacketReader, peer: &NetPeerRef, event_type: u8) {
        let has_state = event_type == BasisNetworkCommons::EVENT_TYPE_VOICE_RECORD_CONSENT;
        let needed = if has_state { 4 } else { 3 }; // targetID(2) [+ state(1)] + purpose(1)
        if reader.available_bytes() < needed {
            return;
        }
        let Ok(target_id) = reader.get_ushort() else {
            return;
        };
        let state = if has_state { reader.get_byte().unwrap_or(0) } else { 0 };
        let Ok(purpose) = reader.get_byte() else {
            return;
        };
        let Some(target_peer) = NetworkServer::authenticated_peers().get(&i32::from(target_id)).map(|p| p.value().clone()) else {
            return;
        };
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(event_type);
        writer.put_ushort(peer.id() as u16);
        if has_state {
            writer.put_byte(state);
        }
        writer.put_byte(purpose);
        NetworkServer::try_send(&target_peer, &writer, BasisNetworkCommons::EVENTS_CHANNEL, DeliveryMethod::ReliableOrdered);
        NetworkServer::return_writer(writer);
    }
}
