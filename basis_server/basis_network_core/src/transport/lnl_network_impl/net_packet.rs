//! Port of `LiteNetLib/NetPacket.cs`: one datagram (or one entry of a merged datagram) and the
//! header fields packed into its first bytes.
//!
//! Byte 0 is `[fragmented:1][connection number:2][property:5]`; what follows depends on the
//! property. The layout is the wire contract with every existing C# client, so the accessors
//! below are the only place it is spelled out.

use super::net_constants::NetConstants;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum PacketProperty {
    Unreliable = 0,
    Channeled = 1,
    Ack = 2,
    Ping = 3,
    Pong = 4,
    ConnectRequest = 5,
    ConnectAccept = 6,
    Disconnect = 7,
    UnconnectedMessage = 8,
    MtuCheck = 9,
    MtuOk = 10,
    Broadcast = 11,
    Merged = 12,
    ShutdownOk = 13,
    PeerNotFound = 14,
    InvalidProtocol = 15,
    NatMessage = 16,
    Empty = 17,
    CompactMerged = 18,
}

impl PacketProperty {
    pub const COUNT: u8 = 19;

    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::Unreliable,
            1 => Self::Channeled,
            2 => Self::Ack,
            3 => Self::Ping,
            4 => Self::Pong,
            5 => Self::ConnectRequest,
            6 => Self::ConnectAccept,
            7 => Self::Disconnect,
            8 => Self::UnconnectedMessage,
            9 => Self::MtuCheck,
            10 => Self::MtuOk,
            11 => Self::Broadcast,
            12 => Self::Merged,
            13 => Self::ShutdownOk,
            14 => Self::PeerNotFound,
            15 => Self::InvalidProtocol,
            16 => Self::NatMessage,
            17 => Self::Empty,
            18 => Self::CompactMerged,
            _ => return None,
        })
    }

    /// Bytes before the payload for this property.
    pub fn header_size(self) -> usize {
        match self {
            Self::Unreliable => NetConstants::UNRELIABLE_HEADER_SIZE,
            Self::Channeled | Self::Ack => NetConstants::CHANNELED_HEADER_SIZE,
            Self::Ping => NetConstants::HEADER_SIZE + 2,
            Self::ConnectRequest => super::internal_packets::NetConnectRequestPacket::HEADER_SIZE,
            Self::ConnectAccept => super::internal_packets::NetConnectAcceptPacket::SIZE,
            Self::Disconnect => NetConstants::HEADER_SIZE + 8,
            Self::Pong => NetConstants::HEADER_SIZE + 10,
            _ => NetConstants::HEADER_SIZE,
        }
    }
}

/// A packet is its bytes; `size()` is what LiteNetLib called `Size`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetPacket {
    data: Vec<u8>,
}

impl NetPacket {
    const PROPERTY_MASK: u8 = 0x1F;
    const CONNECTION_MASK: u8 = 0x60;
    const FRAGMENTED_BIT: u8 = 0x80;

    pub fn with_size(size: usize) -> Self {
        Self { data: vec![0; size] }
    }

    /// A packet of `property` with room for `payload_size` bytes after the header.
    pub fn with_property(property: PacketProperty, payload_size: usize) -> Self {
        let mut packet = Self::with_size(payload_size + property.header_size());
        packet.set_property(property);
        packet
    }

    pub fn from_bytes(data: Vec<u8>) -> Self {
        Self { data }
    }

    pub fn size(&self) -> usize {
        self.data.len()
    }

    pub fn raw(&self) -> &[u8] {
        &self.data
    }

    pub fn raw_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }

    /// Shrinks the packet to `size` bytes (never grows it).
    pub fn truncate(&mut self, size: usize) {
        self.data.truncate(size);
    }

    pub fn property_byte(&self) -> u8 {
        self.data.first().copied().unwrap_or(0) & Self::PROPERTY_MASK
    }

    pub fn property(&self) -> Option<PacketProperty> {
        PacketProperty::from_byte(self.property_byte())
    }

    pub fn set_property(&mut self, property: PacketProperty) {
        if let Some(b) = self.data.first_mut() {
            *b = (*b & !Self::PROPERTY_MASK) | property as u8;
        }
    }

    pub fn connection_number(&self) -> u8 {
        (self.data.first().copied().unwrap_or(0) & Self::CONNECTION_MASK) >> 5
    }

    pub fn set_connection_number(&mut self, value: u8) {
        if let Some(b) = self.data.first_mut() {
            *b = (*b & !Self::CONNECTION_MASK) | ((value & 0x3) << 5);
        }
    }

    pub fn is_fragmented(&self) -> bool {
        self.data.first().copied().unwrap_or(0) & Self::FRAGMENTED_BIT != 0
    }

    pub fn mark_fragmented(&mut self) {
        if let Some(b) = self.data.first_mut() {
            *b |= Self::FRAGMENTED_BIT;
        }
    }

    pub fn sequence(&self) -> u16 {
        self.read_u16(1)
    }

    pub fn set_sequence(&mut self, value: u16) {
        self.write_u16(1, value);
    }

    pub fn channel_id(&self) -> u8 {
        self.data.get(3).copied().unwrap_or(0)
    }

    pub fn set_channel_id(&mut self, value: u8) {
        if let Some(b) = self.data.get_mut(3) {
            *b = value;
        }
    }

    pub fn fragment_id(&self) -> u16 {
        self.read_u16(4)
    }

    pub fn set_fragment_id(&mut self, value: u16) {
        self.write_u16(4, value);
    }

    pub fn fragment_part(&self) -> u16 {
        self.read_u16(6)
    }

    pub fn set_fragment_part(&mut self, value: u16) {
        self.write_u16(6, value);
    }

    pub fn fragments_total(&self) -> u16 {
        self.read_u16(8)
    }

    pub fn set_fragments_total(&mut self, value: u16) {
        self.write_u16(8, value);
    }

    pub fn header_size(&self) -> usize {
        self.property().map(PacketProperty::header_size).unwrap_or(NetConstants::HEADER_SIZE)
    }

    /// Whether the bytes are at least a complete header for the property they claim.
    pub fn verify(&self) -> bool {
        let Some(property) = self.property() else {
            return false;
        };
        let header_size = property.header_size();
        let size = self.size();
        size >= header_size && (!self.is_fragmented() || size >= header_size + NetConstants::FRAGMENT_HEADER_SIZE)
    }

    pub fn read_u16(&self, at: usize) -> u16 {
        self.data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).unwrap_or(0)
    }

    pub fn write_u16(&mut self, at: usize, value: u16) {
        if let Some(b) = self.data.get_mut(at..at + 2) {
            b.copy_from_slice(&value.to_le_bytes());
        }
    }

    pub fn read_i32(&self, at: usize) -> i32 {
        self.data.get(at..at + 4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).unwrap_or(0)
    }

    pub fn write_i32(&mut self, at: usize, value: i32) {
        if let Some(b) = self.data.get_mut(at..at + 4) {
            b.copy_from_slice(&value.to_le_bytes());
        }
    }

    pub fn read_i64(&self, at: usize) -> i64 {
        self.data
            .get(at..at + 8)
            .map(|b| i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
            .unwrap_or(0)
    }

    pub fn write_i64(&mut self, at: usize, value: i64) {
        if let Some(b) = self.data.get_mut(at..at + 8) {
            b.copy_from_slice(&value.to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_sizes_match_litenetlib() {
        assert_eq!(PacketProperty::Unreliable.header_size(), 2);
        assert_eq!(PacketProperty::Channeled.header_size(), 4);
        assert_eq!(PacketProperty::Ack.header_size(), 4);
        assert_eq!(PacketProperty::Ping.header_size(), 3);
        assert_eq!(PacketProperty::Pong.header_size(), 11);
        assert_eq!(PacketProperty::ConnectRequest.header_size(), 18);
        assert_eq!(PacketProperty::ConnectAccept.header_size(), 15);
        assert_eq!(PacketProperty::Disconnect.header_size(), 9);
        assert_eq!(PacketProperty::Merged.header_size(), 1);
        assert_eq!(PacketProperty::CompactMerged.header_size(), 1);
    }

    #[test]
    fn wire_values_are_stable() {
        // CompactMerged was appended, so nothing that already shipped moved.
        assert_eq!(PacketProperty::Unreliable as u8, 0);
        assert_eq!(PacketProperty::Merged as u8, 12);
        assert_eq!(PacketProperty::CompactMerged as u8, 18);
        assert_eq!(PacketProperty::from_byte(19), None);
        for b in 0..PacketProperty::COUNT {
            assert_eq!(PacketProperty::from_byte(b).map(|p| p as u8), Some(b));
        }
    }

    #[test]
    fn first_byte_packs_property_connection_and_fragment_flag() {
        let mut p = NetPacket::with_property(PacketProperty::Channeled, 3);
        assert_eq!(p.size(), 7);
        p.set_connection_number(3);
        p.mark_fragmented();
        assert_eq!(p.raw()[0], 0x80 | (3 << 5) | 1);
        assert_eq!(p.property(), Some(PacketProperty::Channeled));
        assert_eq!(p.connection_number(), 3);
        assert!(p.is_fragmented());
        p.set_property(PacketProperty::Ack);
        assert_eq!(p.raw()[0], 0x80 | (3 << 5) | 2);
        p.set_sequence(0x1234);
        p.set_channel_id(9);
        assert_eq!(&p.raw()[1..4], &[0x34, 0x12, 9]);
        assert_eq!(p.sequence(), 0x1234);
    }

    #[test]
    fn verify_needs_a_whole_header() {
        assert!(!NetPacket::from_bytes(vec![PacketProperty::Channeled as u8, 0, 0]).verify());
        assert!(NetPacket::from_bytes(vec![PacketProperty::Channeled as u8, 0, 0, 0]).verify());
        assert!(!NetPacket::from_bytes(vec![0x80 | PacketProperty::Channeled as u8, 0, 0, 0, 0, 0]).verify());
        assert!(NetPacket::from_bytes(vec![0x80 | PacketProperty::Channeled as u8; 10]).verify());
        assert!(!NetPacket::from_bytes(vec![31]).verify());
        assert!(NetPacket::from_bytes(vec![PacketProperty::Ping as u8, 1, 0]).verify());
    }
}
