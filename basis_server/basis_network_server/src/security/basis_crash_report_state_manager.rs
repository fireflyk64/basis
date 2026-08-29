//! Port of `Security/BasisCrashReportStateManager.cs`.

use std::sync::atomic::{AtomicBool, Ordering};

use basis_network_core::SerializableBasis::AdminRequestMode;
use basis_network_core::configuration::Configuration;
use basis_network_core::NetPeerRef;

use super::{broadcast_admin_state, send_admin_state_to_peer};

/// Server toggle for client error/exception reporting.
pub struct BasisCrashReportStateManager;

static ENABLED: AtomicBool = AtomicBool::new(true);

impl BasisCrashReportStateManager {
    pub fn enabled() -> bool {
        ENABLED.load(Ordering::Acquire)
    }

    pub fn initialize_from_config(config: &Configuration) {
        ENABLED.store(config.crash_reporting_enabled, Ordering::Release);
    }

    pub fn set_enabled(enabled: bool) -> bool {
        ENABLED.swap(enabled, Ordering::AcqRel) != enabled
    }

    pub fn send_state_to_peer(peer: &NetPeerRef) {
        send_admin_state_to_peer(peer, AdminRequestMode::GlobalGetCrashReportState, |writer| {
            writer.put_bool(Self::enabled());
            Ok(())
        });
    }

    pub fn broadcast_state() {
        broadcast_admin_state(AdminRequestMode::GlobalGetCrashReportState, |writer| {
            writer.put_bool(Self::enabled());
            Ok(())
        });
    }
}
