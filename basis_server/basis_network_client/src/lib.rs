//! Port of `BasisNetworkClient`: the connect handshake and the DID identity a client answers
//! the server's challenge with.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]
#![deny(unused_must_use)]

pub mod basis_did_auth_identity_client;
pub mod basis_did_auth_identity_provider;
pub mod network_client;

pub use basis_did_auth_identity_client::{BasisDIDAuthIdentityClient, ClientIdentity};
pub use basis_did_auth_identity_provider::BasisDIDAuthIdentityProvider;
pub use network_client::NetworkClient;
