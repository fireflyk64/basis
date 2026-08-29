//! Port of `Diagnostics/BasisStatistics.cs`. The C# worker was commented out; the API is kept so
//! callers read the same, and the poll is a no-op.

use basis_network_core::transport::basis_network_shell::NetManagerRef;
use parking_lot::Mutex;

static MANAGER: Mutex<Option<NetManagerRef>> = Mutex::new(None);

pub struct BasisStatistics;

impl BasisStatistics {
    pub fn manager() -> Option<NetManagerRef> {
        MANAGER.lock().clone()
    }

    pub fn start_worker_thread(manager: NetManagerRef) {
        *MANAGER.lock() = Some(manager);
    }

    pub fn stop_worker_thread() {
        *MANAGER.lock() = None;
    }

    pub fn poll_latest_statistics() {}
}
