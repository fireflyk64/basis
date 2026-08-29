//! Port of `Security/BasisHeadlessAudioStateManager.cs`.

use std::sync::atomic::{AtomicBool, Ordering};

use basis_network_core::SerializableBasis::AdminRequestMode;
use basis_network_core::NetPeerRef;

use super::{broadcast_admin_state, send_admin_state_to_peer};

/// Runtime-only server toggle for headless audio clip playback.
pub struct BasisHeadlessAudioStateManager;

static HEADLESS_AUDIO_OFF: AtomicBool = AtomicBool::new(false);

impl BasisHeadlessAudioStateManager {
    pub fn headless_audio_off() -> bool {
        HEADLESS_AUDIO_OFF.load(Ordering::Acquire)
    }

    pub fn set_headless_audio(headless_audio_off: bool) -> bool {
        HEADLESS_AUDIO_OFF.swap(headless_audio_off, Ordering::AcqRel) != headless_audio_off
    }

    pub fn send_state_to_peer(peer: &NetPeerRef) {
        send_admin_state_to_peer(peer, AdminRequestMode::GlobalGetHeadlessAudioState, |writer| {
            writer.put_bool(Self::headless_audio_off());
            Ok(())
        });
    }

    pub fn broadcast_state() {
        broadcast_admin_state(AdminRequestMode::GlobalGetHeadlessAudioState, |writer| {
            writer.put_bool(Self::headless_audio_off());
            Ok(())
        });
    }
}
