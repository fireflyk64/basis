//! `BasisServerReductionSystemEvents.Tick.cs` + `Lifecycle.cs`: the tick thread, the per-tick
//! pipeline, pending removals, and the load controller.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use basis_network_core::{BNL, BasisCpuBudget};

use super::distance::DistanceSweepState;
use super::{BasisServerReductionSystemEvents, MS_TO_TICK, S, now_ticks};
use crate::NetworkServer;
use crate::handlers::BasisNetworkPIPCamera;
use crate::reduction::{BSRProfiler, PlayerState, QueuedMessage, SenderFrame};

/// Everything the tick thread owns. Nothing here is shared; the published diagnostics are
/// copied out to atomics at the end of each tick.
pub struct TickState {
    pub messages_snapshot: Vec<QueuedMessage>,
    pub generation_snapshot: Vec<i64>,
    pub frames: Vec<Arc<SenderFrame>>,
    pub distance: DistanceSweepState,
    pub slice_count: i32,
    pub slice_index: usize,
    pub sender_rotation: u32,
    pub tick_duty_ema: f64,
    pub last_send_pairs: i64,
    pub last_send_workers: i32,
    pub last_rebalance_tick: i64,
    pub last_pool_load_log_tick: i64,
    pub tick_ms_ema: f64,
    pub last_slice_log_tick: i64,
    pub load_legend_written: bool,
    pub tick_window_count: i32,
    pub tick_overrun_count: i32,
    pub tick_overrun_ratio: f64,
    pub tick_control_ready: bool,
    pub last_unreliable_dropped: i64,
    pub drop_baseline_ready: bool,
    pub drops_per_player_window: f64,
    pub load_shed_tier: i32,
    pub last_drop_total: i64,
    pub drop_rate_ema: f64,
    pub send_pressure_streak: i32,
    pub growth_unavailable_warned: bool,
}

impl Default for TickState {
    fn default() -> Self {
        Self {
            messages_snapshot: Vec::with_capacity(1024),
            generation_snapshot: vec![0; BasisServerReductionSystemEvents::INITIAL_PLAYER_ARRAY_CAPACITY],
            frames: Vec::new(),
            distance: DistanceSweepState::default(),
            slice_count: 1,
            slice_index: 0,
            sender_rotation: 0,
            tick_duty_ema: 0.0,
            last_send_pairs: 0,
            last_send_workers: 0,
            last_rebalance_tick: 0,
            last_pool_load_log_tick: 0,
            tick_ms_ema: 0.0,
            last_slice_log_tick: 0,
            load_legend_written: false,
            tick_window_count: 0,
            tick_overrun_count: 0,
            tick_overrun_ratio: 0.0,
            tick_control_ready: false,
            last_unreliable_dropped: 0,
            drop_baseline_ready: false,
            drops_per_player_window: 0.0,
            load_shed_tier: 0,
            last_drop_total: -1,
            drop_rate_ema: 0.0,
            send_pressure_streak: 0,
            growth_unavailable_warned: false,
        }
    }
}

impl BasisServerReductionSystemEvents {
    pub(super) fn background_tick_loop() {
        let mut tick = TickState::default();
        while !S.shutdown.load(Ordering::Acquire) {
            let start_tick = now_ticks();
            // One bad tick must not kill the thread: a panic here would otherwise freeze avatar
            // sync until restart.
            if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Self::run_tick(&mut tick, start_tick))) {
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_string())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "non-string panic payload".to_string());
                BNL::log_error(format!("[BSR Tick] Unhandled panic: {message}"));
            }

            // Empty server: park until work arrives instead of spinning.
            if S.active_player_count.load(Ordering::Relaxed) == 0 {
                Self::wait_for_wake(Duration::from_millis(Self::IDLE_WAIT_MS));
                continue;
            }
            // Load-adaptive wait: block when the budget has slack, spin the last stretch to hit
            // the rate precisely, and neither when saturated.
            let target_tick = start_tick + (Self::interval_ms() as f64 * MS_TO_TICK) as i64;
            let remain_ms = (target_tick - now_ticks()) as f64 / MS_TO_TICK;
            if remain_ms > Self::max_spin_ms() {
                Self::wait_for_wake(Duration::from_micros((remain_ms * 1000.0).round().max(0.0) as u64));
            } else {
                while now_ticks() < target_tick {
                    std::hint::spin_loop();
                }
            }
        }
    }

    fn wait_for_wake(timeout: Duration) {
        let mut woken = S.tick_wake.lock();
        if !*woken {
            S.tick_wake_signal.wait_for(&mut woken, timeout);
        }
        *woken = false;
    }

    pub(super) fn run_tick(tick: &mut TickState, start_tick: i64) {
        let profiling = BSRProfiler::enabled();
        let mut phase_tick = if profiling { now_ticks() } else { 0 };

        // Phase 1: Drain — take everything queued since the last tick, removing as we go.
        tick.messages_snapshot.clear();
        S.current_messages.drain_into(&mut tick.messages_snapshot);
        if profiling {
            BSRProfiler::add_drain_ticks(now_ticks() - phase_tick);
            phase_tick = now_ticks();
        }

        // Phase 2: Process messages. Range-partitioned so the scheduler splits evenly.
        let message_count = tick.messages_snapshot.len();
        if message_count > 0 {
            let messages: Vec<QueuedMessage> = std::mem::take(&mut tick.messages_snapshot);
            let slots: Vec<parking_lot::Mutex<Option<QueuedMessage>>> = messages.into_iter().map(|m| parking_lot::Mutex::new(Some(m))).collect();
            Self::parallel_for(0, message_count, |i| {
                if let Some(message) = slots[i].lock().take()
                    && std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Self::process_message(message))).is_err()
                {
                    BNL::log_error("[ProcessMessage] a frame panicked the processor; the frame was dropped");
                }
            });
        }
        if profiling {
            BSRProfiler::add_process_ticks(now_ticks() - phase_tick);
            phase_tick = now_ticks();
        }

        Self::process_pending_removals();
        Self::process_pending_keyframe_requests();

        // Phase 2.5: Distance cache update, amortized across the interval.
        let dist_start = if profiling { now_ticks() } else { 0 };
        let did_distance_work = Self::update_distance_cache_slice(tick);
        if profiling && did_distance_work {
            BSRProfiler::add_distance_ticks(now_ticks() - dist_start);
            phase_tick = now_ticks();
        }

        // Phase 3: Send loop
        let now = now_ticks();
        tick.last_send_pairs = 0;
        tick.last_send_workers = 0;
        Self::update_communication_and_distances(tick, now);
        let send_phase_ticks = now_ticks() - now;
        if profiling {
            BSRProfiler::add_update_ticks(now_ticks() - phase_tick);
            phase_tick = now_ticks();
        }
        // Pairs served per millisecond this phase was busy — the signal the core allocator uses.
        let send_phase_ms = send_phase_ticks as f64 / MS_TO_TICK;
        Self::note_send_pass_cost(tick.last_send_pairs, send_phase_ms, tick.last_send_workers);
        if tick.last_send_pairs > 0 {
            BasisCpuBudget::reduction_send_lease().add_work(tick.last_send_pairs, send_phase_ms);
        }

        // Phase 4: Network I/O
        BasisNetworkPIPCamera::update_pip_positions(now / 1000);
        if profiling {
            BSRProfiler::add_trigger_ticks(now_ticks() - phase_tick);
            BSRProfiler::add_tick(message_count as i64);
        }

        // Tick bookkeeping
        let elapsed_ms = (now_ticks() - start_tick) as f64 / MS_TO_TICK;
        BSRProfiler::try_print(now_ticks());
        Self::run_load_controller(tick, start_tick, elapsed_ms);
    }

    /// The load controller: three levers escalated in order of how much a player notices —
    /// tick period, shed tier, then slicing as the last resort. Recovery unwinds in reverse.
    fn run_load_controller(tick: &mut TickState, start_tick: i64, elapsed_ms: f64) {
        let interval_ms = Self::interval_ms();
        tick.tick_ms_ema = if tick.tick_ms_ema <= 0.0 { elapsed_ms } else { tick.tick_ms_ema * 0.9 + elapsed_ms * 0.1 };

        // Control signal: how OFTEN the period is missed, not the average time — bounded and
        // outlier-insensitive.
        tick.tick_window_count += 1;
        if elapsed_ms > interval_ms as f64 {
            tick.tick_overrun_count += 1;
        }
        // Duty cycle of this pool: work time against the period it is trying to hold.
        let duty = elapsed_ms / (interval_ms as f64).max(1.0);
        tick.tick_duty_ema = if tick.tick_duty_ema <= 0.0 { duty } else { tick.tick_duty_ema * 0.9 + duty * 0.1 };
        Self::rebalance_cpu_budget(tick, start_tick);

        if tick.tick_window_count >= Self::TICK_CONTROL_WINDOW {
            tick.tick_overrun_ratio = f64::from(tick.tick_overrun_count) / f64::from(tick.tick_window_count);
            tick.tick_window_count = 0;
            tick.tick_overrun_count = 0;
            tick.tick_control_ready = true;

            // Sample undeliverable packets over the same window, normalised per player.
            let transport = NetworkServer::server();
            let dropped_now = transport.as_ref().map(|t| t.unreliable_dropped()).unwrap_or(0);
            let population = transport.as_ref().map(|t| t.connected_peers_count()).unwrap_or(0);
            if !tick.drop_baseline_ready {
                tick.last_unreliable_dropped = dropped_now;
                tick.drop_baseline_ready = true;
                tick.drops_per_player_window = 0.0;
            } else {
                let delta = (dropped_now - tick.last_unreliable_dropped).max(0);
                tick.last_unreliable_dropped = dropped_now;
                tick.drops_per_player_window = if population > 0 { delta as f64 / f64::from(population) } else { 0.0 };
            }
        }
        Self::publish_load_state(tick);
        if !tick.tick_control_ready {
            return;
        }
        tick.tick_control_ready = false;

        let dropping_hard = tick.drops_per_player_window > Self::DROP_ESCALATE_PER_PLAYER;
        let delivering_cleanly = tick.drops_per_player_window < Self::DROP_RECOVER_PER_PLAYER;
        let overloaded = tick.tick_overrun_ratio > Self::OVERRUN_ESCALATE_RATIO || dropping_hard;
        let comfortable = tick.tick_overrun_ratio < Self::OVERRUN_RECOVER_RATIO && delivering_cleanly;
        let drop_panic = tick.drops_per_player_window > Self::DROP_ESCALATE_PER_PLAYER * 8.0;
        let escalation_steps = if tick.tick_overrun_ratio > Self::OVERRUN_PANIC_RATIO || drop_panic { Self::PANIC_ESCALATION_STEPS } else { 1 };
        let previous_slice_count = tick.slice_count;
        let previous_shed_tier = tick.load_shed_tier;
        let previous_interval = interval_ms;

        let mut interval = interval_ms;
        let adaptive_min = Self::adaptive_min_interval_ms();
        if interval < adaptive_min {
            interval = adaptive_min;
        }
        if overloaded && interval < Self::MAX_TICK_INTERVAL_MS {
            interval = Self::MAX_TICK_INTERVAL_MS.min(interval + 2 * i64::from(escalation_steps));
        } else if comfortable && interval > adaptive_min && tick.slice_count == 1 && tick.load_shed_tier == 0 {
            // Only tighten the period once nothing is being degraded.
            interval = adaptive_min.max(interval - 1);
        }

        if overloaded {
            // Shed the furthest pairs FIRST, and only fall back to slicing once even a
            // High-quality-only workload cannot fit the budget.
            if interval < Self::MAX_TICK_INTERVAL_MS {
                // Period is still stretching — give that a chance before degrading anything.
            } else if Self::load_shedding_enabled() && tick.load_shed_tier < Self::MAX_LOAD_SHED_TIER {
                tick.load_shed_tier = Self::MAX_LOAD_SHED_TIER.min(tick.load_shed_tier + escalation_steps);
            } else if tick.slice_count < Self::max_slice_count() {
                tick.slice_count = Self::max_slice_count().min(tick.slice_count + escalation_steps);
            }
        } else if comfortable {
            // Recover visibility BEFORE rate.
            if tick.load_shed_tier > 0 {
                tick.load_shed_tier -= 1;
            } else if tick.slice_count > 1 {
                tick.slice_count -= 1;
            }
        }
        S.interval_ms.store(interval, Ordering::Relaxed);
        Self::publish_load_state(tick);

        // Rate-limited: this is a health signal, not a per-change trace.
        if Self::write_load_log() && (tick.slice_count != previous_slice_count || tick.load_shed_tier != previous_shed_tier || interval != previous_interval) {
            let now_log = now_ticks();
            if now_log - tick.last_slice_log_tick > 5_000_000 {
                tick.last_slice_log_tick = now_log;
                if !tick.load_legend_written {
                    tick.load_legend_written = true;
                    BNL::log("[BSR] Load legend: period alone is harmless; tier > 0 means distant players stop updating; slicing > 1 means everyone's rate is reduced.");
                }
                BNL::log(format!(
                    "[BSR] Load: {:.0}% ticks over budget (mean {:.2} ms), period {interval} ms ({} Hz), tier {} {}, slicing {}",
                    tick.tick_overrun_ratio * 100.0,
                    tick.tick_ms_ema,
                    1000 / interval.max(1),
                    tick.load_shed_tier,
                    Self::load_shed_tier_name(tick.load_shed_tier),
                    tick.slice_count
                ));
            }
        }
    }

    pub(super) fn process_pending_removals() {
        let mut removals_this_tick = 0;
        while removals_this_tick < Self::MAX_REMOVALS_PER_TICK {
            let Some(id) = S.players_to_remove.pop() else {
                break;
            };
            removals_this_tick += 1;
            S.uplink_states.remove(&id);
            // Admin bypass is per-player-session; ids are recycled, so a stale entry would grant
            // the next player on this id full-quality broadcast.
            S.bypass_reduction_ids.remove(&id);
            match S.player_states.remove(id) {
                Some(removed_state) => {
                    removed_state.is_active.store(false, Ordering::Release);
                    {
                        let mut active = S.active_players.lock();
                        if let Some(position) = active.iter().rposition(|(pid, _)| *pid == id) {
                            active.remove(position);
                            S.active_players_dirty.store(true, Ordering::Release);
                            S.active_player_count.fetch_sub(1, Ordering::Relaxed);
                        }
                    }
                    // Clear stale per-player tracking for the removed id across all remaining
                    // players, so a reused id does not inherit a high last-seen generation.
                    if let Ok(index) = usize::try_from(id) {
                        S.player_states.for_each(|_, other: &Arc<PlayerState>| {
                            let mut recv = other.receiver.lock();
                            if let Some(slot) = recv.peer_tracking.get_mut(index) {
                                *slot = Default::default();
                            }
                        });
                    }
                    BNL::log(format!("Player {id} removed and cleaned up."));
                }
                None => BNL::log_error(format!("Missing Player From Index, Normally Quick Disconnect after Connect {id}")),
            }
        }
    }
}
