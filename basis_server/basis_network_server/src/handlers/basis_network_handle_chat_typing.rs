//! Port of `Handlers/BasisNetworkHandleChatTyping.cs`: relays transient chat typing state.

use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetPacketReader, NetPeerRef};

use crate::NetworkServer;
use crate::networking::BasisNetworkChat;

pub struct BasisNetworkHandleChatTyping;

impl BasisNetworkHandleChatTyping {
    /// Wire (in): `[byte eventType][bool isTyping]`; (out): `[byte eventType][ushort senderId][bool isTyping]`.
    pub fn handle_event(mut reader: NetPacketReader, peer: &NetPeerRef, event_type: u8) {
        let Ok(is_typing) = reader.get_bool() else {
            return;
        };
        if BasisNetworkChat::is_chat_blocked_for(peer) {
            return;
        }
        let Ok(sender_id) = u16::try_from(peer.id()) else {
            BNL::log_error(format!("Cannot broadcast chat typing state for peer id {}: outside ushort wire format.", peer.id()));
            return;
        };
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(event_type);
        writer.put_ushort(sender_id);
        writer.put_bool(is_typing);
        NetworkServer::broadcast_message_to_clients_excluding(
            &writer,
            BasisNetworkCommons::EVENTS_CHANNEL,
            peer,
            &NetworkServer::peer_snapshot(),
            DeliveryMethod::Sequenced,
        );
        NetworkServer::return_writer(writer);
    }
}
