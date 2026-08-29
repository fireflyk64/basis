//! `BasisServerReductionSystemEvents.LoadControl.cs`: the CPU-budget rebalance, the drop-rate
//! sampler and the load controller's constants.

use std::sync::atomic::Ordering;

use basis_network_core::configuration::BasisPopulationScale;
use basis_network_core::{BNL, BasisCpuBudget};

use super::tick::TickState;
use super::{BasisServerReductionSystemEvents, S};
use crate::NetworkServer;
use crate::diagnostics::BasisNetworkUdpDropMonitor;

impl BasisServerReductionSystemEvents {
    /// Minimum receivers per distance slice. Below roughly this the parallel dispatch costs more
    /// than the work it schedules.
    pub(super) const MIN_DISTANCE_SLICE_RECEIVERS: usize = 128;
    /// Overrun-ratio control window, in ticks. Short enough to respond within a second, long
    /// enough that one stall cannot flip it.
    pub(super) const TICK_CONTROL_WINDOW: i32 = 16;
    pub(super) const OVERRUN_ESCALATE_RATIO: f64 = 0.25;
    pub(super) const OVERRUN_RECOVER_RATIO: f64 = 0.05;
    pub(super) const OVERRUN_PANIC_RATIO: f64 = 0.75;
    pub(super) const PANIC_ESCALATION_STEPS: i32 = 4;
    /// Escalate above one lost packet per player per control window, recover below an eighth.
    pub(super) const DROP_ESCALATE_PER_PLAYER: f64 = 1.0;
    pub(super) const DROP_RECOVER_PER_PLAYER: f64 = 0.125;
    /// Tier 3 would mean "High only" — nobody past ~10 m updates — so the cap is 2.
    pub(super) const MAX_LOAD_SHED_TIER: i32 = 2;
    const POOL_LOAD_LOG_INTERVAL_TICKS: i64 = 15_000 * 1000;
    const SOCKET_GROW_SETTLE_TICKS: i64 = 5_000 * 1000;
    const SOCKET_PROBE_WINDOW_TICKS: i64 = 30_000 * 1000;
    const SEND_PRESSURE_STREAK_TO_GROW: i32 = 20;

    pub(super) fn max_slice_count() -> i32 {
        BasisPopulationScale::slice_cap(
            NetworkServer::configuration().map(|c| c.bsr_max_slice_count).unwrap_or(0),
            NetworkServer::server().map(|s| s.connected_peers_count()).unwrap_or(0),
        )
    }

    pub(super) fn rebalance_cpu_budget(tick: &mut TickState, now_tick: i64) {
        if now_tick - tick.last_rebalance_tick < Self::REBALANCE_INTERVAL_TICKS {
            return;
        }
        tick.last_rebalance_tick = now_tick;

        // The iroh transport services peers on its own runtime; it reports no peer-update
        // pressure of its own, so the send pool is the only pool the allocator balances here.
        let peer_pressure = 0.0;
        BasisCpuBudget::report_pressure(tick.tick_duty_ema, peer_pressure);
        BasisCpuBudget::rebalance();
        let util = BasisCpuBudget::sample_utilization();
        Self::maybe_grow_send_sockets(tick, now_tick, util);

        // Say which pool is hot, periodically.
        if Self::write_load_log() && now_tick - tick.last_pool_load_log_tick >= Self::POOL_LOAD_LOG_INTERVAL_TICKS {
            tick.last_pool_load_log_tick = now_tick;
            let pop = NetworkServer::server().map(|s| s.connected_peers_count()).unwrap_or(0);
            BNL::log(format!(
                "[CPU/POP] {pop} peers | send {}/{} wkr ({:.0} pairs/wkr-ms, budget {:.2}), machine {:.0}% of {} cores | drops {:.2}/player (esc {:.2}), slice {}/{}, tier {} {}",
                Self::send_workers(),
                BasisCpuBudget::reduction_send_cap(),
                Self::pairs_per_worker_ms(),
                Self::send_budget_duty(),
                BasisCpuBudget::utilization() * 100.0,
                BasisCpuBudget::total_cores(),
                tick.drops_per_player_window,
                Self::DROP_ESCALATE_PER_PLAYER,
                tick.slice_count,
                Self::max_slice_count(),
                tick.load_shed_tier,
                Self::load_shed_tier_name(tick.load_shed_tier)
            ));
        }
    }

    fn sample_drop_rate(tick: &mut TickState) {
        let total = BasisNetworkUdpDropMonitor::total_receive_buffer_drops();
        if tick.last_drop_total < 0 {
            tick.last_drop_total = total;
            return;
        }
        let per_second = (total - tick.last_drop_total) as f64 * (1000.0 / f64::from(BasisCpuBudget::REBALANCE_INTERVAL_MS));
        tick.last_drop_total = total;
        // Slow, because the source counter advances in 10 s steps.
        const ALPHA: f64 = 0.0033;
        tick.drop_rate_ema += (per_second - tick.drop_rate_ema) * ALPHA;
    }

    /// The LiteNetLib server grew SO_REUSEPORT send sockets under pressure. The iroh endpoint has
    /// one socket per address family; the pressure detection is kept so the operator is told,
    /// once, when the network path is what limits the instance.
    fn maybe_grow_send_sockets(tick: &mut TickState, now_tick: i64, utilization: f64) {
        Self::sample_drop_rate(tick);
        if Self::max_send_sockets() <= 1 {
            return;
        }
        let _ = (Self::SOCKET_GROW_SETTLE_TICKS, Self::SOCKET_PROBE_WINDOW_TICKS, now_tick);
        let (under_pressure, receive_dropping) = Self::network_path_under_pressure(tick, utilization);
        if !under_pressure {
            tick.send_pressure_streak = 0;
            return;
        }
        if receive_dropping {
            tick.send_pressure_streak = Self::SEND_PRESSURE_STREAK_TO_GROW;
        }
        tick.send_pressure_streak += 1;
        if tick.send_pressure_streak < Self::SEND_PRESSURE_STREAK_TO_GROW {
            return;
        }
        tick.send_pressure_streak = 0;
        Self::warn_socket_growth_unavailable(tick);
    }

    /// Whether the evidence for another send path is present: receive-side drops, or a pinned
    /// send pool behind on its tick with cores to spare.
    fn network_path_under_pressure(tick: &TickState, utilization: f64) -> (bool, bool) {
        let receive_dropping = tick.drop_rate_ema > 0.0;
        let send_pool_pinned = Self::send_workers() >= BasisCpuBudget::reduction_send_cap();
        let tick_behind = tick.tick_overrun_ratio > Self::OVERRUN_ESCALATE_RATIO || tick.slice_count > 1;
        let machine_has_room = utilization > 0.0 && utilization < 0.80;
        let send_path_limited = send_pool_pinned && tick_behind && machine_has_room;
        (send_path_limited || receive_dropping, receive_dropping)
    }

    fn warn_socket_growth_unavailable(tick: &mut TickState) {
        if tick.growth_unavailable_warned {
            return;
        }
        tick.growth_unavailable_warned = true;
        BNL::log_warning(
            "[CPU] The network path is what limits this server. The iroh transport drives one QUIC endpoint per address family, so no send socket can be added at runtime: raise net.core.rmem_max / net.core.wmem_max (Linux clamps the socket buffers to them), and spread the population across instances if the link itself is saturated.",
        );
    }

    pub(super) fn publish_load_state(tick: &TickState) {
        S.tick_ms_ema.set(tick.tick_ms_ema);
        S.tick_overrun_ratio.set(tick.tick_overrun_ratio);
        S.load_shed_tier.store(tick.load_shed_tier, Ordering::Relaxed);
        S.slice_count.store(tick.slice_count, Ordering::Relaxed);
    }
}
