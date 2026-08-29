//! `BasisServerReductionSystemEvents.MessageProcessing.cs`: turns one inbound frame into the
//! sender's published state.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use basis_network_core::SerializableBasis::LocalAvatarSyncMessage;
use basis_network_core::compression::{BasisAvatarBitPacking, BasisNetworkCompressionExtensions, BitQuality};
use basis_network_core::transport::basis_network_shell::peers_equal;
use basis_network_core::BNL;

use super::{BasisServerReductionSystemEvents, MS_TO_TICK, S, now_ticks};
use crate::NetworkServer;
use crate::reduction::{AvatarQualityRepacker, PlayerState, QueuedMessage, QueuedMessagePool, SenderWork};
use crate::security::BasisGlobalLockManager;

impl BasisServerReductionSystemEvents {
    fn keyframe_phase_fraction(id: i32) -> f64 {
        let scrambled = (id as u32).wrapping_mul(2_654_435_761);
        f64::from(scrambled) / f64::from(u32::MAX)
    }

    pub(super) fn propagate_additional_data(sender: &mut SenderWork) {
        let additional = sender.avatar_high.additional_avatar_datas.clone();
        let size = sender.avatar_high.additional_avatar_data_size;
        let linked = sender.avatar_high.linked_avatar_index;
        sender.avatar_medium.additional_avatar_datas = additional.clone();
        sender.avatar_medium.additional_avatar_data_size = size;
        sender.avatar_medium.linked_avatar_index = linked;
        let strip = Self::strip_additional_data_at_low_quality();
        for low in [&mut sender.avatar_low, &mut sender.avatar_very_low] {
            low.additional_avatar_datas = if strip { None } else { additional.clone() };
            low.additional_avatar_data_size = if strip { 0 } else { size };
            low.linked_avatar_index = linked;
        }
    }

    /// Position-only fast path: carry position to all lower qualities without re-packing bones.
    #[inline]
    fn copy_position_to_lower_qualities(sender: &mut SenderWork) {
        let pos_bytes = BasisAvatarBitPacking::WRITE_POSITION;
        let Some(high) = sender.avatar_high.array.as_ref() else {
            return;
        };
        if high.len() < pos_bytes {
            return;
        }
        let position: [u8; BasisAvatarBitPacking::WRITE_POSITION] = match high[..pos_bytes].try_into() {
            Ok(p) => p,
            Err(_) => return,
        };
        for lower in [&mut sender.avatar_medium, &mut sender.avatar_low, &mut sender.avatar_very_low] {
            if let Some(array) = lower.array.as_mut()
                && array.len() >= pos_bytes
            {
                array[..pos_bytes].copy_from_slice(&position);
            }
        }
    }

    fn null_lower_qualities(sender: &mut SenderWork) {
        sender.avatar_medium.array = None;
        sender.avatar_low.array = None;
        sender.avatar_very_low.array = None;
    }

    fn repack_lower(sender: &mut SenderWork) {
        let s = &mut *sender;
        if let Err(e) = AvatarQualityRepacker::build_all_lower_from_high_into(&s.avatar_high, &mut s.avatar_medium, &mut s.avatar_low, &mut s.avatar_very_low) {
            BNL::log_error(format!("[ProcessMessage] Repack failed: {e}"));
            // Don't alias High into lower slots — that sends High-packed muscle data on
            // lower-quality channels. Null the arrays so pre-serialization skips them.
            Self::null_lower_qualities(sender);
        }
    }

    pub(super) fn process_message(mut message: QueuedMessage) {
        let Some(from_peer) = message.from_peer.clone() else {
            QueuedMessagePool::return_message(message);
            return;
        };
        let id = from_peer.id();
        let inbound_seq = message.sequence;

        if BasisGlobalLockManager::additional_avatar_data_lock() {
            message.avatar_message.additional_avatar_datas = None;
            message.avatar_message.additional_avatar_data_size = 0;
        }
        let Some(incoming_quality) = BitQuality::from_byte(message.avatar_message.data_quality_level) else {
            QueuedMessagePool::return_message(message);
            return;
        };
        let is_high_quality = incoming_quality == BitQuality::High;
        let expected_payload_size = BasisAvatarBitPacking::convert_to_size(incoming_quality);
        let Some(pos) = message.avatar_message.array.as_deref().filter(|a| a.len() >= expected_payload_size).and_then(BasisNetworkCompressionExtensions::read_position)
        else {
            if message.avatar_message.array.is_none() {
                BNL::log_error(format!("[ProcessMessage] Avatar array is null for peer {id}"));
            }
            QueuedMessagePool::return_message(message);
            return;
        };

        // A message can outlive its sender: removals drain at MAX_REMOVALS_PER_TICK, and a stale
        // frame drained after its player's removal must not recreate state around a dead peer.
        let existing = S.player_states.get_cloned(id);
        if existing.is_none() {
            let live = NetworkServer::authenticated_peers().get(&id).map(|p| p.value().clone());
            if !live.is_some_and(|live| peers_equal(&live, &from_peer)) {
                QueuedMessagePool::return_message(message);
                return;
            }
        }

        // Deep-copy the avatar payload so the sender's High slot owns its own buffer; the pooled
        // message keeps its array for the next rent.
        let high = LocalAvatarSyncMessage {
            data_quality_level: message.avatar_message.data_quality_level,
            additional_avatar_datas: message.avatar_message.additional_avatar_datas.take(),
            additional_avatar_data_size: message.avatar_message.additional_avatar_data_size,
            linked_avatar_index: message.avatar_message.linked_avatar_index,
            array: message.avatar_message.array.as_ref().map(|a| a[..expected_payload_size].to_vec()),
        };

        match existing {
            None => {
                let state = Arc::new(PlayerState::new(id, from_peer.clone(), pos, Self::INITIAL_PLAYER_ARRAY_CAPACITY));
                state.bypass_reduction.store(S.bypass_reduction_ids.contains_key(&id), Ordering::Relaxed);
                {
                    let mut sender = state.sender.lock();
                    sender.has_additional_data = high.additional_avatar_datas.as_ref().is_some_and(|d| !d.is_empty());
                    sender.avatar_high = high;
                    sender.high_array_actual_size = expected_payload_size;
                    // Stagger the periodic keyframe phase per player so a mass join does not
                    // produce a synchronized herd of keyframe ticks. Hashed rather than random so
                    // it is deterministic.
                    sender.last_keyframe_time_ticks = now_ticks()
                        - (Self::keyframe_phase_fraction(id) * f64::from(Self::avatar_delta_keyframe_interval_ms()) * MS_TO_TICK) as i64;
                    sender.last_inbound_sequence = inbound_seq;
                    sender.has_received_first = true;
                    sender.outbound_sequence = 0;
                    if is_high_quality {
                        Self::repack_lower(&mut sender);
                    } else {
                        // Non-High quality: can't repack downward.
                        Self::null_lower_qualities(&mut sender);
                    }
                    Self::propagate_additional_data(&mut sender);
                    // First frame: always a keyframe (generation 1).
                    Self::pre_serialize_frame(&state, &mut sender, 1, true);
                }
                state.data_generation.store(1, Ordering::Release);
                S.player_states.insert(id, state.clone());
                {
                    let mut active = S.active_players.lock();
                    active.push((id, state));
                    S.active_players_dirty.store(true, Ordering::Release);
                    S.active_player_count.fetch_add(1, Ordering::Relaxed);
                }
            }
            Some(state) => {
                let mut sender = state.sender.lock();
                // Peer-slot reuse: ids are recycled after disconnect. If the incoming peer is a
                // different connection, refresh the reference and treat the next frame as the
                // first so the sequence-delta check doesn't drop it.
                if !peers_equal(&state.peer(), &from_peer) {
                    *state.peer.write() = from_peer.clone();
                    sender.has_received_first = false;
                    state.small_id.store(id <= i32::from(u8::MAX), Ordering::Relaxed);
                    state.bypass_reduction.store(S.bypass_reduction_ids.contains_key(&id), Ordering::Relaxed);
                }
                // Drop stale inbound packets (unreliable can deliver out of order).
                if sender.has_received_first {
                    let delta = inbound_seq.wrapping_sub(sender.last_inbound_sequence);
                    if delta == 0 || delta >= 128 {
                        drop(sender);
                        QueuedMessagePool::return_message(message);
                        return;
                    }
                }
                sender.last_inbound_sequence = inbound_seq;
                sender.has_received_first = true;
                state.is_active.store(true, Ordering::Release);
                state.set_position(pos);
                sender.outbound_sequence = sender.outbound_sequence.wrapping_add(1);

                let prev_array = sender.avatar_high.array.take();
                let prev_actual_size = sender.high_array_actual_size;
                sender.avatar_high = high;
                sender.high_array_actual_size = expected_payload_size;

                if is_high_quality {
                    // Skip the expensive bit repacking if only the position moved.
                    let muscle_and_tail = BasisAvatarBitPacking::muscle_bytes(BitQuality::High) + BasisAvatarBitPacking::TAIL_BYTES;
                    let start = BasisAvatarBitPacking::WRITE_POSITION;
                    let muscles_or_tail_changed = match (prev_array.as_deref(), sender.avatar_high.array.as_deref()) {
                        (Some(prev), Some(cur)) => {
                            prev_actual_size != expected_payload_size
                                || prev.len() < start + muscle_and_tail
                                || cur.len() < start + muscle_and_tail
                                || cur[start..start + muscle_and_tail] != prev[start..start + muscle_and_tail]
                        }
                        _ => true,
                    };
                    // Force a full repack when any lower quality array is missing (e.g. after a
                    // previous repack failure); otherwise far receivers would never see the player.
                    let needs_recovery = sender.avatar_medium.array.is_none() || sender.avatar_low.array.is_none() || sender.avatar_very_low.array.is_none();
                    if muscles_or_tail_changed || needs_recovery {
                        Self::repack_lower(&mut sender);
                    } else {
                        Self::copy_position_to_lower_qualities(&mut sender);
                    }
                } else {
                    Self::null_lower_qualities(&mut sender);
                }
                Self::propagate_additional_data(&mut sender);
                sender.has_additional_data = sender.avatar_high.additional_avatar_datas.as_ref().is_some_and(|d| !d.is_empty());

                // publish_gen = the generation value this frame becomes after the increment below.
                let publish_gen = state.data_generation() + 1;
                Self::pre_serialize_frame(&state, &mut sender, publish_gen, false);
                drop(sender);
                // Receivers detect new data by comparing this generation against their last seen.
                state.data_generation.fetch_add(1, Ordering::AcqRel);
            }
        }
        QueuedMessagePool::return_message(message);
    }
}
