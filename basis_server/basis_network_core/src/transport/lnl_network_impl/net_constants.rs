//! Port of `LiteNetLib/NetConstants.cs`: the numbers a LiteNetLib peer either agrees on or
//! cannot talk at all. Every value here is on the wire or decides what fits on it, so none is
//! configurable.

pub struct NetConstants;

impl NetConstants {
    pub const DEFAULT_WINDOW_SIZE: usize = 128;
    /// 32 MB — needed for 1000+ player servers.
    pub const SOCKET_BUFFER_SIZE: usize = 32 * 1024 * 1024;
    pub const SOCKET_TTL: u32 = 255;

    pub const HEADER_SIZE: usize = 1;
    pub const UNRELIABLE_HEADER_SIZE: usize = 2;
    pub const CHANNELED_HEADER_SIZE: usize = 4;
    pub const FRAGMENT_HEADER_SIZE: usize = 6;
    pub const FRAGMENTED_HEADER_TOTAL_SIZE: usize = Self::CHANNELED_HEADER_SIZE + Self::FRAGMENT_HEADER_SIZE;
    pub const MAX_SEQUENCE: u16 = 32768;
    pub const HALF_MAX_SEQUENCE: u16 = Self::MAX_SEQUENCE / 2;

    /// 14 introduced the CompactMerged framing, which a peer either understands or silently
    /// loses every unreliable message to. Rejecting the connection outright is the loud failure
    /// that replaces a runtime capability exchange.
    pub const PROTOCOL_ID: i32 = 14;
    pub const MAX_UDP_HEADER_SIZE: usize = 68;
    pub const CHANNEL_TYPE_COUNT: usize = 4;
    pub const FRAGMENTED_CHANNELS_COUNT: usize = 2;
    pub const MAX_FRAGMENTS_IN_WINDOW: usize = Self::DEFAULT_WINDOW_SIZE / 2;

    /// The MTU ladder discovery climbs: most-games standard first, Ethernet II last.
    pub const POSSIBLE_MTU: [usize; 6] = [
        1024,                              // most games standard
        1232 - Self::MAX_UDP_HEADER_SIZE,  //
        1460 - Self::MAX_UDP_HEADER_SIZE,  // google cloud
        1472 - Self::MAX_UDP_HEADER_SIZE,  // VPN
        1492 - Self::MAX_UDP_HEADER_SIZE,  // Ethernet with LLC and SNAP, PPPoE (RFC 1042)
        1500 - Self::MAX_UDP_HEADER_SIZE,  // Ethernet II (RFC 1191)
    ];

    /// Max possible single packet size.
    pub const INITIAL_MTU: usize = Self::POSSIBLE_MTU[0];
    pub const MAX_PACKET_SIZE: usize = Self::POSSIBLE_MTU[Self::POSSIBLE_MTU.len() - 1];
    pub const MAX_UNRELIABLE_DATA_SIZE: usize = Self::MAX_PACKET_SIZE - Self::UNRELIABLE_HEADER_SIZE;

    /// Connection numbers cycle 0..4 so a reconnect is told apart from a late packet.
    pub const MAX_CONNECTION_NUMBER: u8 = 4;
}
