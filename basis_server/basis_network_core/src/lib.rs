//! Port of `BasisNetworkCore`: everything shared between the Basis server and its clients —
//! the wire protocol, message serialization, avatar compression, configuration, and the
//! transport abstraction (implemented over iroh in [`transport`]).
//!
//! Module names follow the C# folder and file names; type names follow the C# type names.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unimplemented,
        clippy::todo,
        clippy::unreachable
    )
)]
#![deny(unused_must_use)]
// Index loops and long callback types are kept where they mirror the C# codec layout; both lints
// are stylistic and the ported shape is what the C# developer expects to find.
#![allow(clippy::needless_range_loop, clippy::type_complexity)]

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
pub use serializable as SerializableBasis;
pub use transport::basis_network_shell::{
    ConnectionRequest, DeliveryMethod, DisconnectInfo, DisconnectReason, EventBasedNetListener,
    NetManager, NetPeer, NetPeerRef, NetStatistics,
};
