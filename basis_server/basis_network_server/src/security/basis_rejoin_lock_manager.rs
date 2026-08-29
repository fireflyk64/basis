//! Port of `Security/BasisRejoinLockManager.cs`.

use std::sync::LazyLock;

use basis_network_core::BNL;
use dashmap::DashMap;

use crate::NetworkServer;

/// Runtime-only "rejoin-only" lockdown: while the restriction mode is `RejoinOnly`, only the
/// UUIDs captured at the moment the mode was enabled may (re)connect — nobody new. Not persisted;
/// a restart normalizes the mode back to Normal and the set starts empty.
pub struct BasisRejoinLockManager;

static ALLOWED: LazyLock<DashMap<String, ()>> = LazyLock::new(DashMap::new);

impl BasisRejoinLockManager {
    pub fn count() -> usize {
        ALLOWED.len()
    }

    /// Snapshot every currently-authenticated peer's UUID as the allowed set.
    pub fn capture_current_population() {
        ALLOWED.clear();
        let Some(identity) = NetworkServer::auth_identity() else {
            return;
        };
        for peer in NetworkServer::authenticated_peers().iter() {
            if let Some(uuid) = identity.net_id_to_uuid(peer.value())
                && !uuid.is_empty()
            {
                ALLOWED.insert(uuid, ());
            }
        }
        BNL::log(format!("Rejoin-only lockdown enabled — captured {} current player(s).", ALLOWED.len()));
    }

    /// Drop the captured set (mode changed away from RejoinOnly).
    pub fn clear() {
        ALLOWED.clear();
    }

    /// True if this UUID was connected when the lockdown was enabled.
    pub fn is_allowed(uuid: &str) -> bool {
        !uuid.is_empty() && ALLOWED.contains_key(uuid)
    }
}
