//! Port of `Security/BasisUserOpusBitrateStateManager.cs`.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicI32, Ordering};

use basis_network_core::SerializableBasis::AdminRequestMode;
use basis_network_core::NetPeerRef;
use dashmap::DashMap;

use super::{broadcast_admin_state, send_admin_state_to_peer};
use crate::NetworkServer;

/// Per-user admin override for the Opus encoder bitrate (bits per second). 0 = "no override".
pub struct BasisUserOpusBitrateStateManager;

static OVERRIDES: LazyLock<DashMap<i32, i32>> = LazyLock::new(DashMap::new);
/// Server-wide bitrate applied to every client without a per-user override. 0 = none.
static GLOBAL_BITRATE: AtomicI32 = AtomicI32::new(0);

impl BasisUserOpusBitrateStateManager {
    // Opus accepts 500..512000 bps; clamp to a slightly tighter, conservative range for voice.
    pub const MIN_BITRATE: i32 = 6000;
    pub const MAX_BITRATE: i32 = 510000;

    pub fn try_get_bitrate(net_id: i32) -> Option<i32> {
        OVERRIDES.get(&net_id).map(|v| *v)
    }

    /// Set or clear the bitrate override for a user. Pass 0 to clear. Returns the value that
    /// was actually stored after clamping (0 = cleared).
    pub fn set_bitrate(net_id: i32, bitrate: i32) -> i32 {
        if bitrate <= 0 {
            OVERRIDES.remove(&net_id);
            return 0;
        }
        let bitrate = bitrate.clamp(Self::MIN_BITRATE, Self::MAX_BITRATE);
        OVERRIDES.insert(net_id, bitrate);
        bitrate
    }

    pub fn clear_for_peer(net_id: i32) {
        OVERRIDES.remove(&net_id);
    }

    pub fn global_bitrate() -> i32 {
        GLOBAL_BITRATE.load(Ordering::Acquire)
    }

    /// Set or clear the global bitrate. Pass 0 to clear. Returns the stored value after clamping.
    pub fn set_global_bitrate(bitrate: i32) -> i32 {
        let bitrate = if bitrate <= 0 { 0 } else { bitrate.clamp(Self::MIN_BITRATE, Self::MAX_BITRATE) };
        GLOBAL_BITRATE.store(bitrate, Ordering::Release);
        bitrate
    }

    /// Per-user override wins over the global value; 0 = client default.
    pub fn effective_bitrate_for(net_id: i32) -> i32 {
        Self::try_get_bitrate(net_id).unwrap_or_else(Self::global_bitrate)
    }

    pub fn send_state_to_peer(peer: &NetPeerRef) {
        Self::send_override_to_peer(peer, Self::effective_bitrate_for(peer.id()));
    }

    pub fn push_effective_to_all_peers() {
        for peer in NetworkServer::peer_snapshot().iter() {
            Self::send_override_to_peer(peer, Self::effective_bitrate_for(peer.id()));
        }
    }

    pub fn send_global_state_to_peer(peer: &NetPeerRef) {
        send_admin_state_to_peer(peer, AdminRequestMode::GlobalGetOpusBitrateState, |writer| {
            writer.put_int(Self::global_bitrate());
            Ok(())
        });
    }

    pub fn broadcast_global_state() {
        broadcast_admin_state(AdminRequestMode::GlobalGetOpusBitrateState, |writer| {
            writer.put_int(Self::global_bitrate());
            Ok(())
        });
    }

    pub fn send_override_to_peer(peer: &NetPeerRef, bitrate: i32) {
        send_admin_state_to_peer(peer, AdminRequestMode::UserOpusBitrateOverride, |writer| {
            writer.put_int(bitrate);
            Ok(())
        });
    }
}
