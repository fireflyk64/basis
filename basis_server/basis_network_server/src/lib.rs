//! Port of `BasisNetworkServer`: the Basis relay server.
//!
//! Module names follow the C# folder names (`Core`, `Auth`, `Security`, `Reduction`, ...) and
//! type names follow the C# type names, so a reader of the C# server finds `NetworkServer`,
//! `BasisServerHandleEvents`, `BasisPlayerModeration` and the rest where they expect them.
//!
//! The C# server is a set of static classes; each is a `pub struct X;` with associated functions
//! over module-level statics here. Everything that can fail returns a `Result` — a malformed
//! packet is a [`NetDataError`](basis_network_core::NetDataError) counted against the peer by
//! the message processor exactly as the C# `catch` did, and everything else is a
//! [`basis_error::BasisError`].
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

pub mod auth;
pub mod core;
pub mod diagnostics;
pub mod handlers;
pub mod identity;
pub mod messaging;
pub mod networking;
pub mod p2p;
pub mod reduction;
pub mod resources;
pub mod rest_api;
pub mod security;
pub mod util;

pub use core::network_server::NetworkServer;
pub use core::basis_server_handle_events::BasisServerHandleEvents;
pub use core::basis_server_control::{BasisServerControl, IServerControl, LoadStrategy, PlayerInfo, SwitchWorldParams, WorldInfo, WorldLoadParams};
