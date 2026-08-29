//! Port of `BasisNetworkCompute`: the optional distance-sweep backend the reduction system can
//! offload to.
//!
//! The C# backend compiled the sweep to a GPU through ILGPU. This crate offers the same contract
//! ([`IBasisDistanceSolver`]) with a host-vectorised solver: every pair's squared distance is
//! computed eight lanes at a time at the widest level `fearless_simd` finds on the host, and the
//! interval byte and quality tier are produced with the protocol's own scalar routines so the
//! result can never disagree with the CPU sweep it stands in for. A GPU backend can slot in
//! behind the same factory later; the reduction system verifies whichever one it gets against
//! the CPU on its first slice.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]
#![deny(unused_must_use)]

use basis_network_core::BasisNetworkCommons;
use basis_network_core::BasisSimdCapabilities;
use basis_network_core::compute::{BasisDistanceSolveRequest, IBasisDistanceSolver};
use fearless_simd::{Level, Simd, SimdBase, SimdFloat, dispatch, f32x8};

/// The solver backends this build can offer.
pub struct BasisComputeFactory;

impl BasisComputeFactory {
    pub const CPU_SIMD_BACKEND: &'static str = "cpu-simd";

    /// Creates the solver `device_selector` names (an index or a name; empty picks the best).
    /// The failure is a plain message, because every reason is ordinary: the caller logs it and
    /// runs the sweep on the CPU.
    pub fn try_create_distance_solver(base_interval_ms: i32, device_selector: &str) -> Result<Box<dyn IBasisDistanceSolver>, String> {
        if base_interval_ms <= 0 {
            return Err(format!("base interval {base_interval_ms} ms is not positive"));
        }
        let selector = device_selector.trim();
        if !selector.is_empty() && selector != "0" && !Self::CPU_SIMD_BACKEND.eq_ignore_ascii_case(selector) && !"cpu".eq_ignore_ascii_case(selector) {
            return Err(format!("no compute device matches '{selector}'; available: {}", Self::describe_devices().unwrap_or_default().trim()));
        }
        if !BasisSimdCapabilities::hardware_accelerated() {
            return Err("the host has no vector unit the sweep could use".to_string());
        }
        Ok(Box::new(CpuSimdDistanceSolver { device_name: BasisSimdCapabilities::describe() }))
    }

    /// The devices an operator may choose between, one per line.
    pub fn describe_devices() -> Option<String> {
        Some(format!("[0] {} — {}\n", Self::CPU_SIMD_BACKEND, BasisSimdCapabilities::describe()))
    }
}

/// The host-vectorised sweep.
pub struct CpuSimdDistanceSolver {
    device_name: String,
}

impl CpuSimdDistanceSolver {
    #[inline(always)]
    fn solve_impl<S: Simd>(simd: S, request: &BasisDistanceSolveRequest, interval_byte: &mut [u8], quality: &mut [u8]) {
        let p = request.parameters;
        let n = request.player_count.min(request.pos_x.len()).min(request.pos_y.len()).min(request.pos_z.len());
        for (local, i) in (request.slice_start..request.slice_end).enumerate() {
            if i >= n {
                break;
            }
            let base = local * request.player_count;
            let ix = f32x8::splat(simd, request.pos_x[i]);
            let iy = f32x8::splat(simd, request.pos_y[i]);
            let iz = f32x8::splat(simd, request.pos_z[i]);
            let mut j = 0usize;
            while j + 8 <= n {
                let dx = ix - f32x8::from_slice(simd, &request.pos_x[j..j + 8]);
                let dy = iy - f32x8::from_slice(simd, &request.pos_y[j..j + 8]);
                let dz = iz - f32x8::from_slice(simd, &request.pos_z[j..j + 8]);
                let dist_sq: [f32; 8] = (dx * dx + dy * dy + dz * dz).into();
                for (lane, d) in dist_sq.iter().enumerate() {
                    Self::write_pair(&p, *d, base + j + lane, interval_byte, quality);
                }
                j += 8;
            }
            while j < n {
                let dx = request.pos_x[i] - request.pos_x[j];
                let dy = request.pos_y[i] - request.pos_y[j];
                let dz = request.pos_z[i] - request.pos_z[j];
                Self::write_pair(&p, dx * dx + dy * dy + dz * dz, base + j, interval_byte, quality);
                j += 1;
            }
        }
    }

    #[inline(always)]
    fn write_pair(p: &basis_network_core::compute::BasisDistanceSolveParameters, dist_sq: f32, slot: usize, interval_byte: &mut [u8], quality: &mut [u8]) {
        let raw_interval = (p.base_interval_ms as f32 * (p.base_multiplier + dist_sq * p.increase_rate)) as i32;
        let encoded = BasisNetworkCommons::encode_avatar_interval_byte(raw_interval, p.base_interval_ms);
        let tier = if dist_sq <= p.high_distance_sq {
            3
        } else if dist_sq <= p.medium_distance_sq {
            2
        } else if dist_sq <= p.low_distance_sq {
            1
        } else {
            0
        };
        if let (Some(b), Some(q)) = (interval_byte.get_mut(slot), quality.get_mut(slot)) {
            *b = encoded;
            *q = tier;
        }
    }
}

impl IBasisDistanceSolver for CpuSimdDistanceSolver {
    fn backend(&self) -> &str {
        BasisComputeFactory::CPU_SIMD_BACKEND
    }

    fn device_name(&self) -> &str {
        &self.device_name
    }

    fn solve(&self, request: &BasisDistanceSolveRequest, interval_byte: &mut [u8], quality: &mut [u8]) {
        let level = Level::new();
        dispatch!(level, simd => Self::solve_impl(simd, request, interval_byte, quality));
    }
}
