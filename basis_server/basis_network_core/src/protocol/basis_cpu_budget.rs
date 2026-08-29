//! Port of `BasisCoreLease` / `BasisCpuBudget` (BasisNetworkCore/Protocol/BasisNetworkCommons.cs).
//!
//! Divides the machine's cores between the server's parallel pools so that the sum of every grant
//! stays inside the machine. Subsystems [`BasisCpuBudget::register`] a [`BasisCoreLease`] and read
//! [`BasisCoreLease::granted`] wherever a worker count is needed; the allocator moves grants with
//! reported demand and discovers each lease's real ceiling by experiment.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use parking_lot::{Mutex, RwLock};

/// An `f64` stored by bit pattern so it can be read and written without a lock
/// (the C# used `Volatile.Read/Write` on a `double`).
struct AtomicF64(AtomicU64);

impl AtomicF64 {
    const fn new(value: f64) -> Self {
        Self(AtomicU64::new(value.to_bits()))
    }

    fn load(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Acquire))
    }

    fn store(&self, value: f64) {
        self.0.store(value.to_bits(), Ordering::Release);
    }
}

/// The ceiling callback a lease declares (`System.Func<int>` in C#).
pub type MaxCoresFn = Box<dyn Fn() -> i32 + Send + Sync>;

/// Discovery state, driven only by [`BasisCpuBudget`] under its rebalance lock.
#[derive(Debug, Default)]
struct ProbeState {
    /// Grant the discovery pass is holding this lease at, or 0 when it is not.
    forced_grant: i32,
    /// 0 = idle, 1 = measuring baseline, 2 = measuring narrowed.
    phase: u8,
    baseline_grant: i32,
    baseline_rate: f64,
    window_start: Option<Instant>,
    window_start_work: i64,
    window_start_busy_micros: i64,
    cooldown_steps: i32,
    /// Demand at the moment a ceiling was accepted. A ceiling is only true for the load it was
    /// measured under, and this is what lets the allocator notice the load has moved on.
    demand_at_settle: f64,
}

/// A subsystem's claim on a share of the machine.
///
/// `min_cores` is the floor below which it cannot do its job, `max_cores` the ceiling past which
/// it cannot convert cores into throughput, `weight` its resting share. A lease that reports work
/// through [`add_work`](Self::add_work) gets its ceiling found by experiment (see the discovery
/// pass in [`BasisCpuBudget`]) and [`effective_max`](Self::effective_max) drops to whatever that
/// turns out to be. A lease that reports nothing keeps its declared number, because there is
/// nothing to measure it against.
pub struct BasisCoreLease {
    name: String,
    min_cores: i32,
    max_cores: MaxCoresFn,
    weight: f64,
    granted: AtomicI32,
    demand: AtomicF64,
    work: AtomicI64,
    busy_micros: AtomicI64,
    ever_reported_work: AtomicBool,
    /// Ceiling found by experiment. `i32::MAX` until something has been measured, so the declared
    /// ceiling governs from a cold start.
    discovered_max: AtomicI32,
    probe: Mutex<ProbeState>,
}

impl fmt::Debug for BasisCoreLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BasisCoreLease")
            .field("name", &self.name)
            .field("min_cores", &self.min_cores)
            .field("weight", &self.weight)
            .field("granted", &self.granted())
            .field("demand", &self.demand())
            .field("effective_max", &self.effective_max())
            .finish()
    }
}

impl BasisCoreLease {
    fn new(name: &str, min_cores: i32, max_cores: MaxCoresFn, weight: f64) -> Self {
        let min_cores = if min_cores < 1 { 1 } else { min_cores };
        Self {
            name: name.to_string(),
            min_cores,
            max_cores,
            weight: if weight <= 0.0 { 1.0 } else { weight },
            granted: AtomicI32::new(min_cores),
            demand: AtomicF64::new(0.0),
            work: AtomicI64::new(0),
            busy_micros: AtomicI64::new(0),
            ever_reported_work: AtomicBool::new(false),
            discovered_max: AtomicI32::new(i32::MAX),
            probe: Mutex::new(ProbeState::default()),
        }
    }

    /// Subsystem name, for the allocator's diagnostic line.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Workers below which this subsystem cannot do its job at all.
    pub fn min_cores(&self) -> i32 {
        self.min_cores
    }

    /// Workers past which this subsystem cannot convert cores into throughput, as declared.
    pub fn max_cores(&self) -> i32 {
        (self.max_cores)()
    }

    /// Resting share of the pool when no lease reports pressure.
    pub fn weight(&self) -> f64 {
        self.weight
    }

    /// Cores the allocator has granted. Read this to size a parallel region.
    pub fn granted(&self) -> i32 {
        self.granted.load(Ordering::Acquire)
    }

    /// Most recent demand this lease reported, 0..1.
    pub fn demand(&self) -> f64 {
        self.demand.load()
    }

    /// How saturated this subsystem is right now, 0 (idle) to 1 (cannot keep up). Usually a pass
    /// duration over its budget. Cheap enough to call every pass; the allocator only reads it on
    /// its own cadence.
    pub fn report_demand(&self, demand01: f64) {
        let clamped = if demand01.is_nan() { 0.0 } else { demand01.clamp(0.0, 1.0) };
        self.demand.store(clamped);
    }

    pub(crate) fn grant(&self, cores: i32) {
        self.granted.store(cores, Ordering::Release);
    }

    /// The ceiling this lease declared, floored at [`min_cores`](Self::min_cores).
    pub fn declared_max(&self) -> i32 {
        let max = (self.max_cores)();
        if max < self.min_cores { self.min_cores } else { max }
    }

    /// Reports a completed pass: how much work it did, and how long it was busy doing it.
    ///
    /// Units are whatever this subsystem counts in — receivers served, peers updated — and never
    /// have to agree between leases, because only the ratio is ever used.
    ///
    /// **Both halves are required, and the second is the one that makes this measurable.** Work
    /// per second of wall time is not a core-scaling signal: a pass that runs on a fixed cadence
    /// delivers the same amount per second however many workers it has. Work per millisecond *of
    /// the pool's own busy time* asks the question that actually matters — how much can this pool
    /// chew through while it is running — and that number does rise with cores, right up until
    /// the point where it stops.
    ///
    /// Calling this is what opts a lease into having its ceiling measured rather than believed.
    pub fn add_work(&self, units: i64, busy_ms: f64) {
        if units <= 0 || busy_ms.is_nan() || busy_ms <= 0.0 {
            return;
        }
        self.work.fetch_add(units, Ordering::AcqRel);
        self.busy_micros.fetch_add((busy_ms * 1000.0) as i64, Ordering::AcqRel);
        self.ever_reported_work.store(true, Ordering::Release);
    }

    pub fn work_total(&self) -> i64 {
        self.work.load(Ordering::Acquire)
    }

    pub fn busy_micros_total(&self) -> i64 {
        self.busy_micros.load(Ordering::Acquire)
    }

    pub fn reports_work(&self) -> bool {
        self.ever_reported_work.load(Ordering::Acquire)
    }

    /// Ceiling found by experiment, `i32::MAX` until one has been measured.
    pub fn discovered_max(&self) -> i32 {
        self.discovered_max.load(Ordering::Acquire)
    }

    /// Grant the discovery pass is holding this lease at, or 0 when it is not.
    pub fn forced_grant(&self) -> i32 {
        self.probe.lock().forced_grant
    }

    /// Discovery phase: 0 = idle, 1 = measuring baseline, 2 = measuring narrowed.
    pub fn probe_phase(&self) -> u8 {
        self.probe.lock().phase
    }

    /// Rebalance steps until this lease is re-measured.
    pub fn probe_cooldown_steps(&self) -> i32 {
        self.probe.lock().cooldown_steps
    }

    /// The measured ceiling if there is one, otherwise the declared one. Never below
    /// [`min_cores`](Self::min_cores).
    pub fn effective_max(&self) -> i32 {
        let declared = self.declared_max();
        let discovered = self.discovered_max();
        let max = if discovered < declared { discovered } else { declared };
        if max < self.min_cores { self.min_cores } else { max }
    }

    /// Throws away what was measured, because the thing that set the ceiling has changed — a send
    /// socket added, the population stepped up. The next discovery pass starts again from the
    /// declared bound rather than staying pinned to a number that described a machine in a
    /// different configuration.
    pub fn invalidate_discovery(&self) {
        self.discovered_max.store(i32::MAX, Ordering::Release);
        let mut probe = self.probe.lock();
        probe.phase = 0;
        probe.forced_grant = 0;
        probe.cooldown_steps = 0;
    }

    /// True once a ceiling has actually been measured for this lease.
    pub fn has_measured_ceiling(&self) -> bool {
        self.discovered_max() != i32::MAX
    }
}

struct Registry {
    leases: RwLock<Vec<Arc<BasisCoreLease>>>,
    /// Serialises rebalance passes; the C# ran them unsynchronised, which is a race between the
    /// timer and a registration in Rust's terms.
    rebalance: Mutex<()>,
    /// The single lease under measurement, if any.
    probing: Mutex<Option<Arc<BasisCoreLease>>>,
    send_socket_count: AtomicI32,
    probe_window_ms: AtomicF64,
    utilization: AtomicF64,
    cpu_sample: Mutex<CpuSample>,
}

#[derive(Default)]
struct CpuSample {
    last_cpu_micros: Option<u64>,
    last_wall: Option<Instant>,
}

static REGISTRY: Registry = Registry {
    leases: RwLock::new(Vec::new()),
    rebalance: Mutex::new(()),
    probing: Mutex::new(None),
    send_socket_count: AtomicI32::new(1),
    probe_window_ms: AtomicF64::new(2000.0),
    utilization: AtomicF64::new(0.0),
    cpu_sample: Mutex::new(CpuSample { last_cpu_micros: None, last_wall: None }),
};

/// The two standing leases. Registered here rather than by their owners because they are
/// referenced through [`BasisCpuBudget::reduction_send_cap`] and
/// [`BasisCpuBudget::peer_update_cap`] from code that runs before either subsystem has started.
static STANDING: LazyLock<(Arc<BasisCoreLease>, Arc<BasisCoreLease>)> = LazyLock::new(|| {
    let reduction = BasisCpuBudget::register_inner(
        "reduction-send",
        BasisCpuBudget::min_workers_per_pool(),
        Box::new(BasisCpuBudget::max_reduction_send_workers),
        BasisCpuBudget::REDUCTION_SEND_WEIGHT,
    );
    let peer_update = BasisCpuBudget::register_inner(
        "peer-update",
        BasisCpuBudget::min_workers_per_pool(),
        Box::new(BasisCpuBudget::total_cores),
        BasisCpuBudget::PEER_UPDATE_WEIGHT,
    );
    (reduction, peer_update)
});

static TOTAL_CORES: LazyLock<i32> = LazyLock::new(|| {
    std::thread::available_parallelism()
        .map(|n| i32::try_from(n.get()).unwrap_or(i32::MAX))
        .unwrap_or(1)
});

/// Divides the machine's cores between the server's parallel pools.
///
/// It is a pool, not a split. Subsystems [`register`](Self::register) a [`BasisCoreLease`] and read
/// [`BasisCoreLease::granted`]; the allocator holds the invariant nobody can hold alone — **the sum
/// of every grant stays inside the machine**. Grants move with load: each lease reports how
/// saturated it is, and cores flow toward the subsystem that is behind and away from the one that
/// is coasting, bounded at both ends by what that subsystem itself declared it can use. Movement
/// is damped, so the split follows a real load change in about a second without chasing single
/// noisy samples.
///
/// Why the two standing leases get the resting split they do:
///  - The send phase is **throughput-bound** and already rate-limited by the tick budget, so extra
///    workers cannot make it deliver sooner. It takes the smaller share.
///  - The per-peer pass is **latency-bound**: its interval is the floor on reliable delivery. It
///    takes the larger share.
pub struct BasisCpuBudget;

impl BasisCpuBudget {
    /// Resting share for the reduction system's send/process/distance phases.
    pub const REDUCTION_SEND_WEIGHT: f64 = 1.0;

    /// Resting share for the transport's per-peer update pass.
    pub const PEER_UPDATE_WEIGHT: f64 = 3.0;

    /// Threads that can usefully contend on one UDP socket before they just queue.
    const WORKERS_PER_SEND_SOCKET: i32 = 8;

    /// How hard measured pressure may bend the resting split. At gain 4: both pools idle holds
    /// 25/75; the send loop saturated alone moves to 62/38; the peer pass saturated alone moves
    /// to 13/87.
    const PRESSURE_GAIN: f64 = 4.0;

    /// Fraction of the distance to the target moved per rebalance.
    const DAMPING: f64 = 0.25;

    /// How often live pool load is sampled for the diagnostic line.
    pub const REBALANCE_INTERVAL_MS: i32 = 100;

    /// How far throughput may fall before a narrower grant counts as too narrow. Measured against
    /// the rate at the *start* of the narrowing sequence, not against the previous step.
    const PROBE_TOLERANCE: f64 = 0.02;

    /// Demand below which a lease is not worth measuring.
    const PROBE_MIN_DEMAND: f64 = 0.5;

    /// Rebalance steps between re-measurements. ~60s at a 100ms cadence.
    const PROBE_COOLDOWN_STEPS: i32 = 600;

    /// How much demand has to climb above where a ceiling was measured before that ceiling is
    /// treated as describing a load the server has left behind.
    const DEMAND_REOPEN_MARGIN: f64 = 0.15;

    /// Floor for any pool. Below roughly this a parallel region costs more to dispatch than it
    /// saves — but never more workers than the machine has cores.
    pub fn min_workers_per_pool() -> i32 {
        4.min(1.max(Self::total_cores()))
    }

    /// Sizes a per-thread-contention structure — stripe tables, shard counts, pools whose only
    /// job is to keep concurrent threads off each other's cache lines. Always derived, never a
    /// literal; rounded up to a power of two so callers can index it with a mask.
    pub fn concurrency_width(per_core: i32, min: i32, max: i32) -> i32 {
        let per_core = i64::from(if per_core < 1 { 1 } else { per_core });
        let mut wanted = i64::from(Self::total_cores()) * per_core;
        if wanted < i64::from(min) {
            wanted = i64::from(min);
        }
        if wanted > i64::from(max) {
            wanted = i64::from(max);
        }
        let mut pow2: i64 = 1;
        while pow2 < wanted && pow2 < (1 << 30) {
            pow2 <<= 1;
        }
        i32::try_from(pow2).unwrap_or(1 << 30)
    }

    /// Hard ceiling on the send pool regardless of how many cores the machine has: what limits
    /// it is the send syscall on a single socket, so its width is an absolute number per socket.
    pub fn max_reduction_send_workers() -> i32 {
        Self::total_cores()
            .min(Self::WORKERS_PER_SEND_SOCKET.saturating_mul(1.max(Self::send_socket_count())))
    }

    /// Sockets the send path has available.
    pub fn send_socket_count() -> i32 {
        REGISTRY.send_socket_count.load(Ordering::Acquire)
    }

    /// Records how many send sockets were actually bound. A change here invalidates anything
    /// measured about the send pool.
    pub fn set_send_socket_count(bound: i32) {
        let next = if bound > 0 { bound } else { 1 };
        if next == Self::send_socket_count() {
            return;
        }
        REGISTRY.send_socket_count.store(next, Ordering::Release);
        Self::reduction_send_lease().invalidate_discovery();
    }

    /// Ceiling on runtime-added send sockets when the operator has not set one: half the cores,
    /// floored at four but never above the core count.
    pub fn auto_max_send_sockets() -> i32 {
        let cores = Self::total_cores();
        let floor = 4.min(cores);
        let mut wanted = cores / 2;
        if wanted < floor {
            wanted = floor;
        }
        if wanted > cores {
            wanted = cores;
        }
        wanted
    }

    /// Cores the allocator is dividing up.
    pub fn total_cores() -> i32 {
        *TOTAL_CORES
    }

    /// Fraction of the whole machine this process is using, 0..1, over the last sample.
    pub fn utilization() -> f64 {
        REGISTRY.utilization.load()
    }

    /// Re-samples [`utilization`](Self::utilization). Cheap enough for a ~100 ms cadence.
    /// Returns the previous value unchanged on a platform that will not report CPU time.
    pub fn sample_utilization() -> f64 {
        let now = Instant::now();
        let Some(cpu_micros) = process_cpu_micros() else {
            return REGISTRY.utilization.load();
        };

        let mut sample = REGISTRY.cpu_sample.lock();
        if let (Some(last_cpu), Some(last_wall)) = (sample.last_cpu_micros, sample.last_wall) {
            let wall_ms = now.saturating_duration_since(last_wall).as_secs_f64() * 1000.0;
            let cpu_ms = cpu_micros.saturating_sub(last_cpu) as f64 / 1000.0;
            if wall_ms > 0.0 {
                let u = (cpu_ms / (wall_ms * f64::from(Self::total_cores()))).clamp(0.0, 1.0);
                let previous = REGISTRY.utilization.load();
                // Smoothed: a single sample straddling a GC pause is not a busy machine.
                let next = if previous <= 0.0 { u } else { previous * 0.7 + u * 0.3 };
                REGISTRY.utilization.store(next);
            }
        }
        sample.last_cpu_micros = Some(cpu_micros);
        sample.last_wall = Some(now);
        REGISTRY.utilization.load()
    }

    /// Cores held back from the pool for threads that are not pools at all: the tick loop, logic
    /// loop and stats thread are one apiece; the receive threads are one per bound socket. Capped
    /// at half the machine.
    fn reserved_cores() -> i32 {
        let cores = Self::total_cores();
        let reserved = 1 + 1.max(Self::send_socket_count());
        let ceiling = 1.max(cores / 2);
        if reserved > ceiling { ceiling } else { reserved }
    }

    fn ensure_standing() {
        LazyLock::force(&STANDING);
    }

    /// Claims a share of the machine for a subsystem. Call once at startup and keep the lease;
    /// read [`BasisCoreLease::granted`] wherever a worker count is needed.
    pub fn register(name: &str, min_cores: i32, max_cores: MaxCoresFn, weight: f64) -> Arc<BasisCoreLease> {
        Self::ensure_standing();
        Self::register_inner(name, min_cores, max_cores, weight)
    }

    fn register_inner(name: &str, min_cores: i32, max_cores: MaxCoresFn, weight: f64) -> Arc<BasisCoreLease> {
        let lease = Arc::new(BasisCoreLease::new(name, min_cores, max_cores, weight));
        REGISTRY.leases.write().push(Arc::clone(&lease));
        Self::rebalance_inner();
        lease
    }

    /// Gives a lease's cores back to the pool. A subsystem that has stopped should not keep
    /// holding a share of the machine.
    pub fn unregister(lease: &Arc<BasisCoreLease>) {
        Self::ensure_standing();
        {
            let mut leases = REGISTRY.leases.write();
            let before = leases.len();
            leases.retain(|l| !Arc::ptr_eq(l, lease));
            if leases.len() == before {
                return;
            }
        }
        // A lease removed mid-probe would otherwise hold the single-prober slot forever.
        {
            let mut probing = REGISTRY.probing.lock();
            if probing.as_ref().is_some_and(|p| Arc::ptr_eq(p, lease)) {
                *probing = None;
            }
        }
        Self::rebalance_inner();
    }

    /// Every registered lease, for diagnostics.
    pub fn leases() -> Vec<Arc<BasisCoreLease>> {
        Self::ensure_standing();
        REGISTRY.leases.read().clone()
    }

    /// How long one grant is held before its throughput is believed.
    pub fn probe_window_ms() -> f64 {
        REGISTRY.probe_window_ms.load()
    }

    /// Test seam: a narrowing search takes one window per step, so a test that ran at the shipped
    /// length would spend a minute proving a controller that converges in seconds.
    pub fn set_probe_window_ms(ms: f64) {
        REGISTRY.probe_window_ms.store(if ms.is_finite() && ms >= 0.0 { ms } else { 2000.0 });
    }

    /// Finds each lease's real ceiling by experiment. Discovery narrows rather than widens: taking
    /// a core away and checking whether throughput held is a test that is always affordable, and
    /// it converges on the ceiling from above. One lease at a time.
    fn drive_discovery(leases: &[Arc<BasisCoreLease>], probing: &mut Option<Arc<BasisCoreLease>>) {
        let now = Instant::now();
        let window_ms = Self::probe_window_ms();

        for lease in leases {
            let mut probe = lease.probe.lock();

            // A ceiling describes the load it was measured under, not the machine. When demand
            // climbs well past that point the old number is stale; reopen generously and let the
            // search narrow back down.
            if lease.has_measured_ceiling() && lease.demand() > probe.demand_at_settle + Self::DEMAND_REOPEN_MARGIN {
                let measured = lease.discovered_max();
                lease
                    .discovered_max
                    .store(measured.saturating_add(measured / 2).saturating_add(1), Ordering::Release);
                probe.demand_at_settle = lease.demand();
                probe.cooldown_steps = 0;
            }

            if probe.cooldown_steps > 0 {
                probe.cooldown_steps -= 1;
                if probe.cooldown_steps == 0 && lease.has_measured_ceiling() {
                    // Routine re-measurement. Nudge the ceiling up a step first, so a settled
                    // lease can rediscover room that opened up without waiting for demand to
                    // spike — otherwise the first narrow result would be permanent.
                    let reopened = lease.discovered_max();
                    let step = (reopened / 8).max(1);
                    lease.discovered_max.store(reopened.saturating_add(step), Ordering::Release);
                }
                continue;
            }

            if !lease.reports_work() {
                continue;
            }

            let is_probing_this = probing.as_ref().is_some_and(|p| Arc::ptr_eq(p, lease));

            if lease.demand() < Self::PROBE_MIN_DEMAND {
                // Went quiet mid-probe: the measurement is no longer about cores. Abandon it
                // rather than drawing a conclusion from it.
                if probe.phase != 0 {
                    probe.phase = 0;
                    probe.forced_grant = 0;
                    if is_probing_this {
                        *probing = None;
                    }
                }
                continue;
            }

            if probing.is_some() && !is_probing_this {
                continue;
            }

            match probe.phase {
                0 => {
                    if lease.granted() <= lease.min_cores() {
                        continue; // nothing to give back
                    }
                    *probing = Some(Arc::clone(lease));
                    probe.phase = 1;
                    Self::start_window(lease, &mut probe, now);
                }
                1 => {
                    if !Self::window_elapsed(&probe, now, window_ms) {
                        continue;
                    }
                    probe.baseline_rate = Self::rate_over(lease, &probe);
                    probe.baseline_grant = lease.granted();
                    lease.discovered_max.store(probe.baseline_grant, Ordering::Release);

                    let from = probe.baseline_grant;
                    if !Self::try_narrow(lease, &mut probe, from, now) {
                        Self::settle_probe(lease, &mut probe, probing);
                    }
                }
                _ => {
                    if !Self::window_elapsed(&probe, now, window_ms) {
                        continue;
                    }
                    let rate = Self::rate_over(lease, &probe);
                    let held = probe.baseline_rate <= 0.0
                        || rate >= probe.baseline_rate * (1.0 - Self::PROBE_TOLERANCE);

                    if held {
                        // This width is enough. Record it and keep taking cores away until it
                        // is not — the point of the search is the smallest width that still
                        // delivers, so the rest can go to a lease that will use them.
                        lease.discovered_max.store(probe.forced_grant, Ordering::Release);
                        let from = probe.forced_grant;
                        if !Self::try_narrow(lease, &mut probe, from, now) {
                            Self::settle_probe(lease, &mut probe, probing);
                        }
                    } else {
                        // Too far. discovered_max still holds the last width that delivered.
                        Self::settle_probe(lease, &mut probe, probing);
                    }
                }
            }
        }
    }

    fn window_elapsed(probe: &ProbeState, now: Instant, window_ms: f64) -> bool {
        match probe.window_start {
            Some(start) => now.saturating_duration_since(start).as_secs_f64() * 1000.0 >= window_ms,
            None => true,
        }
    }

    /// Work per millisecond the pool was actually busy — not per millisecond of wall clock.
    fn rate_over(lease: &BasisCoreLease, probe: &ProbeState) -> f64 {
        let busy_ms = lease.busy_micros_total().saturating_sub(probe.window_start_busy_micros) as f64 / 1000.0;
        if busy_ms <= 0.0 {
            return 0.0;
        }
        lease.work_total().saturating_sub(probe.window_start_work) as f64 / busy_ms
    }

    /// Holds the lease one step narrower and starts a fresh measurement window.
    fn try_narrow(lease: &BasisCoreLease, probe: &mut ProbeState, from: i32, now: Instant) -> bool {
        let step = (from / 8).max(1);
        let narrowed = from - step;
        if narrowed < lease.min_cores() {
            return false;
        }
        probe.forced_grant = narrowed;
        probe.phase = 2;
        Self::start_window(lease, probe, now);
        true
    }

    fn start_window(lease: &BasisCoreLease, probe: &mut ProbeState, now: Instant) {
        probe.window_start = Some(now);
        probe.window_start_work = lease.work_total();
        probe.window_start_busy_micros = lease.busy_micros_total();
    }

    fn settle_probe(lease: &Arc<BasisCoreLease>, probe: &mut ProbeState, probing: &mut Option<Arc<BasisCoreLease>>) {
        probe.phase = 0;
        probe.forced_grant = 0;
        probe.cooldown_steps = Self::PROBE_COOLDOWN_STEPS;
        probe.demand_at_settle = lease.demand();
        if probing.as_ref().is_some_and(|p| Arc::ptr_eq(p, lease)) {
            *probing = None;
        }
    }

    /// Hands the machine out to the registered leases. Called on the
    /// [`REBALANCE_INTERVAL_MS`](Self::REBALANCE_INTERVAL_MS) cadence.
    ///
    /// Floors first, then the remainder by weighted demand, then whatever a lease could not accept
    /// because it hit its own ceiling is offered back to the leases that still have room.
    pub fn rebalance() {
        Self::ensure_standing();
        Self::rebalance_inner();
    }

    fn rebalance_inner() {
        let _guard = REGISTRY.rebalance.lock();
        let leases: Vec<Arc<BasisCoreLease>> = REGISTRY.leases.read().clone();
        let count = leases.len();
        if count == 0 {
            return;
        }

        {
            let mut probing = REGISTRY.probing.lock();
            Self::drive_discovery(&leases, &mut probing);
        }

        let mut pool = Self::total_cores() - Self::reserved_cores();
        let count_i32 = i32::try_from(count).unwrap_or(i32::MAX);
        if pool < count_i32 {
            pool = count_i32; // every lease gets at least one core
        }

        let mut want = vec![0i32; count];
        let mut max = vec![0i32; count];
        let mut weight = vec![0f64; count];
        let mut forced = vec![false; count];

        let mut assigned: i64 = 0;
        for (i, lease) in leases.iter().enumerate() {
            // A lease under measurement is pinned to exactly the width being tested — the
            // experiment is that it runs at that width for a whole window, so it claims its
            // share up front rather than converging on it.
            let forced_grant = lease.forced_grant();
            forced[i] = forced_grant > 0;
            max[i] = if forced[i] { forced_grant } else { lease.effective_max() };
            want[i] = if forced[i] { max[i] } else { lease.min_cores() };
            if want[i] > max[i] {
                want[i] = max[i];
            }
            assigned += i64::from(want[i]);

            // Demand is added to the resting weight rather than replacing it, so with no signal
            // the allocator returns to the measured split instead of inventing one.
            weight[i] = lease.weight() * (1.0 + Self::PRESSURE_GAIN * lease.demand());
        }

        // Floors alone can exceed a small host. Trim the largest claims until they fit rather
        // than letting the sum run over — on a 2-core box the invariant matters most.
        while assigned > i64::from(pool) {
            let mut biggest = 0;
            for i in 1..count {
                if want[i] > want[biggest] {
                    biggest = i;
                }
            }
            if want[biggest] <= 1 {
                break;
            }
            want[biggest] -= 1;
            assigned -= 1;
        }

        let mut remaining = i64::from(pool) - assigned;

        // Repeat because clamping at a ceiling frees cores that the earlier passes already
        // apportioned; each pass redistributes only what the previous one could not place.
        let mut pass = 0;
        while pass < 8 && remaining > 0 {
            pass += 1;
            let mut active_weight = 0.0;
            for i in 0..count {
                if want[i] < max[i] {
                    active_weight += weight[i];
                }
            }
            if active_weight <= 0.0 {
                break;
            }

            let mut handed_out: i64 = 0;
            for i in 0..count {
                let room = i64::from(max[i]) - i64::from(want[i]);
                if room <= 0 {
                    continue;
                }
                let mut give = (remaining as f64 * (weight[i] / active_weight)) as i64;
                if give > room {
                    give = room;
                }
                if give <= 0 {
                    continue;
                }
                want[i] = want[i].saturating_add(i32::try_from(give).unwrap_or(i32::MAX));
                handed_out += give;
            }

            if handed_out == 0 {
                // Integer truncation stalled the split with cores still unplaced. Give the
                // remainder one at a time to the hungriest lease that can still take it.
                let mut best: Option<usize> = None;
                for i in 0..count {
                    if want[i] >= max[i] {
                        continue;
                    }
                    if best.is_none_or(|b| weight[i] > weight[b]) {
                        best = Some(i);
                    }
                }
                let Some(best) = best else { break };
                want[best] += 1;
                handed_out = 1;
            }

            remaining -= handed_out;
        }

        for (i, lease) in leases.iter().enumerate() {
            let target = want[i];

            // Measurement takes effect at once. Easing into the width under test would spend
            // the first part of the window running at the old one and measure a blend of both.
            if forced[i] {
                lease.grant(if target < 1 { 1 } else { target });
                continue;
            }

            let current = lease.granted();
            let mut next = current + ((f64::from(target) - f64::from(current)) * Self::DAMPING).round() as i32;

            // Damping must not round to a standstill, or a lease one core away from its target
            // never arrives.
            if next == current && target != current {
                next += if target > current { 1 } else { -1 };
            }
            if next < 1 {
                next = 1;
            }
            if next > max[i] {
                next = max[i];
            }
            lease.grant(next);
        }
    }

    /// Reports how far behind each pool is, 0..1. Feeds the next [`rebalance`](Self::rebalance).
    ///
    /// Demand only moves a lease inside its own bounds: a pool that is busy for reasons cores
    /// cannot fix stays where its ceiling puts it, and the cores it cannot use go to a lease that
    /// can.
    pub fn report_pressure(reduction_pressure: f64, peer_update_pressure: f64) {
        let (reduction, peer_update) = &*STANDING;
        reduction.report_demand(reduction_pressure);
        peer_update.report_demand(peer_update_pressure);
    }

    /// Worker ceiling for the reduction system's parallel phases.
    pub fn reduction_send_cap() -> i32 {
        STANDING.0.granted()
    }

    /// Worker ceiling for the transport's per-peer update pass.
    pub fn peer_update_cap() -> i32 {
        STANDING.1.granted()
    }

    /// The reduction system's lease. Report work to it so its ceiling gets measured.
    pub fn reduction_send_lease() -> &'static Arc<BasisCoreLease> {
        &STANDING.0
    }

    /// The transport's per-peer lease. Report work to it so its ceiling gets measured.
    pub fn peer_update_lease() -> &'static Arc<BasisCoreLease> {
        &STANDING.1
    }

    /// Current grants plus the demand that produced them, for diagnostics.
    pub fn describe_live() -> String {
        let leases = Self::leases();
        let mut out = String::new();
        for (i, lease) in leases.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{} {} (load {:.2})", lease.name(), lease.granted(), lease.demand()));
        }
        out
    }

    /// One line describing the pool, for the boot log.
    pub fn describe() -> String {
        let leases = Self::leases();
        let mut out = format!(
            "{} cores, {} reserved for dedicated threads: ",
            Self::total_cores(),
            Self::reserved_cores()
        );
        for (i, lease) in leases.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{} {}/{}", lease.name(), lease.granted(), lease.effective_max()));
            // Say which ceilings were measured and which are still the shipped guess.
            out.push_str(if lease.has_measured_ceiling() { " (measured)" } else { " (declared)" });
        }
        out.push_str(&format!(
            " (floor {} each; grants move with load and pools grow with population up to their grant)",
            Self::min_workers_per_pool()
        ));
        out
    }
}

/// CPU time this process has consumed, user plus system, in microseconds. `None` where the
/// platform will not report it, which the caller treats as "keep the last utilisation sample".
#[cfg(unix)]
fn process_cpu_micros() -> Option<u64> {
    // SAFETY: getrusage writes into the zeroed struct we hand it and reads nothing else.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    if rc != 0 {
        return None;
    }
    let to_micros = |tv: libc::timeval| -> u64 {
        u64::try_from(tv.tv_sec).unwrap_or(0).saturating_mul(1_000_000)
            + u64::try_from(tv.tv_usec).unwrap_or(0)
    };
    Some(to_micros(usage.ru_utime).saturating_add(to_micros(usage.ru_stime)))
}

#[cfg(not(unix))]
fn process_cpu_micros() -> Option<u64> {
    None
}
