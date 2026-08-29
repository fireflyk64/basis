//! Port of `BasisRestApi.Tests`: the REST API and health check exercised over real HTTP on a
//! loopback port. The tests live under `tests/`; this crate holds what they share.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]
#![deny(unused_must_use)]

pub mod support;
