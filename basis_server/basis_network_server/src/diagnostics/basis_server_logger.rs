//! Port of `Diagnostics/BasisServerLogger.cs`: routes transport log lines into BNL.

use basis_network_core::BNL;
use basis_network_core::transport::basis_network_shell::{INetLogger, NetLogLevel};

pub struct BasisServerLogger;

impl INetLogger for BasisServerLogger {
    fn write_net(&self, level: NetLogLevel, message: &str) {
        match level {
            NetLogLevel::Warning => BNL::log_warning(message),
            NetLogLevel::Error => BNL::log_error(message),
            // Trace and Info are deliberately not forwarded, as in the C#.
            _ => {}
        }
    }
}
