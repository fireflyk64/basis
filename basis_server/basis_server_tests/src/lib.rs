//! Port of `BasisServerTests`. The tests live under `tests/` in the C# folder layout; this crate
//! holds what they share: a real server fixture and the wait helpers.

// Test support: a fixture that cannot be set up fails the test that asked for it, so panics are
// the right tool here and the production lint policy does not apply.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unreachable)]
#![deny(unused_must_use)]

pub mod support;
