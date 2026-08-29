//! Port of `BasisNetworkCore/Transport`: the transport abstraction the server and clients are
//! written against, the stack registry that picks an implementation, and the iroh
//! implementation ([`iroh_network_impl`]) that replaces `LNLNetworkImpl`.
//!
//! The shape is deliberately the C# one — `EventBasedNetListener` events, `ConnectionRequest`,
//! `NetPeer`, `NetManager` — which is what lets three stacks sit behind it: iroh
//! ([`iroh_network_impl`]), the LiteNetLib protocol the existing C# clients speak
//! ([`lnl_network_impl`]), and the mixed stack that runs both at once ([`mixed_network_impl`]).

pub mod basis_network_shell;
pub mod basis_network_stack_registry;
pub mod connection_target;
pub mod iroh_connection_target_parser;
pub mod iroh_network_impl;
pub mod lnl_connection_target_parser;
pub mod lnl_network_impl;
pub mod mixed_network_impl;

pub use basis_network_shell::*;
pub use basis_network_stack_registry::{BasisNetworkStackRegistry, PeerIntroducerFactory, ServerProbeResult, StackInfo, StackProbe};
pub use connection_target::{ConnectionTarget, ConnectionTargetKeys, IConnectionTargetParser};
pub use iroh_connection_target_parser::IrohConnectionTargetParser;
pub use iroh_network_impl::{IrohNetManager, IrohNetPeer, IrohRuntime};
pub use lnl_connection_target_parser::LNLConnectionTargetParser;
pub use lnl_network_impl::{LnlNetManager, LnlNetPeer};
pub use mixed_network_impl::{MixedConnectionTargetParser, MixedNetManager};
