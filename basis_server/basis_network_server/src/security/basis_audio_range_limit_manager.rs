//! Port of `Security/BasisAudioRangeLimitManager.cs`.

use std::sync::atomic::{AtomicU32, Ordering};

use basis_network_core::SerializableBasis::AdminRequestMode;
use basis_network_core::configuration::Configuration;
use basis_network_core::NetPeerRef;

use super::{broadcast_admin_state, send_admin_state_to_peer};

/// Server-defined ceilings (in metres) for how far clients may set their microphone (voice
/// transmit) and hearing (audio receive) range. Seeded from the configuration at boot and
/// pushed to clients so they clamp their sliders and effective range to it.
pub struct BasisAudioRangeLimitManager;

static MAX_MICROPHONE_RANGE_METERS: AtomicU32 = AtomicU32::new(BasisAudioRangeLimitManager::DEFAULT_METERS.to_bits());
static MAX_HEARING_RANGE_METERS: AtomicU32 = AtomicU32::new(BasisAudioRangeLimitManager::DEFAULT_METERS.to_bits());

impl BasisAudioRangeLimitManager {
    const DEFAULT_METERS: f32 = 25.0;

    pub fn max_microphone_range_meters() -> f32 {
        f32::from_bits(MAX_MICROPHONE_RANGE_METERS.load(Ordering::Acquire))
    }

    pub fn max_hearing_range_meters() -> f32 {
        f32::from_bits(MAX_HEARING_RANGE_METERS.load(Ordering::Acquire))
    }

    pub fn initialize_from_config(config: &Configuration) {
        MAX_MICROPHONE_RANGE_METERS.store(Self::sanitize(config.max_microphone_range_meters).to_bits(), Ordering::Release);
        MAX_HEARING_RANGE_METERS.store(Self::sanitize(config.max_hearing_range_meters).to_bits(), Ordering::Release);
    }

    /// Clamp, set, and report whether either value actually changed.
    pub fn set_limits(microphone_meters: f32, hearing_meters: f32) -> bool {
        let mic = Self::sanitize(microphone_meters);
        let hearing = Self::sanitize(hearing_meters);
        let prev_mic = f32::from_bits(MAX_MICROPHONE_RANGE_METERS.swap(mic.to_bits(), Ordering::AcqRel));
        let prev_hearing = f32::from_bits(MAX_HEARING_RANGE_METERS.swap(hearing.to_bits(), Ordering::AcqRel));
        prev_mic != mic || prev_hearing != hearing
    }

    pub fn send_state_to_peer(peer: &NetPeerRef) {
        send_admin_state_to_peer(peer, AdminRequestMode::GlobalGetAudioRangeLimits, |writer| {
            writer.put_float(Self::max_microphone_range_meters());
            writer.put_float(Self::max_hearing_range_meters());
            Ok(())
        });
    }

    pub fn broadcast_state() {
        broadcast_admin_state(AdminRequestMode::GlobalGetAudioRangeLimits, |writer| {
            writer.put_float(Self::max_microphone_range_meters());
            writer.put_float(Self::max_hearing_range_meters());
            Ok(())
        });
    }

    fn sanitize(meters: f32) -> f32 {
        if meters.is_nan() || meters <= 0.0 { Self::DEFAULT_METERS } else { meters }
    }
}
