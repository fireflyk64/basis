//! Port of `Handlers/BasisNetworkHandleTempBlock.cs`: routes session-scoped "temp block"
//! notifications to the one peer they target.

use basis_network_core::{BasisNetworkCommons, DeliveryMethod, NetPacketReader, NetPeerRef};

use crate::NetworkServer;

pub struct BasisNetworkHandleTempBlock;

impl BasisNetworkHandleTempBlock {
    /// Wire (in): `[byte eventType][ushort targetID][bool isBlocked]`; (out to target):
    /// `[byte eventType][ushort senderID][bool isBlocked]`.
    pub fn handle_event(mut reader: NetPacketReader, peer: &NetPeerRef, event_type: u8) {
        let (Ok(target_id), Ok(is_blocked)) = (reader.get_ushort(), reader.get_bool()) else {
            return;
        };
        let Some(target_peer) = NetworkServer::authenticated_peers().get(&i32::from(target_id)).map(|p| p.value().clone()) else {
            return;
        };
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(event_type);
        writer.put_ushort(peer.id() as u16);
        writer.put_bool(is_blocked);
        NetworkServer::try_send(&target_peer, &writer, BasisNetworkCommons::EVENTS_CHANNEL, DeliveryMethod::ReliableOrdered);
        NetworkServer::return_writer(writer);
    }
}
