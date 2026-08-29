//! `BasisServerReductionSystemEvents.Serialization.cs`: pre-serializes a sender's keyframes and
//! deltas per quality and publishes the frame the send loop reads.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use basis_network_core::SerializableBasis::LocalAvatarSyncMessage;
use basis_network_core::compression::{BasisAvatarBitPacking, BasisAvatarDeltaCompression, BitQuality};
use basis_network_core::{BNL, BasisNetworkCommons};

use super::{BasisServerReductionSystemEvents, MS_TO_TICK, S, now_ticks};
use crate::reduction::{BSRProfiler, PlayerState, SenderFrame, SenderWork};

impl BasisServerReductionSystemEvents {
    fn quality_msg(s: &SenderWork, qi: usize) -> &LocalAvatarSyncMessage {
        match qi {
            0 => &s.avatar_very_low,
            1 => &s.avatar_low,
            2 => &s.avatar_medium,
            _ => &s.avatar_high,
        }
    }

    /// Serializes the qualities that had receivers (sticky `used_qualities` bits; all four for a
    /// new player) into the keyframe slots.
    fn pre_serialize_all(state: &PlayerState, sender: &mut SenderWork) {
        let player_id = state.id as u16;
        let mut mask = state.used_qualities.load(Ordering::Relaxed);
        if mask == 0 {
            mask = 0xF;
        }
        for qi in 0..4 {
            if mask & (1 << qi) != 0 {
                Self::pre_serialize_keyframe(state, sender, qi, player_id);
                BSRProfiler::increment_pre_serializations();
            } else {
                // Not available — the send loop will skip it and request it for next tick.
                sender.keyframe_arcs[qi] = None;
                BSRProfiler::increment_pre_serializations_skipped();
            }
        }
    }

    /// Byte-ID: `[PlayerID:1][interval:1][sequence:1][array:N][additional...]`;
    /// Ushort-ID: `[PlayerID:2][interval:1][sequence:1][array:N][additional...]`.
    /// Quality and additional-data presence are derived from the channel number.
    pub(super) fn pre_serialize_keyframe(state: &PlayerState, sender: &mut SenderWork, qi: usize, player_id: u16) {
        let s = &mut *sender;
        let msg = match qi {
            0 => &s.avatar_very_low,
            1 => &s.avatar_low,
            2 => &s.avatar_medium,
            _ => &s.avatar_high,
        };
        let Some(array) = msg.array.as_deref() else {
            s.keyframe_arcs[qi] = None;
            return;
        };
        // The message's quality must match the slot: a lower-quality upload must not go out on
        // a High channel (size mismatches on the receiver).
        let Some(quality) = BitQuality::from_byte(msg.data_quality_level) else {
            s.keyframe_arcs[qi] = None;
            return;
        };
        if quality.index() != qi {
            s.keyframe_arcs[qi] = None;
            return;
        }
        let expected_payload = BasisAvatarBitPacking::convert_to_size(quality);
        if array.len() < expected_payload {
            BNL::log_error(format!("[PreSerializeKeyframe] Array undersized for quality {quality:?}: got {}, need {expected_payload}. Skipping.", array.len()));
            s.keyframe_arcs[qi] = None;
            return;
        }
        let has_additional = s.has_additional_data && msg.additional_avatar_datas.as_ref().is_some_and(|d| !d.is_empty() && d.len() <= 255);
        s.keyframe_has_additional[qi] = has_additional;

        let small_id = state.small_id();
        let dst = &mut s.serialized_keyframe[qi];
        dst.clear();
        if small_id {
            dst.push(player_id as u8);
        } else {
            dst.extend_from_slice(&player_id.to_le_bytes());
        }
        dst.push(0); // interval placeholder (patched per receiver in the send loop)
        dst.push(s.outbound_sequence);
        dst.extend_from_slice(&array[..expected_payload]);
        if has_additional {
            Self::write_additional_data(dst, msg);
        }
        s.keyframe_arcs[qi] = Some(Arc::from(dst.as_slice()));
    }

    /// Every entry writes the full `[size:1][messageIndex:1]` header (size 0 for missing or
    /// oversized payloads) — must match `AdditionalAvatarData` exactly.
    fn write_additional_data(dst: &mut Vec<u8>, msg: &LocalAvatarSyncMessage) {
        let Some(datas) = msg.additional_avatar_datas.as_ref() else {
            return;
        };
        dst.push(datas.len() as u8);
        dst.push(msg.linked_avatar_index);
        for ad in datas {
            match ad.array.as_deref() {
                Some(array) if array.len() <= 255 => {
                    dst.push(array.len() as u8);
                    dst.push(ad.message_index);
                    dst.extend_from_slice(array);
                }
                _ => {
                    dst.push(0);
                    dst.push(ad.message_index);
                }
            }
        }
    }

    fn additional_data_size(state_has_additional: bool, msg: &LocalAvatarSyncMessage) -> (usize, bool) {
        let has_additional = state_has_additional && msg.additional_avatar_datas.as_ref().is_some_and(|d| !d.is_empty() && d.len() <= 255);
        if !has_additional {
            return (0, false);
        }
        let mut size = 1 + 1; // AdditionalSize + LinkedAvatarIndex
        if let Some(datas) = msg.additional_avatar_datas.as_ref() {
            for ad in datas {
                size += 1 + 1 + ad.array.as_ref().map(|a| a.len()).unwrap_or(0);
            }
        }
        (size, true)
    }

    /// Builds this sender's frame for `publish_gen` — a keyframe on the periodic cadence (or when
    /// forced, bypassing, or without a baseline), otherwise per-quality deltas — and publishes it.
    pub(super) fn pre_serialize_frame(state: &PlayerState, sender: &mut SenderWork, publish_gen: i64, force_keyframe: bool) {
        if !Self::enable_avatar_delta_compression() {
            Self::pre_serialize_all(state, sender);
            sender.current_is_keyframe = true;
            for qi in 0..4 {
                sender.delta_arcs[qi] = None;
            }
            Self::publish_frame(state, sender, publish_gen);
            return;
        }

        let now = now_ticks();
        let keyframe_interval_ticks = (f64::from(Self::effective_keyframe_interval_ms(sender.keyframe_stretch_shift)) * MS_TO_TICK) as i64;
        let mut is_keyframe = force_keyframe
            || state.bypass_reduction()
            || sender.keyframe_payload_length[3] == 0
            || (now - sender.last_keyframe_time_ticks) >= keyframe_interval_ticks;

        // Promotion: if the High delta isn't actually smaller than a High keyframe (fully-moving
        // avatar), just send a keyframe.
        if !is_keyframe {
            let high_payload = BasisAvatarBitPacking::convert_to_size(BitQuality::High);
            let valid_high = sender.avatar_high.array.as_ref().is_some_and(|a| a.len() >= high_payload) && sender.avatar_high.data_quality_level == 3;
            if !valid_high {
                is_keyframe = true;
            } else {
                let probe_cap = BasisAvatarDeltaCompression::max_delta_size(BitQuality::High);
                if sender.delta_probe_scratch.len() < probe_cap {
                    sender.delta_probe_scratch.resize(probe_cap, 0);
                }
                let s = &mut *sender;
                let dl = match s.avatar_high.array.as_deref() {
                    Some(high) => BasisAvatarDeltaCompression::build_delta(&s.keyframe_payload[3][..s.keyframe_payload_length[3]], high, BitQuality::High, &mut s.delta_probe_scratch, 0),
                    None => None,
                };
                match dl {
                    Some(dl) if dl < high_payload => Self::update_keyframe_stretch(sender, dl),
                    _ => {
                        is_keyframe = true;
                        sender.keyframe_stretch_shift = 0;
                        sender.small_delta_streak = 0;
                    }
                }
            }
        }

        let player_id = state.id as u16;
        if is_keyframe {
            sender.keyframe_gen = publish_gen;
            sender.keyframe_sequence = sender.outbound_sequence;
            sender.last_keyframe_time_ticks = now;
            sender.current_is_keyframe = true;
            // Serialize all four quality keyframes (that have valid arrays) and snapshot their
            // payloads as the delta baseline, so a receiver at ANY quality can rebaseline.
            for qi in 0..4 {
                let (valid, payload) = {
                    let msg = Self::quality_msg(sender, qi);
                    let payload = BitQuality::from_byte(qi as u8).map(BasisAvatarBitPacking::convert_to_size).unwrap_or(0);
                    let valid = msg.array.as_ref().is_some_and(|a| a.len() >= payload) && usize::from(msg.data_quality_level) == qi;
                    (valid, payload)
                };
                if !valid {
                    sender.keyframe_arcs[qi] = None;
                    sender.keyframe_payload_length[qi] = 0;
                    sender.delta_arcs[qi] = None;
                    BSRProfiler::increment_pre_serializations_skipped();
                    continue;
                }
                {
                    let s = &mut *sender;
                    let msg = match qi {
                        0 => &s.avatar_very_low,
                        1 => &s.avatar_low,
                        2 => &s.avatar_medium,
                        _ => &s.avatar_high,
                    };
                    if let Some(array) = msg.array.as_deref() {
                        s.keyframe_payload[qi].clear();
                        s.keyframe_payload[qi].extend_from_slice(&array[..payload]);
                        s.keyframe_payload_length[qi] = payload;
                    }
                }
                Self::pre_serialize_keyframe(state, sender, qi, player_id);
                sender.delta_arcs[qi] = None; // no delta on a keyframe generation
                BSRProfiler::increment_pre_serializations();
            }
        } else {
            sender.current_is_keyframe = false;
            // Build deltas only for qualities that had receivers. Keyframe buffers are left
            // intact from the last keyframe tick so a lagging receiver can rebaseline.
            let mut mask = state.used_qualities.load(Ordering::Relaxed);
            if mask == 0 {
                mask = 0xF;
            }
            for qi in 0..4 {
                if mask & (1 << qi) == 0 {
                    sender.delta_arcs[qi] = None;
                    continue;
                }
                Self::pre_serialize_delta(state, sender, qi, player_id);
            }
        }
        Self::publish_frame(state, sender, publish_gen);
    }

    fn publish_frame(state: &PlayerState, sender: &SenderWork, publish_gen: i64) {
        state.frame.store(Arc::new(SenderFrame {
            generation: publish_gen,
            keyframe_gen: sender.keyframe_gen,
            current_is_keyframe: sender.current_is_keyframe,
            small_id: state.small_id(),
            bypass_reduction: state.bypass_reduction(),
            serialized_keyframe: sender.keyframe_arcs.clone(),
            serialized_has_additional: sender.keyframe_has_additional,
            serialized_delta: sender.delta_arcs.clone(),
        }));
    }

    pub fn effective_keyframe_interval_ms(stretch_shift: i32) -> i32 {
        let base_ms = Self::avatar_delta_keyframe_interval_ms();
        let max_ms = Self::avatar_delta_keyframe_max_interval_ms();
        if max_ms <= base_ms || stretch_shift <= 0 {
            return base_ms;
        }
        let stretched = i64::from(base_ms) << stretch_shift.min(8);
        if stretched >= i64::from(max_ms) { max_ms } else { stretched as i32 }
    }

    pub(super) fn update_keyframe_stretch(sender: &mut SenderWork, high_delta_length: usize) {
        if high_delta_length > Self::SMALL_HIGH_DELTA_BYTES {
            sender.keyframe_stretch_shift = 0;
            sender.small_delta_streak = 0;
            return;
        }
        if Self::effective_keyframe_interval_ms(sender.keyframe_stretch_shift + 1) == Self::effective_keyframe_interval_ms(sender.keyframe_stretch_shift) {
            return;
        }
        sender.small_delta_streak += 1;
        if sender.small_delta_streak >= Self::SMALL_DELTA_STREAK_TO_STRETCH {
            sender.small_delta_streak = 0;
            sender.keyframe_stretch_shift += 1;
        }
    }

    pub fn request_keyframe(sender_id: i32, receiver_id: i32) {
        S.pending_keyframe_requests.push((sender_id, receiver_id));
        Self::wake_tick();
    }

    pub(super) fn process_pending_keyframe_requests() {
        while let Some((sender_id, receiver_id)) = S.pending_keyframe_requests.pop() {
            // Tracking lives on the RECEIVER, indexed by sender id.
            let Some(receiver) = S.player_states.get_cloned(receiver_id) else {
                continue;
            };
            let Ok(sender_index) = usize::try_from(sender_id) else {
                continue;
            };
            let mut data = receiver.receiver.lock();
            if let Some(t) = data.peer_tracking.get_mut(sender_index) {
                t.baseline_keyframe_gen = -1;
                // Reopen the new-data and interval gates: a fully idle sender publishes no new
                // generation, so without this the requested keyframe would wait for motion.
                t.last_seen_generation = 0;
                t.last_sent_time = 0;
            }
        }
    }

    /// Delta frame layout: `[header:1][playerId:1|2][interval:1][sequence:1][baseSeq:1][body][additional...]`.
    pub(super) fn pre_serialize_delta(state: &PlayerState, sender: &mut SenderWork, qi: usize, player_id: u16) {
        let Some(q) = BitQuality::from_byte(qi as u8) else {
            sender.delta_arcs[qi] = None;
            return;
        };
        let payload = BasisAvatarBitPacking::convert_to_size(q);
        let s = &mut *sender;
        let msg = match qi {
            0 => &s.avatar_very_low,
            1 => &s.avatar_low,
            2 => &s.avatar_medium,
            _ => &s.avatar_high,
        };
        let Some(array) = msg.array.as_deref() else {
            s.delta_arcs[qi] = None;
            return;
        };
        if usize::from(msg.data_quality_level) != qi || array.len() < payload || s.keyframe_payload_length[qi] < payload {
            s.delta_arcs[qi] = None;
            return;
        }
        let (additional_size, has_additional) = Self::additional_data_size(s.has_additional_data, msg);
        let small_id = state.small_id();
        let id_size = if small_id { 1 } else { 2 };
        let header_bytes = 1 + id_size + 1 + 1 + 1;
        let cap = header_bytes + BasisAvatarDeltaCompression::max_delta_size(q) + additional_size;
        let dst = &mut s.serialized_delta[qi];
        dst.clear();
        dst.resize(cap, 0);
        let mut o = 0;
        dst[o] = BasisNetworkCommons::build_delta_header(qi as i32, has_additional, !small_id);
        o += 1;
        if small_id {
            dst[o] = player_id as u8;
            o += 1;
        } else {
            dst[o..o + 2].copy_from_slice(&player_id.to_le_bytes());
            o += 2;
        }
        dst[o] = 0; // interval placeholder
        o += 1;
        dst[o] = s.outbound_sequence;
        o += 1;
        dst[o] = s.keyframe_sequence; // baseSeq — the keyframe this delta reconstructs against
        o += 1;
        let Some(body_len) = BasisAvatarDeltaCompression::build_delta(&s.keyframe_payload[qi][..payload], array, q, dst, o) else {
            s.delta_arcs[qi] = None;
            return;
        };
        o += body_len;
        dst.truncate(o);
        if has_additional {
            Self::write_additional_data(dst, msg);
        }
        s.delta_arcs[qi] = Some(Arc::from(dst.as_slice()));
        BSRProfiler::increment_pre_serializations();
    }
}
