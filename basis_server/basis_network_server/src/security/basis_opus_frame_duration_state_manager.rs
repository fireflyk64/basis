//! Port of `Security/BasisOpusFrameDurationStateManager.cs`.

use std::sync::atomic::{AtomicI32, Ordering};

use basis_network_core::SerializableBasis::AdminRequestMode;
use basis_network_core::NetPeerRef;

use super::{broadcast_admin_state, send_admin_state_to_peer};

/// Runtime-only server toggle for the Opus frame duration: 20 or 40 ms only.
pub struct BasisOpusFrameDurationStateManager;

static FRAME_DURATION_MS: AtomicI32 = AtomicI32::new(BasisOpusFrameDurationStateManager::DEFAULT_MS);

impl BasisOpusFrameDurationStateManager {
    pub const DEFAULT_MS: i32 = 20;

    pub fn frame_duration_ms() -> i32 {
        FRAME_DURATION_MS.load(Ordering::Acquire)
    }

    pub fn is_accepted_duration(ms: i32) -> bool {
        ms == 20 || ms == 40
    }

    /// Set the frame duration (20 or 40 ms only). Returns true if it changed.
    pub fn set_frame_duration_ms(ms: i32) -> bool {
        let ms = if Self::is_accepted_duration(ms) { ms } else { Self::DEFAULT_MS };
        FRAME_DURATION_MS.swap(ms, Ordering::AcqRel) != ms
    }

    fn duration_byte() -> u8 {
        u8::try_from(Self::frame_duration_ms()).unwrap_or(Self::DEFAULT_MS as u8)
    }

    pub fn send_state_to_peer(peer: &NetPeerRef) {
        send_admin_state_to_peer(peer, AdminRequestMode::GlobalGetOpusFrameDurationState, |writer| {
            writer.put_byte(Self::duration_byte());
            Ok(())
        });
    }

    pub fn broadcast_state() {
        broadcast_admin_state(AdminRequestMode::GlobalGetOpusFrameDurationState, |writer| {
            writer.put_byte(Self::duration_byte());
            Ok(())
        });
    }
}
