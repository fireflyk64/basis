//! Port of `Diagnostics/BasisServerMemoryReclaim.cs`: hands memory back after the crowd leaves.
//!
//! The C# forced a compacting GC when the population dropped. Rust has no collector, but the
//! same symptom exists: after a mass departure the allocator holds the freed arenas and the
//! resident set never shrinks on its own. The population-drop trigger is kept as-is and the
//! reclaim step asks the allocator to return free pages to the OS (`malloc_trim` on glibc),
//! reporting the working set before and after.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::{Duration, Instant};

use basis_network_core::BNL;
use parking_lot::Mutex;

use crate::NetworkServer;
use crate::util::working_set_bytes;

struct SampleState {
    peak_since_pass: i32,
    eligible_since: Option<Instant>,
    last_pass: Option<Instant>,
}

static RUNNING: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<SampleState> = Mutex::new(SampleState { peak_since_pass: 0, eligible_since: None, last_pass: None });
static PASSES: AtomicI64 = AtomicI64::new(0);
static RECLAIMED_BYTES: AtomicI64 = AtomicI64::new(0);

pub struct BasisServerMemoryReclaim;

impl BasisServerMemoryReclaim {
    const SAMPLE_INTERVAL: Duration = Duration::from_secs(60);
    const DROP_DIVISOR: i32 = 4;
    const MINIMUM_SECONDS_BETWEEN_PASSES: f64 = 120.0;

    /// Reclaim passes this process has run because the population dropped.
    pub fn passes() -> i64 {
        PASSES.load(Ordering::Relaxed)
    }

    /// Resident bytes those passes freed, summed across passes.
    pub fn reclaimed_bytes() -> i64 {
        RECLAIMED_BYTES.load(Ordering::Relaxed)
    }

    pub fn start() {
        if STARTED.swap(true, Ordering::AcqRel) {
            return;
        }
        *STATE.lock() = SampleState { peak_since_pass: 0, eligible_since: None, last_pass: None };
        RUNNING.store(true, Ordering::Release);
        if let Err(e) = std::thread::Builder::new().name("MemoryReclaim".to_string()).spawn(Self::run) {
            RUNNING.store(false, Ordering::Release);
            STARTED.store(false, Ordering::Release);
            BNL::log_error(format!("[MemoryReclaim] could not start: {e}"));
        }
    }

    pub fn stop() {
        RUNNING.store(false, Ordering::Release);
        STARTED.store(false, Ordering::Release);
    }

    fn run() {
        while RUNNING.load(Ordering::Acquire) {
            Self::sample(Self::current_players());
            std::thread::sleep(Self::SAMPLE_INTERVAL);
        }
    }

    /// One sampler step for `players` connected. Returns true when a reclaim pass ran.
    pub fn sample(players: i32) -> bool {
        let Some(configuration) = NetworkServer::configuration() else {
            STATE.lock().eligible_since = None;
            return false;
        };
        if !configuration.idle_memory_reclaim_enabled {
            STATE.lock().eligible_since = None;
            return false;
        }
        let mut state = STATE.lock();
        if players > state.peak_since_pass {
            state.peak_since_pass = players;
        }
        let minimum_peak = configuration.idle_memory_reclaim_minimum_peak.max(1);
        if state.peak_since_pass < minimum_peak || players.saturating_mul(Self::DROP_DIVISOR) > state.peak_since_pass {
            state.eligible_since = None;
            return false;
        }
        let now = Instant::now();
        let Some(eligible_since) = state.eligible_since else {
            state.eligible_since = Some(now);
            return false;
        };
        if now.duration_since(eligible_since).as_secs_f64() < f64::from(configuration.idle_memory_reclaim_settle_seconds.max(1)) {
            return false;
        }
        if let Some(last_pass) = state.last_pass
            && now.duration_since(last_pass).as_secs_f64() < Self::MINIMUM_SECONDS_BETWEEN_PASSES
        {
            return false;
        }
        let peak = state.peak_since_pass;
        state.peak_since_pass = players;
        state.eligible_since = None;
        state.last_pass = Some(now);
        drop(state);
        Self::collect(peak, players);
        true
    }

    fn collect(peak: i32, players: i32) {
        let working_set_before = working_set_bytes();
        let started = Instant::now();
        Self::trim_allocator();
        let working_set_after = working_set_bytes();
        PASSES.fetch_add(1, Ordering::Relaxed);
        if working_set_before > working_set_after {
            RECLAIMED_BYTES.fetch_add((working_set_before - working_set_after) as i64, Ordering::Relaxed);
        }
        BNL::log(format!(
            "[MemoryReclaim] {peak} -> {players} players: working set {} -> {} MB in {:.0} ms.",
            Self::megabytes(working_set_before),
            Self::megabytes(working_set_after),
            started.elapsed().as_secs_f64() * 1000.0
        ));
    }

    /// Returns free heap pages to the OS. glibc only; elsewhere the pass just reports.
    fn trim_allocator() {
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        unsafe {
            // SAFETY: malloc_trim takes an integer pad and touches only allocator state.
            libc::malloc_trim(0);
        }
    }

    fn current_players() -> i32 {
        match NetworkServer::server() {
            Some(server) => server.connected_peers_count(),
            None => NetworkServer::authenticated_peers().len() as i32,
        }
    }

    fn megabytes(bytes: u64) -> String {
        format!("{:.1}", bytes as f64 / 1_048_576.0)
    }

    pub fn reset_for_tests() {
        *STATE.lock() = SampleState { peak_since_pass: 0, eligible_since: None, last_pass: None };
        PASSES.store(0, Ordering::Relaxed);
        RECLAIMED_BYTES.store(0, Ordering::Relaxed);
    }
}
