//! `BasisServerReductionSystemEvents.Inbound.cs`: the receive-thread entry points.

use std::sync::atomic::Ordering;

use basis_network_core::SerializableBasis::LocalAvatarSyncMessage;
use basis_network_core::compression::{BasisAvatarBitPacking, BasisAvatarDeltaCompression, BitQuality};
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetPacketReader, NetPeerRef};
use parking_lot::Mutex;

use super::{BasisServerReductionSystemEvents, S, UplinkDeltaState, now_ticks};
use crate::NetworkServer;
use crate::reduction::QueuedMessagePool;

impl BasisServerReductionSystemEvents {
    /// One NACK per sender per second.
    const NACK_MIN_INTERVAL_TICKS: i64 = 1_000_000;

    pub fn handle_avatar_movement(mut reader: NetPacketReader, from_peer: &NetPeerRef, channel: u8) {
        Self::ensure_started();
        // The application-level sequence byte prepended by the client.
        let Ok(sequence) = reader.get_byte() else {
            return;
        };
        // Quality and additional-data presence are derived from the channel.
        let quality = BasisNetworkCommons::get_quality_from_channel(channel);
        let has_additional = BasisNetworkCommons::channel_has_additional_data(channel);

        // Rent BEFORE deserialize so the pooled buffer is reused.
        let mut message = QueuedMessagePool::rent();
        message.from_peer = Some(from_peer.clone());
        message.sequence = sequence;
        if message.avatar_message.deserialize_for_channel(&mut reader, quality, has_additional).is_err() || message.avatar_message.array.is_none() {
            BNL::log_error(format!("[HandleAvatarMovement] Deserialized avatar message has no payload from peer {}", from_peer.id()));
            QueuedMessagePool::return_message(message);
            return;
        }
        // Every full High frame doubles as the sender's uplink delta baseline.
        if quality == 3
            && let Some(array) = message.avatar_message.array.as_deref()
        {
            Self::uplink_capture_baseline(from_peer.id(), array, sequence);
        }
        // Overwrite any pending message for this peer: only the newest per tick survives.
        Self::enqueue(from_peer.id(), message);
    }

    fn enqueue(peer_id: i32, message: crate::reduction::QueuedMessage) {
        if let Some(previous) = S.current_messages.insert(peer_id, message) {
            QueuedMessagePool::return_message(previous);
        }
        // Wake the loop only while it is parked (empty server).
        if S.active_player_count.load(Ordering::Relaxed) == 0 {
            Self::wake_tick();
        }
    }

    fn uplink_capture_baseline(peer_id: i32, payload: &[u8], sequence: u8) {
        let size = BasisAvatarBitPacking::convert_to_size(BitQuality::High);
        if payload.len() < size {
            return;
        }
        let entry = S
            .uplink_states
            .entry(peer_id)
            .or_insert_with(|| Mutex::new(UplinkDeltaState { baseline: Vec::new(), baseline_seq: 0, has: false, last_nack_ticks: 0 }));
        let mut st = entry.lock();
        st.baseline.clear();
        st.baseline.extend_from_slice(&payload[..size]);
        st.baseline_seq = sequence;
        st.has = true;
    }

    fn nack_uplink(peer: &NetPeerRef, st: Option<&mut UplinkDeltaState>) {
        let now = now_ticks();
        if let Some(st) = st {
            if now - st.last_nack_ticks < Self::NACK_MIN_INTERVAL_TICKS {
                return;
            }
            st.last_nack_ticks = now;
        }
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(BasisNetworkCommons::DELTA_CONTROL_UPLINK_KEYFRAME_REQUEST);
        NetworkServer::try_send(peer, &writer, BasisNetworkCommons::DELTA_AVATAR_CHANNEL, DeliveryMethod::ReliableOrdered);
        NetworkServer::return_writer(writer);
    }

    pub fn handle_delta_channel_inbound(mut reader: NetPacketReader, from_peer: &NetPeerRef) {
        Self::ensure_started();
        let Ok(header) = reader.get_byte() else {
            return;
        };
        if BasisNetworkCommons::is_delta_control_header(header) {
            if header == BasisNetworkCommons::DELTA_CONTROL_KEYFRAME_REQUEST
                && let Ok(sender_id) = reader.get_ushort()
            {
                Self::request_keyframe(i32::from(sender_id), from_peer.id());
            }
            return;
        }
        if BasisNetworkCommons::delta_header_quality(header) != 3 {
            // Clients only ever upload High.
            return;
        }
        let has_additional = BasisNetworkCommons::delta_header_has_additional_data(header);
        let (Ok(sequence), Ok(base_seq)) = (reader.get_byte(), reader.get_byte()) else {
            return;
        };

        let Some(entry) = S.uplink_states.get(&from_peer.id()) else {
            Self::nack_uplink(from_peer, None);
            return;
        };
        let mut st = entry.lock();
        if !st.has || st.baseline_seq != base_seq {
            // Missing/stale baseline (lost keyframe or reorder) — ask for a fresh keyframe.
            Self::nack_uplink(from_peer, Some(&mut st));
            return;
        }
        let raw = reader.raw_data();
        let position = reader.position();
        let available = reader.available_bytes();
        let Some(body_len) = BasisAvatarDeltaCompression::delta_body_length(raw, position, available, BitQuality::High) else {
            return;
        };
        if body_len > available {
            return;
        }
        let payload_size = BasisAvatarDeltaCompression::payload_size(BitQuality::High);
        let mut message = QueuedMessagePool::rent();
        message.from_peer = Some(from_peer.clone());
        message.sequence = sequence;
        let array = message.avatar_message.array.get_or_insert_with(|| vec![0u8; payload_size]);
        if array.len() != payload_size {
            array.resize(payload_size, 0);
        }
        let ok = BasisAvatarDeltaCompression::try_apply_delta(&st.baseline, raw, position, body_len, BitQuality::High, array);
        drop(st);
        if !ok {
            QueuedMessagePool::return_message(message);
            return;
        }
        reader.skip_bytes(body_len);
        message.avatar_message.data_quality_level = 3;
        message.avatar_message.additional_avatar_data_size = 0;
        message.avatar_message.additional_avatar_datas = None;
        if has_additional && message.avatar_message.deserialize_additional_data(&mut reader).is_err() {
            // A torn additional-data section is dropped with the frame rather than half-applied.
            QueuedMessagePool::return_message(message);
            return;
        }
        // Same ingest as handle_avatar_movement — process_message deep-copies and repacks.
        Self::enqueue(from_peer.id(), message);
    }

    /// Queues a frame that arrived by some path other than the avatar channels (the join
    /// ReadyMessage). `sequence` is the application-level sequence to stamp it with.
    pub fn add_message(from_peer: &NetPeerRef, local_message: LocalAvatarSyncMessage, sequence: u8) {
        Self::ensure_started();
        let mut message = QueuedMessagePool::rent();
        message.from_peer = Some(from_peer.clone());
        message.sequence = sequence;
        message.avatar_message = local_message;
        Self::enqueue(from_peer.id(), message);
    }
}
