//! The LiteNetLib-protocol implementation of the transport abstraction: what the existing C#
//! clients — Unity and headless alike — connect to, byte for byte the protocol of the
//! `LiteNetLib` project the C# server links.
//!
//! One file per C# file, so the two trees read side by side:
//!
//! | C#                     | here                        |
//! |------------------------|-----------------------------|
//! | `NetConstants.cs`      | [`net_constants`]           |
//! | `NetPacket.cs`         | [`net_packet`]              |
//! | `InternalPackets.cs`   | [`internal_packets`]        |
//! | `CompactMerge.cs`      | [`compact_merge`]           |
//! | `ReliableChannel.cs`   | [`reliable_channel`]        |
//! | `SequencedChannel.cs`  | [`sequenced_channel`]       |
//! | `NetPeer.cs`           | [`net_peer`]                |
//! | `ConnectionRequest.cs` | [`connection_request`]      |
//! | `NetManager*.cs`       | [`net_manager`]             |
//!
//! What is deliberately not here: the NAT punch module (legacy clients are never offered a
//! direct link — the server relays everything for them), the extra packet layers (CRC/XOR —
//! Basis never enabled one), NTP requests, and the debug latency/loss simulation.

pub mod compact_merge;
pub mod connection_request;
pub mod internal_packets;
pub mod net_constants;
pub mod net_manager;
pub mod net_packet;
pub mod net_peer;
pub mod net_utils;
pub mod reliable_channel;
pub mod sequenced_channel;

pub use compact_merge::{CompactEntry, CompactMerge};
pub use connection_request::LnlConnectionRequest;
pub use net_constants::NetConstants;
pub use net_manager::{LnlNetManager, LnlNetPeer, LnlSettings};
pub use net_packet::{NetPacket, PacketProperty};
pub use net_peer::ConnectionState;
