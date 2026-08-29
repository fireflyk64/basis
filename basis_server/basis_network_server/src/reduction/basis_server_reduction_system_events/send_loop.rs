//! `BasisServerReductionSystemEvents.SendLoop.cs`: the per-tick O(receivers × senders) pass that
//! decides which frames go to whom, and the per-receiver flush.

use std::sync::Arc;

use basis_network_core::statistics::basis_network_statistics::BasisNetworkStatistics;
use basis_network_core::{BasisNetworkCommons, NetPeerRef};

use super::tick::TickState;
use super::{BasisServerReductionSystemEvents, MS_TO_TICK};
use crate::p2p::BasisServerP2PBroker;
use crate::reduction::{BSRProfiler, BSRThreadCounters, PendingAvatarSend, PlayerState, ReceiverData, SenderFrame};

/// Per-receiver-tick stats accumulator: folds per-send counting into one record per channel.
#[derive(Default)]
pub(super) struct TailStats {
    used: usize,
    channels: [u8; 8],
    counts: [i64; 8],
    bytes: [i64; 8],
}

impl TailStats {
    fn add(&mut self, channel: u8, length: usize) {
        for i in 0..self.used {
            if self.channels[i] == channel {
                self.counts[i] += 1;
                self.bytes[i] += length as i64;
                return;
            }
        }
        if self.used < 8 {
            self.channels[self.used] = channel;
            self.counts[self.used] = 1;
            self.bytes[self.used] = length as i64;
            self.used += 1;
            return;
        }
        // More distinct channels in one flush than expected: record directly.
        BasisNetworkStatistics::record_outbound_batch(channel, 1, length as i64);
    }

    fn flush(&mut self) {
        for i in 0..self.used {
            BasisNetworkStatistics::record_outbound_batch(self.channels[i], self.counts[i], self.bytes[i]);
        }
        self.used = 0;
    }
}

impl BasisServerReductionSystemEvents {
    /// Cap on how far shedding may stretch an interval: 3 doublings = 8x.
    pub(super) const MAX_SHED_INTERVAL_DOUBLINGS: i32 = 3;

    pub(super) fn update_communication_and_distances(tick: &mut TickState, now_ticks: i64) {
        let active_copy = Self::active_players_snapshot();
        let player_count = active_copy.len();
        if player_count == 0 {
            return;
        }
        // Retune workers to the current population before the phase that uses them.
        Self::tune_parallelism(player_count as i32);
        // Advance the sender visit order so queue trims do not always fall on the same players.
        tick.sender_rotation = tick.sender_rotation.wrapping_add(1);

        // Snapshot generation counters and frames once, so the inner loop reads plain arrays.
        let max_id = active_copy.iter().map(|(id, _)| *id).max().unwrap_or(0).max(0) as usize;
        if tick.generation_snapshot.len() < max_id + 1 {
            tick.generation_snapshot.resize((max_id + 1).max(tick.generation_snapshot.len() * 2), 0);
        }
        tick.frames.clear();
        for (id, state) in active_copy.iter() {
            tick.generation_snapshot[*id as usize] = state.data_generation();
            tick.frames.push(state.frame.load_full());
        }

        let base_interval = Self::bsrs_millisecond_default_interval();
        // Fallback interval for pairs not yet in the distance cache (new players).
        let min_interval_ticks = (f64::from(base_interval) * f64::from(Self::bsr_base_multiplier()) * MS_TO_TICK) as i64;

        // Floor on the interval advertised this tick: a receiver is only visited every
        // slice_count ticks, so nothing can arrive faster than that regardless of distance.
        let slice_count = Self::slice_count().max(1) as usize;
        let deliverable_interval_ms = (Self::interval_ms() * slice_count as i64) as i32;
        let degraded_interval_byte = BasisNetworkCommons::encode_avatar_interval_byte(deliverable_interval_ms, base_interval);

        // Tick slicing: only process a slice of receivers per tick.
        let slice_size = player_count.div_ceil(slice_count);
        let start = tick.slice_index * slice_size;
        let end = (start + slice_size).min(player_count);
        tick.slice_index = (tick.slice_index + 1) % slice_count;
        if start >= player_count {
            return;
        }
        // Sender/receiver pairs this pass will consider — the unit the send phase's cost scales in.
        tick.last_send_pairs = ((end - start) * player_count) as i64;
        tick.last_send_workers = (Self::send_workers() as usize).min(end - start) as i32;

        let bundling_enabled = Self::enable_avatar_bundle_compression();
        let load_shed_tier = Self::load_shed_tier();
        let enable_delta = Self::enable_avatar_delta_compression();
        let has_offloaded = BasisServerP2PBroker::has_offloaded_pairs();
        let sender_rotation = tick.sender_rotation;
        let profiling = BSRProfiler::enabled();
        let generation_snapshot: &[i64] = &tick.generation_snapshot;
        let frames: &[Arc<SenderFrame>] = &tick.frames;
        let roster: &[(i32, Arc<PlayerState>)] = &active_copy;

        Self::parallel_for(start, end, |i| {
            let (id, state_i) = &roster[i];
            let id = *id;
            let peer = state_i.peer();
            let mut recv = state_i.receiver.lock();
            let mut pending = std::mem::take(&mut recv.pending_sends);
            pending.clear();
            let mut local_sends: i64 = 0;

            // Senders are visited from a rotating offset, staggered by receiver as well as by
            // tick, so an over-budget queue trims a different sender for each viewer.
            let rotation = ((sender_rotation as usize).wrapping_add(id as usize)) % player_count;
            for step in 0..player_count {
                let mut index = step + rotation;
                if index >= player_count {
                    index -= player_count;
                }
                let (j_id, state_j) = &roster[index];
                let j_id = *j_id;
                if id == j_id {
                    continue;
                }
                let j_index = j_id as usize;
                PlayerState::ensure_tracking(&mut recv, j_index);

                // 1. New data check — the cheapest test in the loop and it rejects most pairs.
                let sender_gen = generation_snapshot[j_index];
                if sender_gen <= recv.peer_tracking[j_index].last_seen_generation {
                    continue;
                }
                // Their avatar data goes peer-to-peer, so the server must not also relay it.
                if has_offloaded && BasisServerP2PBroker::is_p2p_offloaded(j_id, id) {
                    continue;
                }
                let frame = &frames[index];
                // Full-quality broadcast bypasses the distance throttle + quality reduction.
                let bypass_reduction = frame.bypass_reduction;
                let tracking = recv.peer_tracking[j_index];
                let qi = if bypass_reduction { 3 } else { usize::from(tracking.cached_quality_index).min(3) };

                // 2. Interval check using cached distance results. Load shedding is applied as an
                //    interval MULTIPLIER rather than a drop, so an overloaded server slows distant
                //    players down instead of freezing them.
                let shed_steps = load_shed_tier - qi as i32;
                if !bypass_reduction {
                    let elapsed = now_ticks - tracking.last_sent_time;
                    let mut required = i64::from(tracking.cached_interval_ticks);
                    if required <= 0 {
                        required = min_interval_ticks;
                    }
                    if shed_steps > 0 {
                        required <<= shed_steps.min(Self::MAX_SHED_INTERVAL_DOUBLINGS);
                    }
                    if elapsed < required {
                        continue;
                    }
                }

                // Report the cadence actually delivered, not the one distance asked for: the
                // client decodes this byte into the window it interpolates the pose over.
                let start_at_zero_interval = if bypass_reduction {
                    0
                } else {
                    let mut pair_interval = tracking.cached_interval_byte;
                    if shed_steps > 0 {
                        let stretched = BasisNetworkCommons::decode_avatar_interval_ms(pair_interval, base_interval) << shed_steps.min(Self::MAX_SHED_INTERVAL_DOUBLINGS);
                        pair_interval = BasisNetworkCommons::encode_avatar_interval_byte(stretched, base_interval);
                    }
                    pair_interval.max(degraded_interval_byte)
                };

                // Delta vs keyframe: a delta only when the current frame is a delta, the receiver
                // already holds the current keyframe at this quality, and the delta exists.
                let send_delta = enable_delta
                    && !bypass_reduction
                    && !frame.current_is_keyframe
                    && tracking.baseline_keyframe_gen == frame.keyframe_gen
                    && usize::from(tracking.baseline_quality) == qi
                    && frame.serialized_delta[qi].is_some();

                let (source, channel, interval_offset) = if send_delta {
                    let Some(delta) = frame.serialized_delta[qi].as_ref() else {
                        continue;
                    };
                    // delta frame layout: [header:1][playerId:1|2][interval:1]...
                    (delta.clone(), BasisNetworkCommons::DELTA_AVATAR_CHANNEL, if frame.small_id { 2 } else { 3 })
                } else {
                    // Keyframe path (also the fallback when the receiver lacks the baseline).
                    let Some(keyframe) = frame.serialized_keyframe[qi].as_ref() else {
                        state_j.mark_quality_used(qi);
                        continue;
                    };
                    let has_additional = frame.serialized_has_additional[qi];
                    let channel = if frame.small_id {
                        BasisNetworkCommons::get_player_avatar_channel_for_quality(qi as i32, has_additional)
                    } else {
                        BasisNetworkCommons::get_player_avatar_large_channel_for_quality(qi as i32, has_additional)
                    };
                    // Receiver now holds this keyframe generation + quality; subsequent deltas apply.
                    let t = &mut recv.peer_tracking[j_index];
                    t.baseline_keyframe_gen = frame.keyframe_gen;
                    t.baseline_quality = qi as u8;
                    (keyframe.clone(), channel, if frame.small_id { 1 } else { 2 })
                };

                let length = source.len();
                pending.push(PendingAvatarSend { source, length, channel, interval: start_at_zero_interval, interval_offset });
                state_j.mark_quality_used(qi);
                let t = &mut recv.peer_tracking[j_index];
                t.last_sent_time = now_ticks;
                t.last_seen_generation = sender_gen;
                local_sends += 1;
            }

            recv.pending_sends = pending;
            if !recv.pending_sends.is_empty() {
                Self::flush_pending_for_receiver(&mut recv, &peer, bundling_enabled, sender_rotation);
            }
            if local_sends > 0 && profiling {
                BSRProfiler::local(|c| BSRThreadCounters::add(&c.sends, local_sends));
            }
        });
    }

    pub(super) fn flush_pending_for_receiver(recv: &mut ReceiverData, peer: &NetPeerRef, bundling_enabled: bool, sender_rotation: u32) {
        let count = recv.pending_sends.len();
        if count == 0 {
            return;
        }
        let mut tail = TailStats::default();
        let mut bundle_count: i64 = 0;
        let mut bundle_bytes: i64 = 0;
        let min_messages = usize::try_from(Self::avatar_bundle_min_messages()).unwrap_or(0);

        // Bundling is only worth its CPU if the payload actually compresses. When a receiver's
        // observed ratio says otherwise we stop deflating for it and re-probe occasionally.
        let mut bundle_this_flush = bundling_enabled && count >= min_messages;
        if bundle_this_flush && recv.bundle_skip_countdown > 0 {
            recv.bundle_skip_countdown -= 1;
            bundle_this_flush = false;
        }

        let mut cursor = 0;
        if bundle_this_flush {
            // Group by channel before chunking, so each bundle carries a few long runs.
            Self::sort_pending_by_channel(recv, count, sender_rotation);
            cursor = Self::emit_greedy_bundles(recv, peer, &mut bundle_count, &mut bundle_bytes);
            if cursor > 0 && recv.last_bundle_ratio > Self::avatar_bundle_max_ratio() {
                recv.bundle_skip_countdown = Self::avatar_bundle_reprobe_flushes();
            }
        }

        // Send anything not packed into a bundle (the tail below the minimum, or everything when
        // bundling is off or produced nothing).
        let mut tail_sent = 0i64;
        for p in &recv.pending_sends[cursor..] {
            if p.length <= usize::from(p.interval_offset) {
                continue;
            }
            if peer.send_unreliable_raw_merge(&p.source, 0, p.length, p.channel, i32::from(p.interval_offset), p.interval).is_ok() {
                tail.add(p.channel, p.length);
                tail_sent += 1;
            }
        }

        if BasisNetworkStatistics::is_recording_data() {
            if bundle_count > 0 {
                BasisNetworkStatistics::record_outbound_batch(BasisNetworkCommons::COMPRESSED_AVATAR_BUNDLE_CHANNEL, bundle_count, bundle_bytes);
            }
            if tail_sent > 0 {
                tail.flush();
            }
        }
        if BSRProfiler::enabled() && tail_sent > 0 {
            BSRProfiler::local(|c| {
                BSRThreadCounters::add(&c.bundle_tail_uncompressed, tail_sent);
                if bundling_enabled && cursor == 0 && count >= min_messages {
                    BSRThreadCounters::add(&c.bundle_fallbacks, 1);
                }
            });
        }
        // Clear the payload references, not just the count, so serialized payloads of players who
        // may already have disconnected are released.
        recv.pending_sends.clear();

        // Give back a buffer that a busy spell grew and quiet ticks no longer justify.
        recv.pending_peak = recv.pending_peak.max(count);
        recv.pending_peak_ticks += 1;
        if recv.pending_peak_ticks >= Self::PENDING_SHRINK_WINDOW_TICKS {
            recv.pending_peak_ticks = 0;
            let want = Self::PENDING_MIN_CAPACITY.max(recv.pending_peak * 2);
            if recv.pending_sends.capacity() > want * 2 {
                recv.pending_sends = Vec::with_capacity(want);
            }
            recv.pending_peak = 0;
        }
        // Keep modest scratch buffers between ticks; only hand back the oversized ones.
        if recv.bundle_raw_scratch.capacity() > Self::RETAINED_SCRATCH_BYTES {
            recv.bundle_raw_scratch = Vec::new();
        }
        if recv.bundle_compressed_scratch.capacity() > Self::RETAINED_SCRATCH_BYTES {
            recv.bundle_compressed_scratch = Vec::new();
        }
    }
}
