//! Port of `Security/BasisResourceLimitManager.cs`.

use std::sync::atomic::{AtomicI32, Ordering};

use basis_network_core::SerializableBasis::AdminRequestMode;
use basis_network_core::configuration::Configuration;
use basis_network_core::NetPeerRef;

use super::{broadcast_admin_state, send_admin_state_to_peer};

/// Server-defined caps that bound per-client resource use (content-share spheres).
pub struct BasisResourceLimitManager;

static MAX_CONTENT_SPHERES_PER_PLAYER: AtomicI32 = AtomicI32::new(BasisResourceLimitManager::DEFAULT_MAX_CONTENT_SPHERES_PER_PLAYER);

impl BasisResourceLimitManager {
    const DEFAULT_MAX_CONTENT_SPHERES_PER_PLAYER: i32 = 32;
    const ABSOLUTE_MAX_CONTENT_SPHERES_PER_PLAYER: i32 = 4096;

    pub fn max_content_spheres_per_player() -> i32 {
        MAX_CONTENT_SPHERES_PER_PLAYER.load(Ordering::Acquire)
    }

    pub fn initialize_from_config(config: &Configuration) {
        Self::set_limits(config.max_content_spheres_per_player);
    }

    pub fn set_limits(max_content_spheres_per_player: i32) -> bool {
        let spheres = Self::sanitize(max_content_spheres_per_player);
        MAX_CONTENT_SPHERES_PER_PLAYER.swap(spheres, Ordering::AcqRel) != spheres
    }

    pub fn send_state_to_peer(peer: &NetPeerRef) {
        send_admin_state_to_peer(peer, AdminRequestMode::GlobalGetResourceLimits, |writer| {
            writer.put_int(Self::max_content_spheres_per_player());
            Ok(())
        });
    }

    pub fn broadcast_state() {
        broadcast_admin_state(AdminRequestMode::GlobalGetResourceLimits, |writer| {
            writer.put_int(Self::max_content_spheres_per_player());
            Ok(())
        });
    }

    fn sanitize(spheres: i32) -> i32 {
        if spheres < 1 {
            return Self::DEFAULT_MAX_CONTENT_SPHERES_PER_PLAYER;
        }
        spheres.min(Self::ABSOLUTE_MAX_CONTENT_SPHERES_PER_PLAYER)
    }
}
