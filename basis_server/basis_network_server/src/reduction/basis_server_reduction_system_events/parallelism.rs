//! `BasisServerReductionSystemEvents.Parallelism.cs`: sizes the send worker pool from the
//! measured cost of the send pass.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, LazyLock};

use basis_network_core::configuration::{BasisPopulationScale, LNLTransportConfig};
use basis_network_core::{BNL, BasisCpuBudget, BasisSimdCapabilities};
use parking_lot::{Mutex, RwLock};
use rayon::ThreadPool;

use super::{BasisServerReductionSystemEvents, now_ticks};

/// The state behind the worker-count controller.
#[derive(Debug)]
pub struct PoolTuning {
    /// Sender/receiver pairs one worker gets through per millisecond the send pass is busy.
    pub pairs_per_worker_ms: f64,
    /// Share of the tick period the send pass is sized against.
    pub send_phase_budget_share: f64,
    pub send_budget_duty_ema: f64,
    pub last_send_workers: i32,
    pub last_degree_step_tick: i64,
    pub aggregate_rate_ema: f64,
    pub widen_trial_from: i32,
    pub aggregate_rate_at_widen: f64,
    pub passes_since_widen: i32,
    pub learned_width_ceiling: i32,
    pub learned_ceiling_players: i32,
    pub learned_ceiling_send_cap: i32,
    pub learned_ceiling_tick: i64,
    pub configured_degree: i32,
}

impl Default for PoolTuning {
    fn default() -> Self {
        Self {
            pairs_per_worker_ms: 0.0,
            send_phase_budget_share: BasisServerReductionSystemEvents::DEFAULT_SEND_PHASE_BUDGET_SHARE,
            send_budget_duty_ema: 0.0,
            last_send_workers: 0,
            last_degree_step_tick: 0,
            aggregate_rate_ema: 0.0,
            widen_trial_from: 0,
            aggregate_rate_at_widen: 0.0,
            passes_since_widen: 0,
            learned_width_ceiling: 0,
            learned_ceiling_players: 0,
            learned_ceiling_send_cap: 0,
            learned_ceiling_tick: 0,
            configured_degree: 0,
        }
    }
}

static POOL_TUNING: LazyLock<Mutex<PoolTuning>> = LazyLock::new(|| Mutex::new(PoolTuning::default()));
/// The C# `parallelOptions.MaxDegreeOfParallelism`.
static DEGREE: AtomicI32 = AtomicI32::new(4);
type BuiltPool = (i32, Arc<ThreadPool>);
static POOL: LazyLock<RwLock<Option<BuiltPool>>> = LazyLock::new(|| RwLock::new(None));

impl BasisServerReductionSystemEvents {
    pub(super) const DEFAULT_SEND_PHASE_BUDGET_SHARE: f64 = 0.6;
    /// Shortest pass worth taking a rate from.
    const MIN_TIMEABLE_SEND_PASS_MS: f64 = 0.25;
    /// Utilisation above which widening moves contention around rather than work.
    const WIDEN_BELOW_UTILIZATION: f64 = 0.70;
    /// Passes to wait before judging a widening.
    pub(super) const WIDEN_TRIAL_PASSES: i32 = 24;
    /// How much better the pool has to get for a widening to be kept.
    const WIDEN_MUST_IMPROVE_BY: f64 = 1.05;
    /// A learned ceiling is a verdict about one load level, so it expires.
    pub(super) const LEARNED_CEILING_RETRY_TICKS: i64 = 30_000 * 1000;
    pub(super) const REBALANCE_INTERVAL_TICKS: i64 = (BasisCpuBudget::REBALANCE_INTERVAL_MS as i64) * 1000;

    fn cores() -> i32 {
        std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(1)
    }

    fn max_auto_workers() -> i32 {
        BasisCpuBudget::reduction_send_cap()
    }

    /// The pool for the current degree, rebuilt when the degree changed.
    pub(super) fn pool() -> Option<Arc<ThreadPool>> {
        let degree = DEGREE.load(Ordering::Relaxed).max(1) as usize;
        if let Some((built_degree, pool)) = POOL.read().as_ref()
            && *built_degree as usize == degree
        {
            return Some(pool.clone());
        }
        let mut slot = POOL.write();
        if let Some((built_degree, pool)) = slot.as_ref()
            && *built_degree as usize == degree
        {
            return Some(pool.clone());
        }
        match rayon::ThreadPoolBuilder::new().num_threads(degree).thread_name(|i| format!("bsr-send-{i}")).build() {
            Ok(pool) => {
                let pool = Arc::new(pool);
                *slot = Some((degree as i32, pool.clone()));
                Some(pool)
            }
            Err(e) => {
                BNL::log_error(format!("[BSR] could not build a {degree}-worker send pool: {e}; keeping the previous one"));
                slot.as_ref().map(|(_, pool)| pool.clone())
            }
        }
    }

    /// `Parallel.For(start, end, body)` on the send pool; runs inline if the pool cannot be built.
    pub(super) fn parallel_for(start: usize, end: usize, body: impl Fn(usize) + Sync + Send) {
        if end <= start {
            return;
        }
        match Self::pool() {
            Some(pool) => {
                use rayon::prelude::*;
                pool.install(|| (start..end).into_par_iter().for_each(&body));
            }
            None => {
                for i in start..end {
                    body(i);
                }
            }
        }
    }

    /// Width the next send pass wants, from what the last ones cost.
    pub(super) fn degree_for(t: &PoolTuning, player_count: i32, mut current: i32) -> i32 {
        let cores = Self::cores();
        if t.configured_degree > 0 {
            return t.configured_degree.min(cores).max(1);
        }
        let mut ceiling = Self::max_auto_workers().min(cores);
        if t.learned_width_ceiling > 0 && t.learned_width_ceiling < ceiling {
            ceiling = t.learned_width_ceiling;
        }
        ceiling = ceiling.max(1);
        let floor = BasisCpuBudget::min_workers_per_pool().min(ceiling).max(1);

        let rate = t.pairs_per_worker_ms;
        if rate <= 0.0 {
            // Nothing timed yet, and the floor is the whole answer.
            return floor;
        }
        let slice_count = Self::slice_count().max(1);
        let pairs = f64::from((player_count + slice_count - 1) / slice_count) * f64::from(player_count);
        let budget_ms = (Self::interval_ms() as f64).max(1.0) * t.send_phase_budget_share;
        let needed = pairs / (rate * budget_ms);
        let mut target = if needed >= f64::from(ceiling) { ceiling } else { needed.ceil() as i32 };
        if target < floor {
            target = floor;
        }
        current = current.clamp(floor, ceiling);
        if target == current {
            return current;
        }
        if target < current {
            // Give workers back one at a time.
            return current - 1;
        }
        // Widen only where there are cores to widen into.
        if BasisCpuBudget::utilization() > Self::WIDEN_BELOW_UTILIZATION {
            return current;
        }
        // At most a doubling per step.
        target.min(current * 2)
    }

    /// Retunes the worker count for what the next pass is expected to cost, on the core
    /// allocator's cadence rather than the tick's.
    pub(super) fn tune_parallelism(player_count: i32) {
        let now = now_ticks();
        let mut t = POOL_TUNING.lock();
        if t.last_degree_step_tick != 0 && now - t.last_degree_step_tick < Self::REBALANCE_INTERVAL_TICKS {
            return;
        }
        t.last_degree_step_tick = now;
        let mut current = DEGREE.load(Ordering::Relaxed);
        // A widening on trial holds the width still until it has answered for itself.
        if t.widen_trial_from > 0 {
            if t.passes_since_widen < Self::WIDEN_TRIAL_PASSES {
                return;
            }
            current = Self::resolve_widen_trial(&mut t, current, player_count, now);
        }
        Self::expire_learned_ceiling(&mut t, now, player_count);
        let desired = Self::degree_for(&t, player_count, current);
        if desired == current {
            return;
        }
        if desired > current {
            t.widen_trial_from = current;
            t.aggregate_rate_at_widen = t.aggregate_rate_ema;
            t.passes_since_widen = 0;
        }
        DEGREE.store(desired, Ordering::Relaxed);
    }

    /// Decides whether the widening on trial earned its workers, and gives them back if not.
    pub(super) fn resolve_widen_trial(t: &mut PoolTuning, current: i32, player_count: i32, now: i64) -> i32 {
        let from = t.widen_trial_from;
        let before = t.aggregate_rate_at_widen;
        let after = t.aggregate_rate_ema;
        t.widen_trial_from = 0;
        t.aggregate_rate_at_widen = 0.0;
        // No usable comparison: let the widening stand; the next one gets judged.
        if before <= 0.0 || after <= 0.0 || current <= from {
            return current;
        }
        if after >= before * Self::WIDEN_MUST_IMPROVE_BY {
            return current;
        }
        t.learned_width_ceiling = from;
        t.learned_ceiling_players = player_count;
        t.learned_ceiling_send_cap = Self::max_auto_workers();
        t.learned_ceiling_tick = now;
        DEGREE.store(from, Ordering::Relaxed);
        BNL::log_warning(format!(
            "[BSR] Send pool {from} -> {current} workers did not pay ({before:.0} -> {after:.0} pairs/ms); holding at {from}. Past the transport's send paths, workers queue on the same one, so what adds capacity is send paths, not cores."
        ));
        from
    }

    /// Drops a learned ceiling once the verdict behind it no longer applies.
    pub(super) fn expire_learned_ceiling(t: &mut PoolTuning, now: i64, player_count: i32) {
        if t.learned_width_ceiling <= 0 {
            return;
        }
        let population_moved = player_count * 4 > t.learned_ceiling_players * 5 || player_count * 4 < t.learned_ceiling_players * 3;
        let more_send_paths = Self::max_auto_workers() > t.learned_ceiling_send_cap;
        let stale = now - t.learned_ceiling_tick > Self::LEARNED_CEILING_RETRY_TICKS;
        if population_moved || more_send_paths || stale {
            t.learned_width_ceiling = 0;
        }
    }

    /// Records what a send pass cost: pairs per millisecond busy, per worker that ran it.
    pub(super) fn note_send_pass_cost(pairs: i64, busy_ms: f64, workers: i32) {
        let mut t = POOL_TUNING.lock();
        let budget_ms = (Self::interval_ms() as f64).max(1.0) * t.send_phase_budget_share;
        let duty = busy_ms / budget_ms;
        t.send_budget_duty_ema = if t.send_budget_duty_ema <= 0.0 { duty } else { t.send_budget_duty_ema * 0.9 + duty * 0.1 };
        t.last_send_workers = workers;
        if pairs <= 0 || workers <= 0 || busy_ms < Self::MIN_TIMEABLE_SEND_PASS_MS {
            return;
        }
        let rate = pairs as f64 / (busy_ms * f64::from(workers));
        if rate.is_nan() || rate <= 0.0 || !rate.is_finite() {
            return;
        }
        let aggregate = pairs as f64 / busy_ms;
        t.aggregate_rate_ema = if t.aggregate_rate_ema <= 0.0 { aggregate } else { t.aggregate_rate_ema * 0.9 + aggregate * 0.1 };
        if t.widen_trial_from > 0 && t.passes_since_widen < Self::WIDEN_TRIAL_PASSES {
            t.passes_since_widen += 1;
        }
        // Smoothed hard: one pass that straddled a stall is not a slower machine.
        t.pairs_per_worker_ms = if t.pairs_per_worker_ms <= 0.0 { rate } else { t.pairs_per_worker_ms * 0.9 + rate * 0.1 };
    }

    /// Sets the send pass's share of the tick period, as a percentage; 0 restores the fitted
    /// default. Clamped to 20..85.
    pub fn set_send_phase_budget_percent(percent: i32) {
        let mut t = POOL_TUNING.lock();
        t.send_phase_budget_share = if percent <= 0 { Self::DEFAULT_SEND_PHASE_BUDGET_SHARE } else { f64::from(percent.clamp(20, 85)) / 100.0 };
    }

    /// Workers the send pool is currently allowed to use.
    pub fn send_workers() -> i32 {
        DEGREE.load(Ordering::Relaxed)
    }

    pub(super) fn set_send_workers(workers: i32) {
        DEGREE.store(workers.max(1), Ordering::Relaxed);
    }

    /// Workers the core allocator currently grants the send pass.
    pub fn send_worker_ceiling() -> i32 {
        Self::max_auto_workers()
    }

    pub fn pairs_per_worker_ms() -> f64 {
        POOL_TUNING.lock().pairs_per_worker_ms
    }

    pub fn send_budget_duty() -> f64 {
        POOL_TUNING.lock().send_budget_duty_ema
    }

    pub fn send_phase_budget_percent() -> i32 {
        (POOL_TUNING.lock().send_phase_budget_share * 100.0).round() as i32
    }

    pub(super) fn with_pool_tuning<R>(f: impl FnOnce(&mut PoolTuning) -> R) -> R {
        f(&mut POOL_TUNING.lock())
    }

    pub fn set_max_degree_of_parallelism(configured: i32) {
        Self::ensure_started();
        // The vector width every SIMD path runs at is chosen from the host; it is the first thing
        // worth knowing when two machines disagree on throughput.
        BNL::log(format!("[CPU] {}", BasisSimdCapabilities::describe()));
        let cores = Self::cores();
        let mut t = POOL_TUNING.lock();
        t.configured_degree = configured;
        if configured > 0 {
            let resolved = configured.min(cores).max(1);
            DEGREE.store(resolved, Ordering::Relaxed);
            BNL::log(format!("[BSR] Parallel worker cap pinned to {resolved} (of {cores} cores)."));
        } else {
            BNL::log(format!("[CPU] {}", BasisCpuBudget::describe()));
            BNL::log(format!(
                "[BSR] Send workers sized from measured pass cost against {:.0}% of the tick period{}, {} to {}; at the floor until this host has timed a pass.",
                t.send_phase_budget_share * 100.0,
                if t.send_phase_budget_share == Self::DEFAULT_SEND_PHASE_BUDGET_SHARE { " (default)" } else { " (fitted, from config)" },
                BasisCpuBudget::min_workers_per_pool(),
                Self::max_auto_workers()
            ));
            BNL::log(format!("[POP] at 1000 players this box would resolve: {}", BasisPopulationScale::describe(1000, LNLTransportConfig::default().packet_pool_size_per_peer)));
        }
    }
}
