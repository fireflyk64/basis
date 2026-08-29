//! Port of `Contrib/Auth/Did`: `did:key` resolution and the challenge/response authentication
//! Basis uses to prove a player holds the private key behind their DID.
//!
//! The C# `Result<T, E>` helper (`Result.cs`) has no counterpart: Rust's own `Result` is used.
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

pub mod base64_url_safe;
pub mod did_auth;
pub mod did_document;
pub mod did_key_resolver;
pub mod i_did_method;
pub mod json_web_key;
pub mod newtypes;

pub use base64_url_safe::Base64UrlSafe;
pub use did_auth::{
    Challenge, Config, DidAuthentication, DidFragmentErr, DidResolveErr, DidSignatureErr,
    IDidVerifyErr, Response,
};
pub use did_document::DidDocument;
pub use did_key_resolver::{DidKeyDecodeError, DidKeyDecodeException, DidKeyResolver};
pub use i_did_method::{DidMethodKind, IDidMethod};
pub use json_web_key::JsonWebKey;
pub use newtypes::{Did, DidUrl, DidUrlFragment, Nonce};
