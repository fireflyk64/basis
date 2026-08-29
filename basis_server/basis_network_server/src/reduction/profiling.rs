//! Port of `Reduction/Profiling.cs`: the lock-free BSR tick profiler.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Weak};

use basis_network_core::BNL;
use parking_lot::{Mutex, RwLock};

use crate::util::utc_now_iso8601;

/// One closed profiling window. Raw window totals only; derived figures are left to the caller
/// so every consumer computes them the same way.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BSRProfilerSnapshot {
    pub captured_utc: String,
    pub ticks: i64,
    pub messages: i64,
    pub sends: i64,
    pub pre_serializations: i64,
    pub pre_serializations_skipped: i64,
    pub drain_ms: f64,
    pub process_ms: f64,
    pub distance_ms: f64,
    pub update_ms: f64,
    pub trigger_ms: f64,
    pub total_ms: f64,
    pub bundles_emitted: i64,
    pub bundle_messages: i64,
    pub bundle_raw_bytes: i64,
    pub bundle_compressed_bytes: i64,
    pub bundle_deflate_ms: f64,
    pub bundle_retries: i64,
    pub bundle_fallbacks: i64,
    pub bundle_tail_uncompressed: i64,
    pub bundle_zstd_emitted: i64,
    pub bundle_zstd_raw_bytes: i64,
    pub bundle_zstd_compressed_bytes: i64,
    pub bundle_zstd_ms: f64,
}

/// One worker thread's counter block. Each worker accumulates into its own block (relaxed
/// atomics on a private cache line) and the window close sums the registered blocks, so the hot
/// path never contends.
#[repr(align(128))]
#[derive(Default)]
pub struct BSRThreadCounters {
    pub sends: AtomicI64,
    pub pre_serializations: AtomicI64,
    pub pre_serializations_skipped: AtomicI64,
    pub bundles_emitted: AtomicI64,
    pub bundle_messages: AtomicI64,
    pub bundle_raw_bytes: AtomicI64,
    pub bundle_compressed_bytes: AtomicI64,
    pub bundle_deflate_ticks: AtomicI64,
    pub bundle_retries: AtomicI64,
    pub bundle_fallbacks: AtomicI64,
    pub bundle_tail_uncompressed: AtomicI64,
    pub bundle_zstd_emitted: AtomicI64,
    pub bundle_zstd_raw_bytes: AtomicI64,
    pub bundle_zstd_compressed_bytes: AtomicI64,
    pub bundle_zstd_ticks: AtomicI64,
}

impl BSRThreadCounters {
    #[inline]
    pub fn add(counter: &AtomicI64, value: i64) {
        counter.fetch_add(value, Ordering::Relaxed);
    }

    fn take(counter: &AtomicI64) -> i64 {
        counter.swap(0, Ordering::Relaxed)
    }

    fn reset(&self) {
        for c in self.all() {
            c.store(0, Ordering::Relaxed);
        }
    }

    fn all(&self) -> [&AtomicI64; 15] {
        [
            &self.sends,
            &self.pre_serializations,
            &self.pre_serializations_skipped,
            &self.bundles_emitted,
            &self.bundle_messages,
            &self.bundle_raw_bytes,
            &self.bundle_compressed_bytes,
            &self.bundle_deflate_ticks,
            &self.bundle_retries,
            &self.bundle_fallbacks,
            &self.bundle_tail_uncompressed,
            &self.bundle_zstd_emitted,
            &self.bundle_zstd_raw_bytes,
            &self.bundle_zstd_compressed_bytes,
            &self.bundle_zstd_ticks,
        ]
    }
}

thread_local! {
    static THREAD_COUNTERS: Arc<BSRThreadCounters> = {
        let counters = Arc::new(BSRThreadCounters::default());
        ALL_COUNTERS.lock().push(Arc::downgrade(&counters));
        counters
    };
}

static ALL_COUNTERS: Mutex<Vec<Weak<BSRThreadCounters>>> = Mutex::new(Vec::new());
static ENABLED: AtomicBool = AtomicBool::new(false);
static WRITE_TO_LOG: AtomicBool = AtomicBool::new(false);
static LATEST: RwLock<Option<Arc<BSRProfilerSnapshot>>> = RwLock::new(None);
static LAST_PRINT_TICK: AtomicI64 = AtomicI64::new(0);

/// Static totals the windows drain into (tick units are µs).
#[derive(Default)]
struct Totals {
    drain_ticks: AtomicI64,
    process_ticks: AtomicI64,
    distance_ticks: AtomicI64,
    update_ticks: AtomicI64,
    trigger_ticks: AtomicI64,
    tick_count: AtomicI64,
    messages_processed: AtomicI64,
    send_count: AtomicI64,
    pre_serializations: AtomicI64,
    pre_serializations_skipped: AtomicI64,
    bundles_emitted: AtomicI64,
    bundle_messages: AtomicI64,
    bundle_raw_bytes: AtomicI64,
    bundle_compressed_bytes: AtomicI64,
    bundle_deflate_ticks: AtomicI64,
    bundle_retries: AtomicI64,
    bundle_fallbacks: AtomicI64,
    bundle_tail_uncompressed: AtomicI64,
    bundle_zstd_emitted: AtomicI64,
    bundle_zstd_raw_bytes: AtomicI64,
    bundle_zstd_compressed_bytes: AtomicI64,
    bundle_zstd_ticks: AtomicI64,
}

static TOTALS: LazyLock<Totals> = LazyLock::new(Totals::default);

/// Lock-free, low-overhead profiler for the BSR tick loop. Disabled by default; when disabled
/// every method is a flag check. Collects a window every 5 seconds, publishes it to
/// [`latest`](BSRProfiler::latest), and resets counters.
pub struct BSRProfiler;

impl BSRProfiler {
    /// Tick units are microseconds.
    pub const MS_TO_TICK: f64 = 1000.0;
    const PRINT_INTERVAL_TICKS: i64 = 5_000 * 1000;

    pub fn enabled() -> bool {
        ENABLED.load(Ordering::Relaxed)
    }

    pub fn set_enabled(enabled: bool) {
        ENABLED.store(enabled, Ordering::Relaxed);
    }

    /// Whether a closed window is written to the log. Collection is driven by `enabled` alone.
    pub fn write_to_log() -> bool {
        WRITE_TO_LOG.load(Ordering::Relaxed)
    }

    pub fn set_write_to_log(value: bool) {
        WRITE_TO_LOG.store(value, Ordering::Relaxed);
    }

    /// Most recently closed window, or None until the first one completes.
    pub fn latest() -> Option<Arc<BSRProfilerSnapshot>> {
        LATEST.read().clone()
    }

    /// This thread's counter block. Hoist the call once per receiver rather than per counter.
    pub fn local<R>(f: impl FnOnce(&BSRThreadCounters) -> R) -> R {
        THREAD_COUNTERS.with(|c| f(c))
    }

    pub fn add_drain_ticks(ticks: i64) {
        TOTALS.drain_ticks.fetch_add(ticks, Ordering::Relaxed);
    }
    pub fn add_process_ticks(ticks: i64) {
        TOTALS.process_ticks.fetch_add(ticks, Ordering::Relaxed);
    }
    pub fn add_distance_ticks(ticks: i64) {
        TOTALS.distance_ticks.fetch_add(ticks, Ordering::Relaxed);
    }
    pub fn add_update_ticks(ticks: i64) {
        TOTALS.update_ticks.fetch_add(ticks, Ordering::Relaxed);
    }
    pub fn add_trigger_ticks(ticks: i64) {
        TOTALS.trigger_ticks.fetch_add(ticks, Ordering::Relaxed);
    }
    pub fn add_tick(messages: i64) {
        TOTALS.tick_count.fetch_add(1, Ordering::Relaxed);
        TOTALS.messages_processed.fetch_add(messages, Ordering::Relaxed);
    }

    pub fn increment_pre_serializations() {
        if !Self::enabled() {
            return;
        }
        Self::local(|c| BSRThreadCounters::add(&c.pre_serializations, 1));
    }

    pub fn increment_pre_serializations_skipped() {
        if !Self::enabled() {
            return;
        }
        Self::local(|c| BSRThreadCounters::add(&c.pre_serializations_skipped, 1));
    }

    /// Drains every thread block into the static totals. A worker mid-increment just lands in
    /// the next window — these are diagnostics.
    fn drain_thread_counters() {
        let mut all = ALL_COUNTERS.lock();
        all.retain(|weak| weak.upgrade().is_some());
        for weak in all.iter() {
            let Some(c) = weak.upgrade() else {
                continue;
            };
            let t = &*TOTALS;
            t.send_count.fetch_add(BSRThreadCounters::take(&c.sends), Ordering::Relaxed);
            t.pre_serializations.fetch_add(BSRThreadCounters::take(&c.pre_serializations), Ordering::Relaxed);
            t.pre_serializations_skipped.fetch_add(BSRThreadCounters::take(&c.pre_serializations_skipped), Ordering::Relaxed);
            t.bundles_emitted.fetch_add(BSRThreadCounters::take(&c.bundles_emitted), Ordering::Relaxed);
            t.bundle_messages.fetch_add(BSRThreadCounters::take(&c.bundle_messages), Ordering::Relaxed);
            t.bundle_raw_bytes.fetch_add(BSRThreadCounters::take(&c.bundle_raw_bytes), Ordering::Relaxed);
            t.bundle_compressed_bytes.fetch_add(BSRThreadCounters::take(&c.bundle_compressed_bytes), Ordering::Relaxed);
            t.bundle_deflate_ticks.fetch_add(BSRThreadCounters::take(&c.bundle_deflate_ticks), Ordering::Relaxed);
            t.bundle_retries.fetch_add(BSRThreadCounters::take(&c.bundle_retries), Ordering::Relaxed);
            t.bundle_fallbacks.fetch_add(BSRThreadCounters::take(&c.bundle_fallbacks), Ordering::Relaxed);
            t.bundle_tail_uncompressed.fetch_add(BSRThreadCounters::take(&c.bundle_tail_uncompressed), Ordering::Relaxed);
            t.bundle_zstd_emitted.fetch_add(BSRThreadCounters::take(&c.bundle_zstd_emitted), Ordering::Relaxed);
            t.bundle_zstd_raw_bytes.fetch_add(BSRThreadCounters::take(&c.bundle_zstd_raw_bytes), Ordering::Relaxed);
            t.bundle_zstd_compressed_bytes.fetch_add(BSRThreadCounters::take(&c.bundle_zstd_compressed_bytes), Ordering::Relaxed);
            t.bundle_zstd_ticks.fetch_add(BSRThreadCounters::take(&c.bundle_zstd_ticks), Ordering::Relaxed);
        }
    }

    /// Closes the current window immediately instead of waiting out the interval. Tests.
    pub fn flush_window_for_tests(now_ticks: i64) {
        LAST_PRINT_TICK.store(now_ticks - Self::PRINT_INTERVAL_TICKS - 1, Ordering::Relaxed);
        Self::try_print(now_ticks);
    }

    /// Clears every counter, the published window, and both flags. Tests.
    pub fn reset_for_tests() {
        Self::set_enabled(false);
        Self::set_write_to_log(false);
        *LATEST.write() = None;
        {
            let mut all = ALL_COUNTERS.lock();
            all.retain(|weak| weak.upgrade().is_some());
            for weak in all.iter() {
                if let Some(c) = weak.upgrade() {
                    c.reset();
                }
            }
        }
        let t = &*TOTALS;
        for counter in [
            &t.drain_ticks,
            &t.process_ticks,
            &t.distance_ticks,
            &t.update_ticks,
            &t.trigger_ticks,
            &t.tick_count,
            &t.messages_processed,
            &t.send_count,
            &t.pre_serializations,
            &t.pre_serializations_skipped,
            &t.bundles_emitted,
            &t.bundle_messages,
            &t.bundle_raw_bytes,
            &t.bundle_compressed_bytes,
            &t.bundle_deflate_ticks,
            &t.bundle_retries,
            &t.bundle_fallbacks,
            &t.bundle_tail_uncompressed,
            &t.bundle_zstd_emitted,
            &t.bundle_zstd_raw_bytes,
            &t.bundle_zstd_compressed_bytes,
            &t.bundle_zstd_ticks,
        ] {
            counter.store(0, Ordering::Relaxed);
        }
    }

    pub fn try_print(now_ticks: i64) {
        if !Self::enabled() {
            return;
        }
        if now_ticks - LAST_PRINT_TICK.load(Ordering::Relaxed) < Self::PRINT_INTERVAL_TICKS {
            return;
        }
        LAST_PRINT_TICK.store(now_ticks, Ordering::Relaxed);

        // Fold per-thread blocks in before anything below reads the totals.
        Self::drain_thread_counters();
        let t = &*TOTALS;
        let ticks = t.tick_count.swap(0, Ordering::Relaxed);
        if ticks == 0 {
            return;
        }
        let msgs = t.messages_processed.swap(0, Ordering::Relaxed);
        let sends = t.send_count.swap(0, Ordering::Relaxed);
        let pre_ser = t.pre_serializations.swap(0, Ordering::Relaxed);
        let pre_skip = t.pre_serializations_skipped.swap(0, Ordering::Relaxed);
        let drain = t.drain_ticks.swap(0, Ordering::Relaxed) as f64 / Self::MS_TO_TICK;
        let process = t.process_ticks.swap(0, Ordering::Relaxed) as f64 / Self::MS_TO_TICK;
        let distance = t.distance_ticks.swap(0, Ordering::Relaxed) as f64 / Self::MS_TO_TICK;
        let update = t.update_ticks.swap(0, Ordering::Relaxed) as f64 / Self::MS_TO_TICK;
        let trigger = t.trigger_ticks.swap(0, Ordering::Relaxed) as f64 / Self::MS_TO_TICK;
        let total = drain + process + distance + update + trigger;

        let b_emit = t.bundles_emitted.swap(0, Ordering::Relaxed);
        let b_msg = t.bundle_messages.swap(0, Ordering::Relaxed);
        let b_raw = t.bundle_raw_bytes.swap(0, Ordering::Relaxed);
        let b_comp = t.bundle_compressed_bytes.swap(0, Ordering::Relaxed);
        let b_deflate = t.bundle_deflate_ticks.swap(0, Ordering::Relaxed);
        let b_retry = t.bundle_retries.swap(0, Ordering::Relaxed);
        let b_fallback = t.bundle_fallbacks.swap(0, Ordering::Relaxed);
        let b_tail = t.bundle_tail_uncompressed.swap(0, Ordering::Relaxed);
        let bz_emit = t.bundle_zstd_emitted.swap(0, Ordering::Relaxed);
        let bz_raw = t.bundle_zstd_raw_bytes.swap(0, Ordering::Relaxed);
        let bz_comp = t.bundle_zstd_compressed_bytes.swap(0, Ordering::Relaxed);
        let bz_ticks = t.bundle_zstd_ticks.swap(0, Ordering::Relaxed);

        let snapshot = BSRProfilerSnapshot {
            captured_utc: utc_now_iso8601(),
            ticks,
            messages: msgs,
            sends,
            pre_serializations: pre_ser,
            pre_serializations_skipped: pre_skip,
            drain_ms: drain,
            process_ms: process,
            distance_ms: distance,
            update_ms: update,
            trigger_ms: trigger,
            total_ms: total,
            bundles_emitted: b_emit,
            bundle_messages: b_msg,
            bundle_raw_bytes: b_raw,
            bundle_compressed_bytes: b_comp,
            bundle_deflate_ms: b_deflate as f64 / Self::MS_TO_TICK,
            bundle_retries: b_retry,
            bundle_fallbacks: b_fallback,
            bundle_tail_uncompressed: b_tail,
            bundle_zstd_emitted: bz_emit,
            bundle_zstd_raw_bytes: bz_raw,
            bundle_zstd_compressed_bytes: bz_comp,
            bundle_zstd_ms: bz_ticks as f64 / Self::MS_TO_TICK,
        };
        *LATEST.write() = Some(Arc::new(snapshot));

        if !Self::write_to_log() {
            return;
        }
        // One line, not eleven: every log call costs a console lock and a file write.
        let ticks_f = ticks as f64;
        let pct_of_total = if total > 0.0 { 100.0 / total } else { 0.0 };
        let phase = |name: &str, ms: f64| format!(" {name} {:.3} {:.0}%", ms / ticks_f, ms * pct_of_total);
        let mut line = format!("[BSR] {ticks}t {:.3}ms/t", total / ticks_f);
        line.push_str(&phase("drain", drain));
        line.push_str(&phase("proc", process));
        line.push_str(&phase("dist", distance));
        line.push_str(&phase("upd", update));
        line.push_str(&phase("trig", trigger));
        line.push_str(&format!(" | {msgs}msg {sends}send preser {pre_ser}/{}", pre_ser + pre_skip));
        if b_emit > 0 || b_tail > 0 || b_fallback > 0 {
            let ratio = if b_raw > 0 { b_comp as f64 / b_raw as f64 } else { 0.0 };
            let saved_pct = if b_raw > 0 { (ratio - 1.0) * 100.0 } else { 0.0 };
            let deflate_ms = b_deflate as f64 / Self::MS_TO_TICK;
            let per_bundle = if b_emit > 0 { 1.0 / b_emit as f64 } else { 0.0 };
            line.push_str(&format!(" | bundles {b_emit} {:.2}/t {b_msg}msg {b_tail}tail {b_fallback}fb", b_emit as f64 / ticks_f));
            line.push_str(&format!(
                " {:.1}msg/b {:.0}→{:.0}B {ratio:.3} {saved_pct:.1}% {:.1}KB",
                b_msg as f64 * per_bundle,
                b_raw as f64 * per_bundle,
                b_comp as f64 * per_bundle,
                (b_raw - b_comp) as f64 / 1024.0
            ));
            line.push_str(&format!(
                " deflate {:.3}ms/t {:.1}% {:.1}µs/b retry {b_retry} {:.1}%",
                deflate_ms / ticks_f,
                deflate_ms * pct_of_total,
                deflate_ms * 1000.0 * per_bundle,
                b_retry as f64 * per_bundle * 100.0
            ));
        }
        BNL::log(line);
    }
}
