//! `BasisServerReductionSystemEvents.TestSeams.cs`: entry points that let tests drive one step of
//! the machinery without a live transport.

use std::sync::Arc;

use basis_network_core::BasisNetworkCommons;

use super::distance::DistanceSweepState;
use super::{BasisServerReductionSystemEvents, now_ticks};
use crate::reduction::{PlayerState, ReceiverData, SenderWork};

impl BasisServerReductionSystemEvents {
    pub fn test_only_pre_serialize_frame(state: &PlayerState, publish_gen: i64, force_keyframe: bool) {
        let mut sender = state.sender.lock();
        Self::pre_serialize_frame(state, &mut sender, publish_gen, force_keyframe);
    }

    /// The C# `PreSerializeKeyframe`: serializes one quality's keyframe wire into `sender`.
    pub fn test_only_pre_serialize_keyframe(state: &PlayerState, sender: &mut SenderWork, qi: usize, player_id: u16) {
        Self::pre_serialize_keyframe(state, sender, qi, player_id);
    }

    /// The C# `PreSerializeDelta`: serializes one quality's delta wire against the held baseline.
    pub fn test_only_pre_serialize_delta(state: &PlayerState, sender: &mut SenderWork, qi: usize, player_id: u16) {
        Self::pre_serialize_delta(state, sender, qi, player_id);
    }

    /// The C# `UpdateKeyframeStretch`: feeds one High delta length into the adaptive stretch.
    pub fn test_only_update_keyframe_stretch(sender: &mut SenderWork, high_delta_length: usize) {
        Self::update_keyframe_stretch(sender, high_delta_length);
    }

    /// The C# `PropagateAdditionalData`: copies (or strips) the High additional data into the
    /// lower tiers held by `sender`.
    pub fn test_only_propagate_additional_data(sender: &mut SenderWork) {
        Self::propagate_additional_data(sender);
    }

    pub fn test_only_build_raw_for_range(recv: &mut ReceiverData, start: usize, end: usize) -> usize {
        Self::build_raw_for_range(recv, start, end)
    }

    pub fn test_only_sort_pending_by_channel(recv: &mut ReceiverData, count: usize, sender_rotation: u32) {
        Self::sort_pending_by_channel(recv, count, sender_rotation);
    }

    /// Drives one widening verdict: pretends the send pool went `from` to `current` workers with
    /// the whole-pool rate moving as given, and returns the width left in force.
    pub fn test_only_resolve_widen_trial(from: i32, current: i32, rate_before: f64, rate_after: f64, player_count: i32) -> i32 {
        Self::set_send_workers(current);
        Self::with_pool_tuning(|t| {
            t.widen_trial_from = from;
            t.aggregate_rate_at_widen = rate_before;
            t.aggregate_rate_ema = rate_after;
            t.passes_since_widen = Self::WIDEN_TRIAL_PASSES;
            Self::resolve_widen_trial(t, current, player_count, now_ticks())
        })
    }

    pub fn test_only_learned_width_ceiling() -> i32 {
        Self::with_pool_tuning(|t| t.learned_width_ceiling)
    }

    pub fn test_only_send_workers() -> i32 {
        Self::send_workers()
    }

    pub fn test_only_expire_learned_ceiling(player_count: i32) {
        Self::with_pool_tuning(|t| Self::expire_learned_ceiling(t, now_ticks(), player_count));
    }

    /// Backdates the learned ceiling past its retry window.
    pub fn test_only_age_learned_ceiling() {
        Self::with_pool_tuning(|t| t.learned_ceiling_tick -= Self::LEARNED_CEILING_RETRY_TICKS + 1);
    }

    pub fn test_only_reset_pool_tuning(workers: i32) {
        Self::with_pool_tuning(|t| {
            t.widen_trial_from = 0;
            t.aggregate_rate_at_widen = 0.0;
            t.passes_since_widen = 0;
            t.aggregate_rate_ema = 0.0;
            t.learned_width_ceiling = 0;
            t.learned_ceiling_players = 0;
            t.learned_ceiling_send_cap = 0;
            t.learned_ceiling_tick = 0;
        });
        Self::set_send_workers(workers);
    }

    /// The refresh period the sweep would use with `distance`'s solver state: the faster device
    /// schedule only while a device is actually carrying the sweep.
    pub fn test_only_effective_distance_interval_ticks(distance: &DistanceSweepState) -> i32 {
        Self::effective_distance_interval_ticks(distance)
    }

    /// The send pool width the sizing would pick for `player_count` players from `current`.
    pub fn test_only_degree_for(player_count: i32, current: i32) -> i32 {
        Self::with_pool_tuning(|t| Self::degree_for(t, player_count, current))
    }

    /// Runs one full CPU distance sweep over `roster`, writing every receiver's tracking cache.
    pub fn test_only_run_distance_sweep(roster: &[(i32, Arc<PlayerState>)]) {
        let mut distance = DistanceSweepState::default();
        Self::snapshot_positions(&mut distance, roster);
        Self::run_distance_slice(&distance, roster, 0, roster.len());
    }

    /// The scalar reference for the vectorised interval encoding: `(encoded, actual_ms)` per input.
    pub fn test_only_encode_avatar_intervals(raw_intervals: &[i32], base_interval_ms: i32) -> (Vec<i32>, Vec<i32>) {
        let mut encoded = Vec::with_capacity(raw_intervals.len());
        let mut actual = Vec::with_capacity(raw_intervals.len());
        for &raw in raw_intervals {
            let e = BasisNetworkCommons::encode_avatar_interval_byte(raw, base_interval_ms);
            encoded.push(i32::from(e));
            actual.push(BasisNetworkCommons::decode_avatar_interval_ms(e, base_interval_ms));
        }
        (encoded, actual)
    }
}
