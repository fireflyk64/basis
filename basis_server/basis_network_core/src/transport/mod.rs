//! Port of `BasisNetworkCore/Transport`: the transport abstraction the server and clients are
//! written against, the stack registry that picks an implementation, and the iroh
//! implementation ([`iroh_network_impl`]) that replaces `LNLNetworkImpl`.
//!
//! The shape is deliberately the C# one — `EventBasedNetListener` events, `ConnectionRequest`,
//! `NetPeer`, `NetManager` — so a LiteNetLib-protocol implementation with the same surface can be
//! registered later without the server noticing.

pub mod basis_network_shell;
pub mod basis_network_stack_registry;
pub mod connection_target;
pub mod iroh_connection_target_parser;
pub mod iroh_network_impl;
pub mod lnl_connection_target_parser;

pub use basis_network_shell::*;
pub use basis_network_stack_registry::{BasisNetworkStackRegistry, PeerIntroducerFactory, ServerProbeResult, StackInfo, StackProbe};
pub use connection_target::{ConnectionTarget, ConnectionTargetKeys, IConnectionTargetParser};
pub use iroh_connection_target_parser::IrohConnectionTargetParser;
pub use iroh_network_impl::{IrohNetManager, IrohNetPeer, IrohRuntime};
pub use lnl_connection_target_parser::LNLConnectionTargetParser;
