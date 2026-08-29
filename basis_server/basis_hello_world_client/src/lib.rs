//! Port of `BasisHelloWorldClient`: the smallest client that can hold a conversation on a Basis
//! server, and the variant that can also talk to another player directly with the server acting
//! only as an introducer.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]
#![deny(unused_must_use)]

pub mod basis_hello_client;
pub mod hello_peer_client;

pub use basis_hello_client::{BasisHelloClient, HelloExtension, HelloTransport, NumberHandler, TextHandler};
pub use hello_peer_client::HelloPeerClient;
