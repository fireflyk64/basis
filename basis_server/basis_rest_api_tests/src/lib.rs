//! Port of `BasisRestApi.Tests`: the REST API and health check exercised over real HTTP on a
//! loopback port. The tests live under `tests/`; this crate holds what they share.

// Test support: a fixture that cannot be set up fails the test that asked for it, so panics are
// the right tool here and the production lint policy does not apply.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable)]
#![deny(unused_must_use)]

pub mod support;
