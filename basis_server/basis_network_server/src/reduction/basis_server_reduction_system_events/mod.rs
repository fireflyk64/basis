//! Port of the `BasisServerReductionSystemEvents` partial class: the avatar reduction system.
//! Each C# partial file is a submodule with an `impl` block of its own.

mod bundling;
mod distance;
mod inbound;
mod load_control;
mod message_processing;
mod parallelism;
mod send_loop;
mod serialization;
mod test_seams;
mod tick;

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Once};
use std::time::Instant;

use basis_network_core::SerializableBasis::LocalAvatarSyncMessage;
use basis_network_core::compression::BasisAvatarBundleZstd;
use basis_network_core::mathematics::Vector3;
use basis_network_core::{BNL, BasisNetworkCommons};
use crossbeam_queue::SegQueue;
use dashmap::DashMap;
use parking_lot::{Condvar, Mutex, RwLock};

use crate::reduction::{PlayerState, QueuedMessage, ShardedConcurrentDictionary};

pub use distance::DistanceSweepState;
pub use parallelism::PoolTuning;

pub struct BasisServerReductionSystemEvents;

/// The active roster snapshot: `(player id, state)` in join order.
pub type Roster = Arc<[(i32, Arc<PlayerState>)]>;

/// Tick units are microseconds from process start; `MS_TO_TICK` converts milliseconds.
pub const MS_TO_TICK: f64 = 1000.0;

static START: LazyLock<Instant> = LazyLock::new(Instant::now);

pub fn now_ticks() -> i64 {
    START.elapsed().as_micros() as i64
}

pub(super) struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub(super) const fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    pub(super) fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub(super) fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

pub(super) struct AtomicF64(AtomicU64);

impl AtomicF64 {
    pub(super) const fn new(v: f64) -> Self {
        Self(AtomicU64::new(v.to_bits()))
    }
    pub(super) fn get(&self) -> f64 {
        f64::from_bits(self.0.load(Ordering::Relaxed))
    }
    pub(super) fn set(&self, v: f64) {
        self.0.store(v.to_bits(), Ordering::Relaxed);
    }
}

pub(super) struct UplinkDeltaState {
    pub baseline: Vec<u8>,
    pub baseline_seq: u8,
    pub has: bool,
    pub last_nack_ticks: i64,
}

/// Everything the C# kept in static fields.
pub(super) struct Statics {
    pub player_states: ShardedConcurrentDictionary<Arc<PlayerState>>,
    /// Admin-flagged full-quality broadcast ids. Authoritative across PlayerState recreation.
    pub bypass_reduction_ids: DashMap<i32, ()>,
    /// Inbound avatar frames, keyed by sender so only the newest per peer survives to the tick.
    pub current_messages: ShardedConcurrentDictionary<QueuedMessage>,
    pub active_players: Mutex<Vec<(i32, Arc<PlayerState>)>>,
    pub active_players_snapshot: RwLock<Roster>,
    pub active_players_dirty: AtomicBool,
    pub active_player_count: AtomicI32,
    pub players_to_remove: SegQueue<i32>,
    pub tick_wake: Mutex<bool>,
    pub tick_wake_signal: Condvar,
    pub uplink_states: DashMap<i32, Mutex<UplinkDeltaState>>,
    /// (senderId, receiverId) pairs whose baseline must be invalidated so the next send to that
    /// receiver is a keyframe.
    pub pending_keyframe_requests: SegQueue<(i32, i32)>,
    pub shutdown: AtomicBool,

    // ── Configuration (written from NetworkServer::initialize_pulse_settings) ──
    pub high_distance_sq: AtomicF32,
    pub medium_distance_sq: AtomicF32,
    pub low_distance_sq: AtomicF32,
    pub interval_ms: AtomicI64,
    pub max_spin_ms: AtomicF64,
    pub bsr_base_multiplier: AtomicF32,
    pub bsrs_increase_rate: AtomicF32,
    pub bsrs_millisecond_default_interval: AtomicI32,
    pub enable_avatar_bundle_compression: AtomicBool,
    pub avatar_bundle_min_messages: AtomicI32,
    pub avatar_bundle_min_bytes: AtomicI32,
    pub enable_avatar_bundle_zstd: AtomicBool,
    pub avatar_bundle_zstd_delta_bundles: AtomicBool,
    pub avatar_bundle_zstd_level: AtomicI32,
    pub avatar_bundle_zstd_max_shed_tier: AtomicI32,
    pub enable_avatar_delta_compression: AtomicBool,
    pub avatar_delta_keyframe_interval_ms: AtomicI32,
    pub avatar_delta_keyframe_max_interval_ms: AtomicI32,
    pub strip_additional_data_at_low_quality: AtomicBool,
    pub avatar_bundle_max_ratio: AtomicF32,
    pub avatar_bundle_reprobe_flushes: AtomicI32,
    pub enable_compute_offload: AtomicBool,
    pub compute_device: RwLock<String>,
    pub compute_distance_update_interval_ticks: AtomicI32,
    pub distance_update_interval_ticks: AtomicI32,
    pub load_shedding_enabled: AtomicBool,
    pub write_load_log: AtomicBool,
    pub max_send_sockets: AtomicI32,

    // ── Published diagnostics ──
    pub tick_ms_ema: AtomicF64,
    pub tick_overrun_ratio: AtomicF64,
    pub load_shed_tier: AtomicI32,
    pub slice_count: AtomicI32,
    pub distance_backend: RwLock<String>,
}

pub(super) static S: LazyLock<Statics> = LazyLock::new(|| Statics {
    player_states: ShardedConcurrentDictionary::default(),
    bypass_reduction_ids: DashMap::new(),
    current_messages: ShardedConcurrentDictionary::default(),
    active_players: Mutex::new(Vec::new()),
    active_players_snapshot: RwLock::new(Arc::from(Vec::new())),
    active_players_dirty: AtomicBool::new(false),
    active_player_count: AtomicI32::new(0),
    players_to_remove: SegQueue::new(),
    tick_wake: Mutex::new(false),
    tick_wake_signal: Condvar::new(),
    uplink_states: DashMap::new(),
    pending_keyframe_requests: SegQueue::new(),
    shutdown: AtomicBool::new(false),
    high_distance_sq: AtomicF32::new(100.0),
    medium_distance_sq: AtomicF32::new(900.0),
    low_distance_sq: AtomicF32::new(2500.0),
    interval_ms: AtomicI64::new(10),
    max_spin_ms: AtomicF64::new(2.5),
    bsr_base_multiplier: AtomicF32::new(1.0),
    bsrs_increase_rate: AtomicF32::new(0.01),
    bsrs_millisecond_default_interval: AtomicI32::new(50),
    enable_avatar_bundle_compression: AtomicBool::new(true),
    avatar_bundle_min_messages: AtomicI32::new(2),
    avatar_bundle_min_bytes: AtomicI32::new(128),
    enable_avatar_bundle_zstd: AtomicBool::new(true),
    avatar_bundle_zstd_delta_bundles: AtomicBool::new(false),
    avatar_bundle_zstd_level: AtomicI32::new(BasisAvatarBundleZstd::DEFAULT_LEVEL),
    avatar_bundle_zstd_max_shed_tier: AtomicI32::new(1),
    enable_avatar_delta_compression: AtomicBool::new(true),
    avatar_delta_keyframe_interval_ms: AtomicI32::new(500),
    avatar_delta_keyframe_max_interval_ms: AtomicI32::new(2000),
    strip_additional_data_at_low_quality: AtomicBool::new(true),
    avatar_bundle_max_ratio: AtomicF32::new(0.98),
    avatar_bundle_reprobe_flushes: AtomicI32::new(600),
    enable_compute_offload: AtomicBool::new(true),
    compute_device: RwLock::new(String::new()),
    compute_distance_update_interval_ticks: AtomicI32::new(32),
    distance_update_interval_ticks: AtomicI32::new(125),
    load_shedding_enabled: AtomicBool::new(true),
    write_load_log: AtomicBool::new(true),
    max_send_sockets: AtomicI32::new(8),
    tick_ms_ema: AtomicF64::new(0.0),
    tick_overrun_ratio: AtomicF64::new(0.0),
    load_shed_tier: AtomicI32::new(0),
    slice_count: AtomicI32::new(1),
    distance_backend: RwLock::new("cpu".to_string()),
});

static TICK_THREAD: Once = Once::new();

impl BasisServerReductionSystemEvents {
    /// Initial capacity for the per-receiver tracking table. Grows by doubling when a player id
    /// exceeds it.
    pub const INITIAL_PLAYER_ARRAY_CAPACITY: usize = 256;
    /// 4 ms (250 Hz) absolute floor and 20 ms (50 Hz) ceiling for the tick period.
    pub const MIN_TICK_INTERVAL_MS: i64 = 4;
    pub const MAX_TICK_INTERVAL_MS: i64 = 20;
    const TICKS_PER_SEND_INTERVAL: i64 = 4;
    /// Fallback wake while the server is empty; the wake signal does the real wake.
    const IDLE_WAIT_MS: u64 = 250;
    const RETAINED_SCRATCH_BYTES: usize = 16 * 1024;
    const PENDING_SHRINK_WINDOW_TICKS: usize = 256;
    const PENDING_MIN_CAPACITY: usize = 64;
    const INITIAL_BUNDLE_RATIO_GUESS: f32 = 0.85;
    const INITIAL_BUNDLE_ZSTD_RATIO_GUESS: f32 = 0.60;
    const MAX_BUNDLE_FILL_MARGIN: f32 = 0.95;
    const MIN_BUNDLE_FILL_MARGIN: f32 = 0.75;
    const BUNDLE_FILL_MARGIN_BACKOFF: f32 = 0.05;
    const BUNDLE_FILL_MARGIN_RECOVER: f32 = 0.01;
    const SMALL_HIGH_DELTA_BYTES: usize = 40;
    const SMALL_DELTA_STREAK_TO_STRETCH: i32 = 4;
    /// Headroom subtracted from the peer MTU before checking whether a bundle fits one datagram.
    const BUNDLE_MTU_HEADROOM: i32 = 32;
    /// Bundle wire header: `[flags:1][rawLen:2-LE]`.
    const BUNDLE_HEADER_SIZE: usize = 3;
    const MAX_REMOVALS_PER_TICK: usize = 8;

    /// Starts the background tick thread (the C# static constructor did this on first touch).
    pub fn ensure_started() {
        TICK_THREAD.call_once(|| {
            LazyLock::force(&START);
            let spawned = std::thread::Builder::new().name("BSR-TickLoop".to_string()).spawn(Self::background_tick_loop);
            if let Err(e) = spawned {
                BNL::log_error(format!("[BSR] the tick thread could not be started: {e}. Avatar sync is offline."));
            }
        });
    }

    pub fn shutdown() {
        S.shutdown.store(true, Ordering::Release);
        Self::wake_tick();
    }

    pub(super) fn wake_tick() {
        *S.tick_wake.lock() = true;
        S.tick_wake_signal.notify_one();
    }

    pub fn set_bypass_reduction(id: u16, enable: bool) {
        let id = i32::from(id);
        if enable {
            S.bypass_reduction_ids.insert(id, ());
        } else {
            S.bypass_reduction_ids.remove(&id);
        }
        if let Some(state) = S.player_states.get_cloned(id) {
            state.bypass_reduction.store(enable, Ordering::Relaxed);
        }
    }

    pub fn remove_player(id: i32) {
        S.players_to_remove.push(id);
    }

    pub fn player_state(id: i32) -> Option<Arc<PlayerState>> {
        S.player_states.get_cloned(id)
    }

    /// The live position of an active player, for relevance filters outside the tick.
    pub fn try_get_active_position(id: i32) -> Option<Vector3> {
        let state = S.player_states.get_cloned(id)?;
        state.is_active().then(|| state.position())
    }

    pub fn active_player_count() -> i32 {
        S.active_player_count.load(Ordering::Relaxed)
    }

    // ── Quality ────────────────────────────────────────────────────────────

    #[inline]
    pub(super) fn get_quality_index(dist_sq: f32) -> usize {
        if dist_sq <= S.high_distance_sq.get() {
            3
        } else if dist_sq <= S.medium_distance_sq.get() {
            2
        } else if dist_sq <= S.low_distance_sq.get() {
            1
        } else {
            0
        }
    }

    /// The tier a joiner at `viewer_position` should receive for `subject_id`, exactly as the
    /// steady-state send loop would pick.
    pub fn try_get_join_snapshot(viewer_position: Vector3, subject_id: i32) -> Option<LocalAvatarSyncMessage> {
        let subject = S.player_states.get_cloned(subject_id)?;
        let sender = subject.sender.lock();
        sender.avatar_high.array.as_ref()?;
        if subject.bypass_reduction() {
            return Some(sender.avatar_high.clone());
        }
        let p = subject.position();
        let dx = viewer_position.x - p.x;
        let dy = viewer_position.y - p.y;
        let dz = viewer_position.z - p.z;
        let dist_sq = dx * dx + dy * dy + dz * dz;
        let tier = match Self::get_quality_index(dist_sq) {
            3 => &sender.avatar_high,
            2 => &sender.avatar_medium,
            1 => &sender.avatar_low,
            _ => &sender.avatar_very_low,
        };
        Some(if tier.array.is_some() { tier.clone() } else { sender.avatar_high.clone() })
    }

    #[inline]
    pub(super) fn calculate_interval_from_distance_sq(distance_sq: f32) -> (u8, i32) {
        let base = S.bsrs_millisecond_default_interval.load(Ordering::Relaxed);
        let raw_interval = (base as f32 * (S.bsr_base_multiplier.get() + distance_sq * S.bsrs_increase_rate.get())) as i32;
        let offset_byte = BasisNetworkCommons::encode_avatar_interval_byte(raw_interval, base);
        (offset_byte, BasisNetworkCommons::decode_avatar_interval_ms(offset_byte, base))
    }

    // ── Configuration accessors (the C# public statics) ────────────────────

    pub fn bsrs_millisecond_default_interval() -> i32 {
        S.bsrs_millisecond_default_interval.load(Ordering::Relaxed)
    }
    pub fn set_bsrs_millisecond_default_interval(v: i32) {
        S.bsrs_millisecond_default_interval.store(v.max(1), Ordering::Relaxed);
    }
    pub fn bsr_base_multiplier() -> f32 {
        S.bsr_base_multiplier.get()
    }
    pub fn set_bsr_base_multiplier(v: f32) {
        S.bsr_base_multiplier.set(v);
    }
    pub fn bsrs_increase_rate() -> f32 {
        S.bsrs_increase_rate.get()
    }
    pub fn set_bsrs_increase_rate(v: f32) {
        S.bsrs_increase_rate.set(v);
    }
    pub fn high_distance_sq() -> f32 {
        S.high_distance_sq.get()
    }
    pub fn set_high_distance_sq(v: f32) {
        S.high_distance_sq.set(v);
    }
    pub fn medium_distance_sq() -> f32 {
        S.medium_distance_sq.get()
    }
    pub fn set_medium_distance_sq(v: f32) {
        S.medium_distance_sq.set(v);
    }
    pub fn low_distance_sq() -> f32 {
        S.low_distance_sq.get()
    }
    pub fn set_low_distance_sq(v: f32) {
        S.low_distance_sq.set(v);
    }
    pub fn interval_ms() -> i64 {
        S.interval_ms.load(Ordering::Relaxed)
    }
    pub fn max_spin_ms() -> f64 {
        S.max_spin_ms.get()
    }
    pub fn set_max_spin_ms(v: f64) {
        S.max_spin_ms.set(v);
    }
    pub fn enable_avatar_bundle_compression() -> bool {
        S.enable_avatar_bundle_compression.load(Ordering::Relaxed)
    }
    pub fn set_enable_avatar_bundle_compression(v: bool) {
        S.enable_avatar_bundle_compression.store(v, Ordering::Relaxed);
    }
    pub fn avatar_bundle_min_messages() -> i32 {
        S.avatar_bundle_min_messages.load(Ordering::Relaxed)
    }
    pub fn set_avatar_bundle_min_messages(v: i32) {
        S.avatar_bundle_min_messages.store(v, Ordering::Relaxed);
    }
    pub fn avatar_bundle_min_bytes() -> i32 {
        S.avatar_bundle_min_bytes.load(Ordering::Relaxed)
    }
    pub fn set_avatar_bundle_min_bytes(v: i32) {
        S.avatar_bundle_min_bytes.store(v, Ordering::Relaxed);
    }
    pub fn enable_avatar_bundle_zstd() -> bool {
        S.enable_avatar_bundle_zstd.load(Ordering::Relaxed)
    }
    pub fn set_enable_avatar_bundle_zstd(v: bool) {
        S.enable_avatar_bundle_zstd.store(v, Ordering::Relaxed);
    }
    pub fn avatar_bundle_zstd_delta_bundles() -> bool {
        S.avatar_bundle_zstd_delta_bundles.load(Ordering::Relaxed)
    }
    pub fn set_avatar_bundle_zstd_delta_bundles(v: bool) {
        S.avatar_bundle_zstd_delta_bundles.store(v, Ordering::Relaxed);
    }
    pub fn avatar_bundle_zstd_level() -> i32 {
        S.avatar_bundle_zstd_level.load(Ordering::Relaxed)
    }
    pub fn set_avatar_bundle_zstd_level(v: i32) {
        S.avatar_bundle_zstd_level.store(v, Ordering::Relaxed);
    }
    pub fn avatar_bundle_zstd_max_shed_tier() -> i32 {
        S.avatar_bundle_zstd_max_shed_tier.load(Ordering::Relaxed)
    }
    pub fn set_avatar_bundle_zstd_max_shed_tier(v: i32) {
        S.avatar_bundle_zstd_max_shed_tier.store(v, Ordering::Relaxed);
    }
    pub fn enable_avatar_delta_compression() -> bool {
        S.enable_avatar_delta_compression.load(Ordering::Relaxed)
    }
    pub fn set_enable_avatar_delta_compression(v: bool) {
        S.enable_avatar_delta_compression.store(v, Ordering::Relaxed);
    }
    pub fn avatar_delta_keyframe_interval_ms() -> i32 {
        S.avatar_delta_keyframe_interval_ms.load(Ordering::Relaxed)
    }
    pub fn set_avatar_delta_keyframe_interval_ms(v: i32) {
        S.avatar_delta_keyframe_interval_ms.store(v, Ordering::Relaxed);
    }
    pub fn avatar_delta_keyframe_max_interval_ms() -> i32 {
        S.avatar_delta_keyframe_max_interval_ms.load(Ordering::Relaxed)
    }
    pub fn set_avatar_delta_keyframe_max_interval_ms(v: i32) {
        S.avatar_delta_keyframe_max_interval_ms.store(v, Ordering::Relaxed);
    }
    pub fn strip_additional_data_at_low_quality() -> bool {
        S.strip_additional_data_at_low_quality.load(Ordering::Relaxed)
    }
    pub fn set_strip_additional_data_at_low_quality(v: bool) {
        S.strip_additional_data_at_low_quality.store(v, Ordering::Relaxed);
    }
    pub fn avatar_bundle_max_ratio() -> f32 {
        S.avatar_bundle_max_ratio.get()
    }
    pub fn set_avatar_bundle_max_ratio(v: f32) {
        S.avatar_bundle_max_ratio.set(v);
    }
    pub fn avatar_bundle_reprobe_flushes() -> i32 {
        S.avatar_bundle_reprobe_flushes.load(Ordering::Relaxed)
    }
    pub fn set_avatar_bundle_reprobe_flushes(v: i32) {
        S.avatar_bundle_reprobe_flushes.store(v, Ordering::Relaxed);
    }
    pub fn enable_compute_offload() -> bool {
        S.enable_compute_offload.load(Ordering::Relaxed)
    }
    pub fn set_enable_compute_offload(v: bool) {
        S.enable_compute_offload.store(v, Ordering::Relaxed);
    }
    pub fn compute_device() -> String {
        S.compute_device.read().clone()
    }
    pub fn set_compute_device(v: &str) {
        *S.compute_device.write() = v.to_string();
    }
    pub fn compute_distance_update_interval_ticks() -> i32 {
        S.compute_distance_update_interval_ticks.load(Ordering::Relaxed)
    }
    pub fn set_compute_distance_update_interval_ticks(v: i32) {
        S.compute_distance_update_interval_ticks.store(v.max(1), Ordering::Relaxed);
    }
    pub fn distance_update_interval_ticks() -> i32 {
        S.distance_update_interval_ticks.load(Ordering::Relaxed)
    }
    pub fn set_distance_update_interval_ticks(v: i32) {
        S.distance_update_interval_ticks.store(v.max(1), Ordering::Relaxed);
    }
    pub fn load_shedding_enabled() -> bool {
        S.load_shedding_enabled.load(Ordering::Relaxed)
    }
    pub fn set_load_shedding_enabled(v: bool) {
        S.load_shedding_enabled.store(v, Ordering::Relaxed);
    }
    pub fn write_load_log() -> bool {
        S.write_load_log.load(Ordering::Relaxed)
    }
    pub fn set_write_load_log(v: bool) {
        S.write_load_log.store(v, Ordering::Relaxed);
    }
    pub fn max_send_sockets() -> i32 {
        S.max_send_sockets.load(Ordering::Relaxed)
    }
    pub fn set_max_send_sockets(v: i32) {
        S.max_send_sockets.store(v, Ordering::Relaxed);
    }
    pub fn distance_backend() -> String {
        S.distance_backend.read().clone()
    }

    // ── Published diagnostics ──
    pub fn tick_ms_ema() -> f64 {
        S.tick_ms_ema.get()
    }
    pub fn tick_overrun_ratio() -> f64 {
        S.tick_overrun_ratio.get()
    }
    pub fn load_shed_tier() -> i32 {
        S.load_shed_tier.load(Ordering::Relaxed)
    }
    pub fn slice_count() -> i32 {
        S.slice_count.load(Ordering::Relaxed)
    }
    pub fn load_shed_tier_label() -> String {
        Self::load_shed_tier_name(Self::load_shed_tier()).to_string()
    }

    pub(super) fn load_shed_tier_name(tier: i32) -> &'static str {
        match tier {
            0 => "none",
            1 => "dropping VeryLow (furthest)",
            2 => "dropping VeryLow+Low",
            _ => "High only (nearest)",
        }
    }

    pub(super) fn adaptive_min_interval_ms() -> i64 {
        Self::MIN_TICK_INTERVAL_MS.max(i64::from(Self::bsrs_millisecond_default_interval()) / Self::TICKS_PER_SEND_INTERVAL)
    }

    /// The roster snapshot, rebuilt only when dirty.
    pub(super) fn active_players_snapshot() -> Arc<[(i32, Arc<PlayerState>)]> {
        if S.active_players_dirty.load(Ordering::Acquire) {
            let players = S.active_players.lock();
            if S.active_players_dirty.swap(false, Ordering::AcqRel) {
                *S.active_players_snapshot.write() = Arc::from(players.clone());
            }
        }
        S.active_players_snapshot.read().clone()
    }

    /// Drops every player and queued message. Tests and server stop.
    pub fn reset_for_tests() {
        S.player_states.clear();
        S.bypass_reduction_ids.clear();
        {
            let mut drained = Vec::new();
            S.current_messages.drain_into(&mut drained);
        }
        S.active_players.lock().clear();
        *S.active_players_snapshot.write() = Arc::from(Vec::new());
        S.active_players_dirty.store(false, Ordering::Release);
        S.active_player_count.store(0, Ordering::Relaxed);
        while S.players_to_remove.pop().is_some() {}
        while S.pending_keyframe_requests.pop().is_some() {}
        S.uplink_states.clear();
        S.load_shed_tier.store(0, Ordering::Relaxed);
        S.slice_count.store(1, Ordering::Relaxed);
    }
}
