//! Port of `Networking/BasisAvatarRequestMessages.cs`: the clone request/response handlers,
//! which today only consume their payload.

use basis_network_core::{NetPacketReader, NetPeerRef};

pub struct BasisAvatarRequestMessages;

impl BasisAvatarRequestMessages {
    pub fn avatar_clone_request_message(mut reader: NetPacketReader, _peer: &NetPeerRef) {
        let _remote_player_id = reader.get_ushort();
    }

    pub fn avatar_clone_response_message(mut reader: NetPacketReader, _peer: &NetPeerRef) {
        let _end_user = reader.get_ushort();
        let _approval_id = reader.get_string();
    }
}
