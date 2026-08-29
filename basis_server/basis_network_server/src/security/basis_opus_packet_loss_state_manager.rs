//! Port of `Security/BasisOpusPacketLossStateManager.cs`.

use std::sync::atomic::{AtomicI32, Ordering};

use basis_network_core::SerializableBasis::AdminRequestMode;
use basis_network_core::NetPeerRef;

use super::{broadcast_admin_state, send_admin_state_to_peer};

/// Runtime-only server toggle for the Opus encoder's in-band FEC aggressiveness (0..100 %).
pub struct BasisOpusPacketLossStateManager;

static PACKET_LOSS_PERCENT: AtomicI32 = AtomicI32::new(10);

impl BasisOpusPacketLossStateManager {
    pub fn packet_loss_percent() -> i32 {
        PACKET_LOSS_PERCENT.load(Ordering::Acquire)
    }

    /// Clamp, set, and report whether the value actually changed.
    pub fn set_packet_loss_percent(percent: i32) -> bool {
        let percent = percent.clamp(0, 100);
        PACKET_LOSS_PERCENT.swap(percent, Ordering::AcqRel) != percent
    }

    fn percent_byte() -> u8 {
        u8::try_from(Self::packet_loss_percent()).unwrap_or(0)
    }

    pub fn send_state_to_peer(peer: &NetPeerRef) {
        send_admin_state_to_peer(peer, AdminRequestMode::GlobalGetOpusPacketLossState, |writer| {
            writer.put_byte(Self::percent_byte());
            Ok(())
        });
    }

    pub fn broadcast_state() {
        broadcast_admin_state(AdminRequestMode::GlobalGetOpusPacketLossState, |writer| {
            writer.put_byte(Self::percent_byte());
            Ok(())
        });
    }
}
