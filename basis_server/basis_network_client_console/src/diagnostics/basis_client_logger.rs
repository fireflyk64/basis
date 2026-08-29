//! Port of `BasisClientLogger.cs`: routes transport warnings and errors to BNL.

use basis_network_core::BNL;
use basis_network_core::transport::basis_network_shell::{INetLogger, NetLogLevel};

pub struct BasisClientLogger;

impl INetLogger for BasisClientLogger {
    fn write_net(&self, level: NetLogLevel, message: &str) {
        match level {
            NetLogLevel::Warning => BNL::log_warning(message),
            NetLogLevel::Error => BNL::log_error(message),
            _ => {}
        }
    }
}
