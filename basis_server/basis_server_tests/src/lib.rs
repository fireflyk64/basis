//! Port of `BasisServerTests`. The tests live under `tests/` in the C# folder layout; this crate
//! holds what they share: a real server fixture and the wait helpers.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]
#![deny(unused_must_use)]

pub mod support;
