//! Port of `Contrib/Handles/Common`: handles are human-readable names (a DNS name, a local
//! nickname) that can be verified to point at a machine-readable identity.
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

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

pub mod newtypes {
    // TODO: Unify with core basis' notion of identity and also DID's identity type
    /// `Identity` is a string that represents the player's machine-readable account identifier.
    #[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
    pub struct Identity(pub String);

    impl Identity {
        pub fn new(v: impl Into<String>) -> Self {
            Self(v.into())
        }
        pub fn v(&self) -> &str {
            &self.0
        }
    }
}

pub use newtypes::Identity;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Resolves whether a handle points to a given identity.
pub trait IHandleVerifier: Send + Sync {
    /// For documentation on this function, see `HandleVerifier`.
    fn handle_points_to_identity<'a>(
        &'a self,
        handle: &'a dyn IHandle,
        identity: &'a Identity,
    ) -> BoxFuture<'a, bool>;

    /// The particular kind of handle
    fn kind(&self) -> HandleKind;

    fn properties(&self) -> HandleProperties;
}

/// All handle types implement `IHandle`
pub trait IHandle: Send + Sync {
    /// Which type of handle?
    fn kind(&self) -> HandleKind;

    fn properties(&self) -> HandleProperties;

    /// Gets the display name to show.
    fn display_name(&self) -> &str;
}

/// Information inherent to a particular `HandleKind` kind/type of handle.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct HandleProperties {
    pub kind: HandleKind,
    pub mutability: HandleMutability,
    pub is_globally_unique: bool,
}

/// The degree to which the set of identities that a handle points to can be changed.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HandleMutability {
    /// Handles always point to the same set of identities.
    Immutable,
    /// Once an identity is added to the set it always remains, but new identities can also be added.
    AppendOnly,
    /// Identities can be added and deleted from the set at will.
    Mutable,
}

/// The different supported handle kinds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum HandleKind {
    Local,
    Dns,
    // TODO: HttpWellKnown
    // TODO: Steam
}

/// Configuration for [`HandleVerifier`]. Be sure to populate the `verifiers`.
#[derive(Default)]
pub struct Config {
    pub verifiers: HashMap<HandleKind, Box<dyn IHandleVerifier>>,
}

/// Resolves whether a handle points to a given identity.
pub struct HandleVerifier {
    verifiers: HashMap<HandleKind, Box<dyn IHandleVerifier>>,
}

impl HandleVerifier {
    pub fn new(cfg: Config) -> Self {
        Self { verifiers: cfg.verifiers }
    }

    /// Checks if the given `Handle` "points to" the given `Identity`.
    ///
    /// SECURITY NOTE:
    ///
    /// All of the following must be true to consider a handle associated with a given peer:
    /// * `handle_points_to_identity(handle, identity)` returns `true`
    /// * `identity` points to `handle` through some other mechanism (for example, a peer is
    ///   authenticated on `identity` and has requested `handle` to be displayed to other players).
    ///
    /// If you only establish that `handle` -> `identity` without also ensuring that
    /// `identity` -> `handle`, then its possible for Bob to point `bob.com` to Alice's handle,
    /// and make Alice appear as `bob.com` without Alice's consent. Likewise if you *only*
    /// establish that `identity` -> `handle`, then bob could point their identity to `alice.com`
    /// and masquerade/spoof themselves as alice. This is why its *very* important to establish
    /// a bidirectional mapping: `handle` <-> `identity`.
    pub async fn handle_points_to_identity(&self, handle: &dyn IHandle, identity: &Identity) -> bool {
        let Some(verifier) = self.verifiers.get(&handle.kind()) else {
            return false;
        };
        verifier.handle_points_to_identity(handle, identity).await
    }
}
