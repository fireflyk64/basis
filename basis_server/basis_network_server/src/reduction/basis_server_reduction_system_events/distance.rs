//! `BasisServerReductionSystemEvents.Distance.cs`: the amortized N² distance sweep that fills
//! each receiver's per-sender interval and quality cache, on the CPU (vectorised) or on the
//! compute backend when one is available and agrees with the CPU.

use std::sync::Arc;

use basis_network_core::compute::{BasisDistanceSolveParameters, BasisDistanceSolveRequest, IBasisDistanceSolver};
use basis_network_core::{BNL, BasisNetworkCommons};
use fearless_simd::{Level, Simd, SimdBase, dispatch, f32x8};

use super::tick::TickState;
use super::{BasisServerReductionSystemEvents, MS_TO_TICK, S};
use crate::reduction::{BasisComputeBackend, PlayerState};

/// The sweep's working set, owned by the tick thread.
#[derive(Default)]
pub struct DistanceSweepState {
    /// Position snapshots in roster order: contiguous arrays for cache-friendly reads.
    pub dense_x: Vec<f32>,
    pub dense_y: Vec<f32>,
    pub dense_z: Vec<f32>,
    pub dense_player_ids: Vec<i32>,
    pub device_interval_byte: Vec<u8>,
    pub device_quality: Vec<u8>,
    /// CachedIntervalTicks for every interval byte, so the device never has to send it.
    pub interval_tick_table: Vec<i32>,
    pub distance_solver: Option<Box<dyn IBasisDistanceSolver>>,
    pub distance_solver_tried: bool,
    pub distance_solver_verified: bool,
    /// Cursor for the amortized sweep: index of the next receiver to refresh.
    pub slice_cursor: usize,
    pub tick_counter: i32,
    /// Roster the in-progress sweep is running against, pinned at its first slice.
    pub sweep_roster: Arc<[(i32, Arc<PlayerState>)]>,
}

impl BasisServerReductionSystemEvents {
    /// The refresh period actually in force this tick: the device period while a backend is
    /// carrying the sweep, the CPU period otherwise.
    fn effective_distance_interval_ticks(distance: &DistanceSweepState) -> i32 {
        if distance.distance_solver.is_some() { Self::compute_distance_update_interval_ticks() } else { Self::distance_update_interval_ticks() }
    }

    pub(super) fn update_distance_cache_slice(tick: &mut TickState) -> bool {
        let active_copy = Self::active_players_snapshot();
        let distance = &mut tick.distance;
        if active_copy.is_empty() {
            distance.slice_cursor = 0;
            distance.sweep_roster = Arc::from(Vec::new());
            return false;
        }
        // Between sweeps the refresh period still applies; the work is just done in chunks.
        if distance.slice_cursor == 0 {
            distance.tick_counter += 1;
            if distance.tick_counter < Self::effective_distance_interval_ticks(distance) {
                return false;
            }
        }
        // Pin the roster for the whole sweep: the position arrays are written in its order on
        // the first slice, so a mid-sweep re-read would pair receivers against the wrong rows.
        if distance.slice_cursor == 0 {
            distance.sweep_roster = active_copy;
        }
        let roster = distance.sweep_roster.clone();
        let player_count = roster.len();
        if player_count == 0 {
            distance.slice_cursor = 0;
            return false;
        }

        // Slice size is bounded BELOW: a slice must carry enough receivers to be worth
        // dispatching in parallel.
        let interval = Self::effective_distance_interval_ticks(distance).max(1) as usize;
        let per_tick = Self::MIN_DISTANCE_SLICE_RECEIVERS.max(player_count.div_ceil(interval));
        if distance.slice_cursor >= player_count {
            distance.slice_cursor = 0;
        }
        let slice_start = distance.slice_cursor;
        let slice_end = (slice_start + per_tick).min(player_count);
        if slice_end >= player_count {
            // Sweep complete — restart the period counter.
            distance.slice_cursor = 0;
            distance.tick_counter = 0;
        } else {
            distance.slice_cursor = slice_end;
        }
        // Positions are re-snapshotted only when starting a fresh sweep, so every receiver in
        // one sweep is measured against the same frame.
        if slice_start == 0 {
            Self::snapshot_positions(distance, &roster);
            Self::ensure_distance_solver(distance);
        }
        if !Self::try_run_distance_slice_on_device(distance, &roster, slice_start, slice_end) {
            Self::run_distance_slice(distance, &roster, slice_start, slice_end);
        }
        true
    }

    pub(super) fn snapshot_positions(distance: &mut DistanceSweepState, roster: &[(i32, Arc<PlayerState>)]) {
        let player_count = roster.len();
        distance.dense_x.resize(player_count, 0.0);
        distance.dense_y.resize(player_count, 0.0);
        distance.dense_z.resize(player_count, 0.0);
        distance.dense_player_ids.resize(player_count, 0);
        for (i, (id, state)) in roster.iter().enumerate() {
            let p = state.position();
            distance.dense_player_ids[i] = *id;
            distance.dense_x[i] = p.x;
            distance.dense_y[i] = p.y;
            distance.dense_z[i] = p.z;
        }
    }

    /// One receiver's row of the sweep: every sender's distance → interval + quality.
    #[inline(always)]
    fn distance_row<Sd: Simd>(simd: Sd, distance: &DistanceSweepState, i: usize, player_count: usize, out: &mut [(i32, u8, u8)]) {
        let base_interval_ms = Self::bsrs_millisecond_default_interval();
        let base_multiplier = Self::bsr_base_multiplier();
        let increase_rate = Self::bsrs_increase_rate();
        let ix = distance.dense_x[i];
        let iy = distance.dense_y[i];
        let iz = distance.dense_z[i];
        let mut index = 0usize;
        // Vector pass: eight senders' squared distances at once; the interval encoding is the
        // protocol's own scalar routine per lane, so it can never disagree with the wire.
        const WIDTH: usize = 8;
        if player_count >= WIDTH {
            let ixv = f32x8::splat(simd, ix);
            let iyv = f32x8::splat(simd, iy);
            let izv = f32x8::splat(simd, iz);
            while index + WIDTH <= player_count {
                let dx = ixv - f32x8::from_slice(simd, &distance.dense_x[index..index + WIDTH]);
                let dy = iyv - f32x8::from_slice(simd, &distance.dense_y[index..index + WIDTH]);
                let dz = izv - f32x8::from_slice(simd, &distance.dense_z[index..index + WIDTH]);
                let dist_sq: [f32; 8] = (dx * dx + dy * dy + dz * dz).into();
                for (lane, d) in dist_sq.iter().enumerate() {
                    let raw_interval = (base_interval_ms as f32 * (base_multiplier + d * increase_rate)) as i32;
                    let interval_byte = BasisNetworkCommons::encode_avatar_interval_byte(raw_interval, base_interval_ms);
                    let actual = BasisNetworkCommons::decode_avatar_interval_ms(interval_byte, base_interval_ms);
                    out[index + lane] = ((f64::from(actual) * MS_TO_TICK) as i32, Self::get_quality_index(*d) as u8, interval_byte);
                }
                index += WIDTH;
            }
        }
        while index < player_count {
            let dx = ix - distance.dense_x[index];
            let dy = iy - distance.dense_y[index];
            let dz = iz - distance.dense_z[index];
            let dist_sq = dx * dx + dy * dy + dz * dz;
            let (interval_byte, actual) = Self::calculate_interval_from_distance_sq(dist_sq);
            out[index] = ((f64::from(actual) * MS_TO_TICK) as i32, Self::get_quality_index(dist_sq) as u8, interval_byte);
            index += 1;
        }
    }

    pub(super) fn run_distance_slice(distance: &DistanceSweepState, roster: &[(i32, Arc<PlayerState>)], slice_start: usize, slice_end: usize) {
        let player_count = roster.len();
        let level = Level::new();
        Self::parallel_for(slice_start, slice_end, |i| {
            let (id, state) = &roster[i];
            let mut row: Vec<(i32, u8, u8)> = vec![(0, 0, 0); player_count];
            dispatch!(level, simd => Self::distance_row(simd, distance, i, player_count, &mut row));
            let mut recv = state.receiver.lock();
            for (index, (interval_ticks, quality, interval_byte)) in row.iter().enumerate() {
                let j_id = distance.dense_player_ids[index];
                if *id == j_id {
                    continue;
                }
                let j_index = j_id as usize;
                PlayerState::ensure_tracking(&mut recv, j_index);
                let t = &mut recv.peer_tracking[j_index];
                t.cached_interval_ticks = *interval_ticks;
                t.cached_quality_index = *quality;
                t.cached_interval_byte = *interval_byte;
            }
        });
    }

    /// Resolves the compute backend once, on the first sweep that has a roster to measure.
    fn ensure_distance_solver(distance: &mut DistanceSweepState) {
        if distance.distance_solver_tried {
            return;
        }
        distance.distance_solver_tried = true;
        let base = Self::bsrs_millisecond_default_interval();
        distance.interval_tick_table = (0..256u32).map(|b| (f64::from(BasisNetworkCommons::decode_avatar_interval_ms(b as u8, base)) * MS_TO_TICK) as i32).collect();

        if !Self::enable_compute_offload() {
            *S.distance_backend.write() = "cpu (offload disabled)".to_string();
            return;
        }
        match BasisComputeBackend::try_load_distance_solver(base, &Self::compute_device()) {
            None => {
                *S.distance_backend.write() = "cpu".to_string();
                BNL::log(format!("[BSR] Distance sweep on the CPU - {}.", BasisComputeBackend::status()));
            }
            Some(solver) => {
                *S.distance_backend.write() = solver.backend().to_string();
                BNL::log(format!(
                    "[BSR] Distance sweep offloaded to {}. Refresh period {} -> {} ticks while it holds. It is checked against the CPU on its first slice and dropped if it disagrees.",
                    BasisComputeBackend::status(),
                    Self::distance_update_interval_ticks(),
                    Self::compute_distance_update_interval_ticks()
                ));
                if let Some(devices) = BasisComputeBackend::describe_devices()
                    && devices.contains("[1]")
                {
                    BNL::log(format!("[BSR] This host has more than one compute device. Set ComputeDevice in config.xml to an index or a name to choose:\n{}", devices.trim_end()));
                }
                distance.distance_solver = Some(solver);
            }
        }
    }

    fn current_solve_parameters() -> BasisDistanceSolveParameters {
        BasisDistanceSolveParameters {
            high_distance_sq: Self::high_distance_sq(),
            medium_distance_sq: Self::medium_distance_sq(),
            low_distance_sq: Self::low_distance_sq(),
            base_multiplier: Self::bsr_base_multiplier(),
            increase_rate: Self::bsrs_increase_rate(),
            base_interval_ms: Self::bsrs_millisecond_default_interval(),
        }
    }

    /// Runs one slice on the device and writes the result into the same cache the CPU sweep
    /// writes. False when the device could not be used, so the caller runs the CPU sweep.
    fn try_run_distance_slice_on_device(distance: &mut DistanceSweepState, roster: &[(i32, Arc<PlayerState>)], slice_start: usize, slice_end: usize) -> bool {
        if distance.distance_solver.is_none() || slice_end <= slice_start {
            return false;
        }
        let player_count = roster.len();
        let result_length = (slice_end - slice_start) * player_count;
        if distance.device_interval_byte.len() < result_length {
            distance.device_interval_byte.resize(result_length, 0);
            distance.device_quality.resize(result_length, 0);
        }
        let request = BasisDistanceSolveRequest {
            pos_x: distance.dense_x[..player_count].to_vec(),
            pos_y: distance.dense_y[..player_count].to_vec(),
            pos_z: distance.dense_z[..player_count].to_vec(),
            player_count,
            slice_start,
            slice_end,
            parameters: Self::current_solve_parameters(),
        };
        let solved = {
            let DistanceSweepState { distance_solver, device_interval_byte, device_quality, .. } = distance;
            let Some(solver) = distance_solver.as_ref() else {
                return false;
            };
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| solver.solve(&request, device_interval_byte, device_quality)))
        };
        if solved.is_err() {
            BNL::log_warning("[BSR] The compute backend failed mid-sweep. Dropping back to the CPU sweep for the rest of this process.");
            Self::disable_distance_solver(distance);
            return false;
        }
        if !distance.distance_solver_verified && !Self::verify_device_against_cpu(distance, player_count, slice_start, slice_end) {
            return false;
        }
        Self::scatter_device_results(distance, roster, slice_start, slice_end);
        true
    }

    /// Checks the device against the CPU on the first slice it produces, and refuses it if they
    /// disagree on a quality tier. The interval byte may differ by one step (fused multiply-add
    /// rounding).
    fn verify_device_against_cpu(distance: &mut DistanceSweepState, player_count: usize, slice_start: usize, slice_end: usize) -> bool {
        let mut checked_pairs = 0;
        let mut interval_drift = 0;
        let receiver_step = ((slice_end - slice_start) / 32).max(1);
        let sender_step = (player_count / 64).max(1);
        let mut s = slice_start;
        while s < slice_end {
            let local = s - slice_start;
            let (ix, iy, iz) = (distance.dense_x[s], distance.dense_y[s], distance.dense_z[s]);
            let base_offset = local * player_count;
            let mut j = 0;
            while j < player_count {
                if s != j {
                    let dx = ix - distance.dense_x[j];
                    let dy = iy - distance.dense_y[j];
                    let dz = iz - distance.dense_z[j];
                    let dist_sq = dx * dx + dy * dy + dz * dz;
                    let (expected_byte, _) = Self::calculate_interval_from_distance_sq(dist_sq);
                    let expected_quality = Self::get_quality_index(dist_sq) as u8;
                    checked_pairs += 1;
                    let device_quality = distance.device_quality[base_offset + j];
                    if device_quality != expected_quality {
                        BNL::log_warning(format!(
                            "[BSR] The compute backend disagrees with the CPU on a quality tier (device {device_quality}, cpu {expected_quality}). Refusing the offload; the sweep stays on the CPU."
                        ));
                        Self::disable_distance_solver(distance);
                        return false;
                    }
                    let difference = i32::from(distance.device_interval_byte[base_offset + j]) - i32::from(expected_byte);
                    if !(-1..=1).contains(&difference) {
                        interval_drift += 1;
                    }
                }
                j += sender_step;
            }
            s += receiver_step;
        }
        if interval_drift > 0 {
            BNL::log_warning(format!("[BSR] The compute backend's interval byte differs by more than one step on {interval_drift} of {checked_pairs} sampled pairs. Refusing the offload."));
            Self::disable_distance_solver(distance);
            return false;
        }
        distance.distance_solver_verified = true;
        BNL::log(format!("[BSR] Compute backend agrees with the CPU over {checked_pairs} sampled pairs."));
        true
    }

    fn disable_distance_solver(distance: &mut DistanceSweepState) {
        distance.distance_solver = None;
        *S.distance_backend.write() = "cpu (backend refused)".to_string();
        BNL::log(format!("[BSR] Refresh period back to {} ticks now the sweep is on the CPU again.", Self::distance_update_interval_ticks()));
    }

    fn scatter_device_results(distance: &DistanceSweepState, roster: &[(i32, Arc<PlayerState>)], slice_start: usize, slice_end: usize) {
        let player_count = roster.len();
        let tick_table = &distance.interval_tick_table;
        Self::parallel_for(slice_start, slice_end, |i| {
            let (id, state) = &roster[i];
            let base_offset = (i - slice_start) * player_count;
            let mut recv = state.receiver.lock();
            for (index, (j_id, _)) in roster.iter().enumerate() {
                if *id == *j_id {
                    continue;
                }
                let j_index = *j_id as usize;
                PlayerState::ensure_tracking(&mut recv, j_index);
                let encoded = distance.device_interval_byte[base_offset + index];
                let t = &mut recv.peer_tracking[j_index];
                t.cached_interval_ticks = tick_table.get(usize::from(encoded)).copied().unwrap_or(0);
                t.cached_quality_index = distance.device_quality[base_offset + index];
                t.cached_interval_byte = encoded;
            }
        });
    }
}
