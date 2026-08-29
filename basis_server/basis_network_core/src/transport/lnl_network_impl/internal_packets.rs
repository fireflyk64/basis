//! Port of `LiteNetLib/InternalPackets.cs`: the connect request and connect accept packets,
//! byte for byte.

use super::net_constants::NetConstants;
use super::net_packet::{NetPacket, PacketProperty};

/// `[property][protocol id:4][connect time:8][peer id:4][address size:1][address][payload]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetConnectRequestPacket {
    pub connection_time: i64,
    pub connection_number: u8,
    pub target_address: Vec<u8>,
    /// The connect payload the application supplied (version, auth bytes, ready message).
    pub data: Vec<u8>,
    /// The id the connecting side knows itself by.
    pub peer_id: i32,
}

impl NetConnectRequestPacket {
    pub const HEADER_SIZE: usize = 18;

    pub fn get_protocol_id(packet: &NetPacket) -> i32 {
        packet.read_i32(1)
    }

    pub fn from_data(packet: &NetPacket) -> Option<Self> {
        if packet.connection_number() >= NetConstants::MAX_CONNECTION_NUMBER {
            return None;
        }
        let connection_time = packet.read_i64(5);
        let peer_id = packet.read_i32(13);
        let addr_size = usize::from(*packet.raw().get(Self::HEADER_SIZE - 1)?);
        if addr_size != 16 && addr_size != 28 {
            return None;
        }
        let target_address = packet.raw().get(Self::HEADER_SIZE..Self::HEADER_SIZE + addr_size)?.to_vec();
        let data = packet.raw().get(Self::HEADER_SIZE + addr_size..).unwrap_or(&[]).to_vec();
        Some(Self { connection_time, connection_number: packet.connection_number(), target_address, data, peer_id })
    }

    pub fn make(connect_data: &[u8], address_bytes: &[u8], connect_time: i64, local_id: i32) -> NetPacket {
        let mut packet = NetPacket::with_property(PacketProperty::ConnectRequest, connect_data.len() + address_bytes.len());
        packet.write_i32(1, NetConstants::PROTOCOL_ID);
        packet.write_i64(5, connect_time);
        packet.write_i32(13, local_id);
        let raw = packet.raw_mut();
        raw[Self::HEADER_SIZE - 1] = u8::try_from(address_bytes.len()).unwrap_or(0);
        raw[Self::HEADER_SIZE..Self::HEADER_SIZE + address_bytes.len()].copy_from_slice(address_bytes);
        raw[Self::HEADER_SIZE + address_bytes.len()..].copy_from_slice(connect_data);
        packet
    }
}

/// `[property][connect time:8][connection number:1][reused:1][peer id:4]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetConnectAcceptPacket {
    pub connection_time: i64,
    pub connection_number: u8,
    /// The id the accepting side assigned — what the client reports as `RemoteId`.
    pub peer_id: i32,
    pub peer_network_changed: bool,
}

impl NetConnectAcceptPacket {
    pub const SIZE: usize = 15;

    pub fn from_data(packet: &NetPacket) -> Option<Self> {
        if packet.size() != Self::SIZE {
            return None;
        }
        let connection_time = packet.read_i64(1);
        let connection_number = packet.raw()[9];
        if connection_number >= NetConstants::MAX_CONNECTION_NUMBER {
            return None;
        }
        let is_reused = packet.raw()[10];
        if is_reused > 1 {
            return None;
        }
        let peer_id = packet.read_i32(11);
        if peer_id < 0 {
            return None;
        }
        Some(Self { connection_time, connection_number, peer_id, peer_network_changed: is_reused == 1 })
    }

    pub fn make(connect_time: i64, connect_num: u8, local_peer_id: i32) -> NetPacket {
        let mut packet = NetPacket::with_property(PacketProperty::ConnectAccept, 0);
        packet.write_i64(1, connect_time);
        packet.raw_mut()[9] = connect_num;
        packet.write_i32(11, local_peer_id);
        packet
    }

    /// The reply to a `PeerNotFound` that tells the other side who we were, so it can re-key
    /// the connection to our new address.
    pub fn make_network_changed(connect_time: i64, connect_num: u8, remote_id: i32) -> NetPacket {
        let mut packet = NetPacket::with_property(PacketProperty::PeerNotFound, Self::SIZE - 1);
        packet.write_i64(1, connect_time);
        packet.raw_mut()[9] = connect_num;
        packet.raw_mut()[10] = 1;
        packet.write_i32(11, remote_id);
        packet
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_request_round_trips_and_matches_the_csharp_layout() {
        let address = vec![2, 0, 0x10, 0xC8, 127, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        let packet = NetConnectRequestPacket::make(b"hello", &address, 0x0102030405060708, 7);
        assert_eq!(packet.size(), 18 + 16 + 5);
        assert_eq!(packet.property(), Some(PacketProperty::ConnectRequest));
        assert_eq!(packet.read_i32(1), 14);
        assert_eq!(packet.raw()[17], 16);
        let parsed = NetConnectRequestPacket::from_data(&packet).unwrap();
        assert_eq!(parsed.connection_time, 0x0102030405060708);
        assert_eq!(parsed.peer_id, 7);
        assert_eq!(parsed.target_address, address);
        assert_eq!(parsed.data, b"hello");
        assert_eq!(parsed.connection_number, 0);
    }

    #[test]
    fn connect_request_refuses_bad_address_sizes_and_connection_numbers() {
        let mut packet = NetConnectRequestPacket::make(&[], &[0; 16], 1, 1);
        packet.raw_mut()[17] = 17;
        assert!(NetConnectRequestPacket::from_data(&packet).is_none());
        let mut packet = NetConnectRequestPacket::make(&[], &[0; 16], 1, 1);
        packet.raw_mut()[0] |= 3 << 5; // connection number 3 is the last valid one
        assert!(NetConnectRequestPacket::from_data(&packet).is_some());
        // An empty payload parses as empty data.
        assert!(NetConnectRequestPacket::from_data(&NetConnectRequestPacket::make(&[], &[0; 28], 1, 1)).unwrap().data.is_empty());
    }

    #[test]
    fn connect_accept_round_trips() {
        let packet = NetConnectAcceptPacket::make(99, 2, 41);
        assert_eq!(packet.size(), 15);
        assert_eq!(packet.raw()[9], 2);
        assert_eq!(packet.raw()[10], 0);
        let parsed = NetConnectAcceptPacket::from_data(&packet).unwrap();
        assert_eq!((parsed.connection_time, parsed.connection_number, parsed.peer_id, parsed.peer_network_changed), (99, 2, 41, false));
        let changed = NetConnectAcceptPacket::make_network_changed(99, 2, 41);
        assert_eq!(changed.property(), Some(PacketProperty::PeerNotFound));
        assert_eq!(changed.size(), 15);
        assert!(NetConnectAcceptPacket::from_data(&changed).unwrap().peer_network_changed);
    }

    #[test]
    fn connect_accept_refuses_malformed_packets() {
        let mut short = NetConnectAcceptPacket::make(1, 0, 1);
        short.truncate(14);
        assert!(NetConnectAcceptPacket::from_data(&short).is_none());
        let mut bad_num = NetConnectAcceptPacket::make(1, 0, 1);
        bad_num.raw_mut()[9] = 4;
        assert!(NetConnectAcceptPacket::from_data(&bad_num).is_none());
        let mut bad_reused = NetConnectAcceptPacket::make(1, 0, 1);
        bad_reused.raw_mut()[10] = 2;
        assert!(NetConnectAcceptPacket::from_data(&bad_reused).is_none());
        let negative = NetConnectAcceptPacket::make(1, 0, -1);
        assert!(NetConnectAcceptPacket::from_data(&negative).is_none());
    }
}
