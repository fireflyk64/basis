//! Port of `Reduction/BasisComputeBackend.cs`: finds the optional compute backend and hands back
//! a solver, or nothing.
//!
//! The C# resolved `BasisNetworkCompute.dll` by name at runtime so the Unity build never saw
//! ILGPU. The Rust workspace links `basis_network_compute` directly; every failure inside it is
//! ordinary rather than exceptional — no device, a kernel that will not build — and all of them
//! mean the same thing to the caller: the sweep stays on the CPU.

use basis_network_core::compute::IBasisDistanceSolver;
use parking_lot::RwLock;

static STATUS: RwLock<Option<String>> = RwLock::new(None);

pub struct BasisComputeBackend;

impl BasisComputeBackend {
    /// What was tried and what came back, for the boot log.
    pub fn status() -> String {
        STATUS.read().clone().unwrap_or_else(|| "not attempted".to_string())
    }

    pub fn try_load_distance_solver(base_interval_ms: i32, device_selector: &str) -> Option<Box<dyn IBasisDistanceSolver>> {
        match basis_network_compute::BasisComputeFactory::try_create_distance_solver(base_interval_ms, device_selector) {
            Ok(solver) => {
                *STATUS.write() = Some(format!("{} ({})", solver.backend(), solver.device_name()));
                Some(solver)
            }
            Err(failure) => {
                *STATUS.write() = Some(failure);
                None
            }
        }
    }

    /// The devices an operator may choose between, one per line, or None when the backend has
    /// nothing to offer.
    pub fn describe_devices() -> Option<String> {
        basis_network_compute::BasisComputeFactory::describe_devices()
    }
}
