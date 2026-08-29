//! Port of `Security/BasisAvatarScaleLimitManager.cs`.

use std::sync::atomic::{AtomicU32, Ordering};

use basis_network_core::SerializableBasis::AdminRequestMode;
use basis_network_core::configuration::Configuration;
use basis_network_core::NetPeerRef;

use super::{broadcast_admin_state, send_admin_state_to_peer};

/// Server-defined minimum/maximum avatar eye height (in metres) a non-admin player may scale to.
pub struct BasisAvatarScaleLimitManager;

static MIN_METERS: AtomicU32 = AtomicU32::new(BasisAvatarScaleLimitManager::DEFAULT_MIN_METERS.to_bits());
static MAX_METERS: AtomicU32 = AtomicU32::new(BasisAvatarScaleLimitManager::DEFAULT_MAX_METERS.to_bits());

impl BasisAvatarScaleLimitManager {
    const DEFAULT_MIN_METERS: f32 = 0.1;
    const DEFAULT_MAX_METERS: f32 = 100.0;
    const ABSOLUTE_FLOOR: f32 = 0.01;
    const ABSOLUTE_CEILING: f32 = 1000.0;

    pub fn min_meters() -> f32 {
        f32::from_bits(MIN_METERS.load(Ordering::Acquire))
    }

    pub fn max_meters() -> f32 {
        f32::from_bits(MAX_METERS.load(Ordering::Acquire))
    }

    pub fn initialize_from_config(config: &Configuration) {
        Self::set_limits(config.min_avatar_eye_height_meters, config.max_avatar_eye_height_meters);
    }

    /// Sanitize, order (min <= max), set, and report whether either bound actually changed.
    pub fn set_limits(min_meters: f32, max_meters: f32) -> bool {
        let (min_meters, max_meters) = Self::sanitize(min_meters, max_meters);
        let prev_min = f32::from_bits(MIN_METERS.swap(min_meters.to_bits(), Ordering::AcqRel));
        let prev_max = f32::from_bits(MAX_METERS.swap(max_meters.to_bits(), Ordering::AcqRel));
        prev_min != min_meters || prev_max != max_meters
    }

    pub fn send_state_to_peer(peer: &NetPeerRef) {
        send_admin_state_to_peer(peer, AdminRequestMode::GlobalGetAvatarScaleLimits, |writer| {
            writer.put_float(Self::min_meters());
            writer.put_float(Self::max_meters());
            Ok(())
        });
    }

    pub fn broadcast_state() {
        broadcast_admin_state(AdminRequestMode::GlobalGetAvatarScaleLimits, |writer| {
            writer.put_float(Self::min_meters());
            writer.put_float(Self::max_meters());
            Ok(())
        });
    }

    fn sanitize(mut min_meters: f32, mut max_meters: f32) -> (f32, f32) {
        if !min_meters.is_finite() || min_meters <= 0.0 {
            min_meters = Self::DEFAULT_MIN_METERS;
        }
        if !max_meters.is_finite() || max_meters <= 0.0 {
            max_meters = Self::DEFAULT_MAX_METERS;
        }
        if min_meters < Self::ABSOLUTE_FLOOR {
            min_meters = Self::ABSOLUTE_FLOOR;
        }
        if max_meters > Self::ABSOLUTE_CEILING {
            max_meters = Self::ABSOLUTE_CEILING;
        }
        if max_meters < min_meters {
            max_meters = min_meters;
        }
        (min_meters, max_meters)
    }
}
