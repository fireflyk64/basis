//! Port of `BasisNetworkCore`: everything shared between the Basis server and its clients —
//! the wire protocol, message serialization, avatar compression, configuration, and the
//! transport abstraction (implemented over iroh in [`transport`]).
//!
//! Module names follow the C# folder and file names; type names follow the C# type names.

pub mod basis_simd_capabilities;
pub mod compression;
pub mod compute;
pub mod configuration;
pub mod diagnostics;
pub mod encryption;
pub mod identity;
pub mod io;
pub mod mathematics;
pub mod p2p;
pub mod pooling;
pub mod protocol;
pub mod sanitization;
pub mod serializable;
pub mod statistics;
pub mod transport;

pub use basis_simd_capabilities::BasisSimdCapabilities;
pub use diagnostics::BNL;
pub use io::{NetDataError, NetDataReader, NetDataWriter, NetPacketReader, NetResult};
pub use protocol::basis_cpu_budget::{BasisCoreLease, BasisCpuBudget};
pub use protocol::basis_network_commons::BasisNetworkCommons;
pub use protocol::basis_network_version::BasisNetworkVersion;
pub use protocol::basis_packet_util::BasisPacketUtil;
/// The C# `SerializableBasis` partial class, as a module alias: `SerializableBasis::PlayerIdMessage`.
#[allow(non_snake_case)]
pub use serializable as SerializableBasis;
pub use transport::basis_network_shell::{
    ConnectionRequest, DeliveryMethod, DisconnectInfo, DisconnectReason, EventBasedNetListener,
    NetManager, NetPeer, NetPeerRef, NetStatistics,
};
