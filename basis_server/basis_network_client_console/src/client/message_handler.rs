//! Port of `MessageHandler.cs`: what a simulated client does with what the server sends it —
//! answer the auth challenge, note who is audible, verify face data end to end, and keep the
//! per-sender fairness counts.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Instant;

use basis_network_core::BNL;
use basis_network_core::SerializableBasis::{LocalAvatarSyncMessage, ServerAvatarDataMessage, ServerSideSyncPlayerMessage};
use basis_network_core::compression::{BasisAvatarBitPacking, BasisAvatarBundleCodec, BasisAvatarBundleZstd, BasisAvatarDeltaCompression, BitQuality};
use basis_network_core::transport::basis_network_shell::DisconnectInfo;
use basis_network_core::{BasisNetworkCommons, DeliveryMethod, NetDataReader, NetPacketReader, NetPeerRef};
use dashmap::DashMap;

use crate::audio::voice_delivery_stats::VoiceDeliveryStats;
use crate::client::client_manager::ConsoleClientIdentity;
use crate::client::config_manager::ConfigManager;
use crate::client::movement_sender::{MovementSender, VoiceSender};
use crate::diagnostics::bundle_capture_sink::BundleCaptureSink;

// ── Face-data observer (BASIS_EMIT_FACE / BASIS_FACE_OBSERVE_ONLY test modes) ──
// Counts every avatar frame per downlink path and verifies the counter embedded in the synthetic
// face payload is strictly increasing per (observer, sender) pair, so a run proves both delivery
// and ordering of AdditionalAvatarData end to end.
static OBSERVE_ONLY: AtomicBool = AtomicBool::new(false);
pub static POSE_ONLY_KEYFRAMES: AtomicI64 = AtomicI64::new(0); // even avatar channels (no additional section)
pub static FACE_KEYFRAMES_SMALL: AtomicI64 = AtomicI64::new(0); // odd byte-id channels (7/9/11/13)
pub static FACE_KEYFRAMES_LARGE: AtomicI64 = AtomicI64::new(0); // odd ushort-id channels (42/44/46/48)
pub static FACE_DELTAS: AtomicI64 = AtomicI64::new(0); // DeltaAvatarChannel frames with the additional bit
pub static POSE_ONLY_DELTAS: AtomicI64 = AtomicI64::new(0); // DeltaAvatarChannel frames without it
pub static FACE_VIA_BUNDLE_KEYFRAMES: AtomicI64 = AtomicI64::new(0); // inner keyframes inside channel-52 bundles
pub static FACE_VIA_BUNDLE_DELTAS: AtomicI64 = AtomicI64::new(0); // inner deltas inside channel-52 bundles
pub static BUNDLES_PARSED: AtomicI64 = AtomicI64::new(0);
pub static UPLINK_NACKS_RECEIVED: AtomicI64 = AtomicI64::new(0); // server asked us to re-key (lost uplink baseline)
pub static MONOTONIC_VIOLATIONS: AtomicI64 = AtomicI64::new(0); // face counter went backwards for a pair
pub static PARSE_FAILURES: AtomicI64 = AtomicI64::new(0);
pub static LARGE_SENDER_FACE_RECEIPTS: AtomicI64 = AtomicI64::new(0); // receipts whose sender id needs a ushort (>255)
pub static AVATAR_CHANNEL_TOTAL: AtomicI64 = AtomicI64::new(0);

static LAST_FACE_LOG_TICKS: AtomicI64 = AtomicI64::new(i64::MIN);
static LAST_COUNTER_PER_PAIR: std::sync::LazyLock<DashMap<i64, i32>> = std::sync::LazyLock::new(DashMap::new);
/// AvatarChannel (15) receipts, keyed by the first payload byte — for HVR that byte is the packet id.
static AVATAR_CHANNEL_BY_PACKET_ID: std::sync::LazyLock<DashMap<i32, i64>> = std::sync::LazyLock::new(DashMap::new);
static CLOCK: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

// Counts inbound avatar frames per sender across the whole crowd. A server that is over capacity
// has to drop something, but it should thin everyone out evenly — the spread of these counts shows
// the difference; an aggregate total cannot.
static SENDER_SEEN: [AtomicI64; u16::MAX as usize + 1] = [const { AtomicI64::new(0) }; u16::MAX as usize + 1];

fn inc(counter: &AtomicI64) -> i64 {
    counter.fetch_add(1, Ordering::Relaxed) + 1
}

pub struct MessageHandler;

impl MessageHandler {
    pub fn observe_only() -> bool {
        OBSERVE_ONLY.load(Ordering::Relaxed)
    }

    pub fn set_observe_only(value: bool) {
        OBSERVE_ONLY.store(value, Ordering::Relaxed);
    }

    // Bundle capture needs sniff_bundle to run (that is where the decoded body exists), so it
    // turns sniffing on by itself rather than making the operator remember to pair the flags.
    fn sniffing() -> bool {
        MovementSender::emit_face_data() || Self::observe_only() || BundleCaptureSink::enabled()
    }

    pub fn reset_stats() {
        for c in [
            &POSE_ONLY_KEYFRAMES,
            &FACE_KEYFRAMES_SMALL,
            &FACE_KEYFRAMES_LARGE,
            &FACE_DELTAS,
            &POSE_ONLY_DELTAS,
            &FACE_VIA_BUNDLE_KEYFRAMES,
            &FACE_VIA_BUNDLE_DELTAS,
            &BUNDLES_PARSED,
            &UPLINK_NACKS_RECEIVED,
            &MONOTONIC_VIOLATIONS,
            &PARSE_FAILURES,
            &LARGE_SENDER_FACE_RECEIPTS,
        ] {
            c.store(0, Ordering::Relaxed);
        }
        LAST_COUNTER_PER_PAIR.clear();
    }

    pub fn total_face_receipts() -> i64 {
        FACE_KEYFRAMES_SMALL.load(Ordering::Relaxed) + FACE_KEYFRAMES_LARGE.load(Ordering::Relaxed) + FACE_DELTAS.load(Ordering::Relaxed) + FACE_VIA_BUNDLE_KEYFRAMES.load(Ordering::Relaxed) + FACE_VIA_BUNDLE_DELTAS.load(Ordering::Relaxed)
    }

    pub fn summary() -> String {
        let r = |c: &AtomicI64| c.load(Ordering::Relaxed);
        format!(
            "[FaceObserver] face: kfSmall={} kfLarge={} delta={} bundleKf={} bundleDelta={} | pose-only: kf={} delta={} | bundles={} nacks={} largeSenderFace={} | violations={} parseFail={}",
            r(&FACE_KEYFRAMES_SMALL),
            r(&FACE_KEYFRAMES_LARGE),
            r(&FACE_DELTAS),
            r(&FACE_VIA_BUNDLE_KEYFRAMES),
            r(&FACE_VIA_BUNDLE_DELTAS),
            r(&POSE_ONLY_KEYFRAMES),
            r(&POSE_ONLY_DELTAS),
            r(&BUNDLES_PARSED),
            r(&UPLINK_NACKS_RECEIVED),
            r(&LARGE_SENDER_FACE_RECEIPTS),
            r(&MONOTONIC_VIOLATIONS),
            r(&PARSE_FAILURES)
        )
    }

    pub fn on_disconnect(peer: &NetPeerRef, _info: &DisconnectInfo) {
        BNL::log_error(format!("Peer {} disconnected.", peer.id()));
    }

    pub fn on_receive(identity: &ConsoleClientIdentity, client_index: usize, peer: &NetPeerRef, mut reader: NetPacketReader, channel: u8, _method: DeliveryMethod) {
        // A client has exactly one connection — the server — so every message here is from it.
        match channel {
            BasisNetworkCommons::AUTH_IDENTITY_CHANNEL => Self::auth_identity_message(identity, peer, &mut reader),
            BasisNetworkCommons::META_DATA_CHANNEL => identity.set_authenticated(true),
            BasisNetworkCommons::DELTA_AVATAR_CHANNEL => {
                if reader.available_bytes() >= 1 && reader.peek_byte().ok() == Some(BasisNetworkCommons::DELTA_CONTROL_UPLINK_KEYFRAME_REQUEST) {
                    inc(&UPLINK_NACKS_RECEIVED);
                    MovementSender::request_keyframe(client_index);
                } else if Self::sniffing() {
                    let (raw, pos, avail) = (reader.raw_data(), reader.position(), reader.available_bytes());
                    Self::sniff_delta(client_index, raw, pos, avail, false);
                }
            }
            BasisNetworkCommons::PLAYER_AVATAR_VERY_LOW_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_LOW_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_MEDIUM_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_LOW_LARGE_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_MEDIUM_LARGE_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_HIGH_LARGE_CHANNEL => {
                inc(&POSE_ONLY_KEYFRAMES);
                Self::note_voice_range(client_index, &reader, channel);
            }
            BasisNetworkCommons::PLAYER_AVATAR_VERY_LOW_ADDITIONAL_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_LOW_ADDITIONAL_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_MEDIUM_ADDITIONAL_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_HIGH_ADDITIONAL_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_VERY_LOW_ADDITIONAL_LARGE_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_LOW_ADDITIONAL_LARGE_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_MEDIUM_ADDITIONAL_LARGE_CHANNEL
            | BasisNetworkCommons::PLAYER_AVATAR_HIGH_ADDITIONAL_LARGE_CHANNEL => {
                if Self::sniffing() {
                    let (raw, pos, avail) = (reader.raw_data(), reader.position(), reader.available_bytes());
                    Self::sniff_keyframe(client_index, raw, pos, avail, channel, false);
                }
            }
            BasisNetworkCommons::COMPRESSED_AVATAR_BUNDLE_CHANNEL => {
                if Self::sniffing() {
                    Self::sniff_bundle(client_index, &reader);
                }
            }
            BasisNetworkCommons::VOICE_CHANNEL => Self::note_voice_delivery(client_index, &reader, false),
            BasisNetworkCommons::VOICE_LARGE_CHANNEL => Self::note_voice_delivery(client_index, &reader, true),
            // HVR's reliable/low-frequency path — counting these splits "HVR networking never
            // started" from "only the high-freq path is dead".
            BasisNetworkCommons::AVATAR_CHANNEL if Self::sniffing() => Self::sniff_avatar_channel(client_index, &mut reader),
            _ => {}
        }
    }

    fn sniff_avatar_channel(_client_index: usize, reader: &mut NetDataReader) {
        let mut sadm = ServerAvatarDataMessage::default();
        if let Err(e) = sadm.deserialize(reader) {
            inc(&PARSE_FAILURES);
            BNL::log_error(format!("[FaceObserver] ch15 sniff failed: {e}"));
            return;
        }
        let total = inc(&AVATAR_CHANNEL_TOTAL);
        let payload = sadm.avatar_data_message.payload.as_deref().unwrap_or(&[]);
        let packet_id = payload.first().map(|b| *b as i32).unwrap_or(-1);
        *AVATAR_CHANNEL_BY_PACKET_ID.entry(packet_id).or_insert(0) += 1;

        if total <= 5 || total % 50 == 0 {
            let mut totals: Vec<(i32, i64)> = AVATAR_CHANNEL_BY_PACKET_ID.iter().map(|kv| (*kv.key(), *kv.value())).collect();
            totals.sort_by_key(|(k, _)| *k);
            let totals: Vec<String> = totals.iter().map(|(k, v)| format!("id{k}={v}")).collect();
            BNL::log(format!(
                "[FaceObserver] ch15 from player {} msgIndex={} packetId={packet_id} bytes={} | ch15 totals: {}",
                sadm.player_id_message.player_id,
                sadm.avatar_data_message.message_index,
                payload.len(),
                totals.join(", ")
            ));
        }
    }

    /// Decodes one channel-52 bundle exactly like the Unity client: [count:1][rawLen:2-LE][body],
    /// flattened through the shared BasisAvatarBundleCodec, then routes each inner message through
    /// the same keyframe/delta sniffers.
    fn sniff_bundle(client_index: usize, reader: &NetDataReader) {
        if reader.available_bytes() < 3 {
            return;
        }
        let raw = reader.raw_data();
        let pos = reader.position();
        let flags = raw[pos];
        let raw_len = u16::from_le_bytes([raw[pos + 1], raw[pos + 2]]) as usize;
        let compressed_len = reader.available_bytes() - 3;
        if raw_len == 0 || compressed_len == 0 {
            return;
        }
        let compressed = &raw[pos + 3..pos + 3 + compressed_len];

        let mut grouped = vec![0u8; raw_len];
        let decoded = if BasisAvatarBundleZstd::codec_of(flags) == BasisAvatarBundleZstd::CODEC_ZSTD_DICT {
            if BasisAvatarBundleZstd::dict_generation_of(flags) != BasisAvatarBundleZstd::dictionary_generation() {
                // Wrong dictionary generation — the payload is undecodable here and counting it as
                // a parse failure is the point: the load-tester and the server were built from
                // different dictionaries.
                inc(&PARSE_FAILURES);
                return;
            }
            BasisAvatarBundleZstd::try_decompress(compressed, &mut grouped).unwrap_or(0)
        } else {
            lz4_flex::block::decompress_into(compressed, &mut grouped).unwrap_or(0)
        };
        if decoded != raw_len {
            inc(&PARSE_FAILURES);
            return;
        }

        // The grouped body here is byte-for-byte what the server compressed, which makes this the
        // natural place to harvest dictionary training samples.
        BundleCaptureSink::capture(&grouped[..decoded], compressed_len, BasisAvatarBundleZstd::codec_of(flags));

        let mut scratch = vec![0u8; BasisAvatarBundleCodec::max_flat_size(decoded)];
        let Some(flat_len) = BasisAvatarBundleCodec::try_flatten(&grouped[..decoded], &mut scratch) else {
            inc(&PARSE_FAILURES);
            return;
        };
        let bundle_number = inc(&BUNDLES_PARSED);

        // Contents of the first few bundles, for the run report.
        if bundle_number <= 5 {
            let mut channels_seen = String::new();
            let mut probe = 0usize;
            while probe + 3 <= flat_len {
                let ch = scratch[probe];
                let len = u16::from_le_bytes([scratch[probe + 1], scratch[probe + 2]]) as usize;
                if len == 0 || probe + 3 + len > flat_len {
                    break;
                }
                channels_seen.push_str(&format!("{ch}:{len} "));
                probe += 3 + len;
            }
            BNL::log(format!("[FaceObserver] bundle -> {channels_seen}"));
        }

        let mut offset = 0usize;
        while offset + 3 <= flat_len {
            let inner_channel = scratch[offset];
            let msg_len = u16::from_le_bytes([scratch[offset + 1], scratch[offset + 2]]) as usize;
            offset += 3;
            if msg_len == 0 || offset + msg_len > flat_len {
                break;
            }
            if inner_channel == BasisNetworkCommons::DELTA_AVATAR_CHANNEL {
                Self::sniff_delta(client_index, &scratch, offset, msg_len, true);
            } else if BasisNetworkCommons::channel_has_additional_data(inner_channel) {
                Self::sniff_keyframe(client_index, &scratch, offset, msg_len, inner_channel, true);
            } else {
                inc(&POSE_ONLY_KEYFRAMES);
            }
            offset += msg_len;
        }
    }

    /// Parses one per-quality keyframe frame the way the real client does and records its
    /// additional data.
    fn sniff_keyframe(client_index: usize, buffer: &[u8], start: usize, length: usize, channel: u8, via_bundle: bool) {
        let Some(slice) = buffer.get(start..start + length) else {
            inc(&PARSE_FAILURES);
            return;
        };
        let mut inner = NetDataReader::from_slice(slice);
        let mut ssm = ServerSideSyncPlayerMessage::default();
        let parsed = ssm.deserialize_for_channel_sized(
            &mut inner,
            BasisNetworkCommons::get_quality_from_channel(channel),
            BasisNetworkCommons::channel_has_additional_data(channel),
            BasisNetworkCommons::is_large_player_id_channel(channel),
        );
        if let Err(e) = parsed {
            inc(&PARSE_FAILURES);
            BNL::log_error(format!("[FaceObserver] keyframe sniff failed on ch{channel}: {e}"));
            return;
        }
        if inner.available_bytes() != 0 {
            inc(&PARSE_FAILURES);
            BNL::log_error(format!("[FaceObserver] keyframe on ch{channel} left {} unread bytes", inner.available_bytes()));
            return;
        }
        if via_bundle {
            inc(&FACE_VIA_BUNDLE_KEYFRAMES);
        } else if BasisNetworkCommons::is_large_player_id_channel(channel) {
            inc(&FACE_KEYFRAMES_LARGE);
        } else {
            inc(&FACE_KEYFRAMES_SMALL);
        }
        Self::report_additional(client_index, ssm.player_id_message.player_id, &ssm.avatar_serialization, if via_bundle { "BUNDLE-KF" } else { "KEYFRAME" });
    }

    /// Parses a downlink delta frame far enough to reach its additional-data tail (no baseline
    /// needed — the delta body is self-delimiting) and records what rode along.
    fn sniff_delta(client_index: usize, buffer: &[u8], start: usize, length: usize, via_bundle: bool) {
        let Some(slice) = buffer.get(start..start + length) else {
            inc(&PARSE_FAILURES);
            return;
        };
        let mut inner = NetDataReader::from_slice(slice);
        let Some(header) = inner.try_get_byte() else { return };
        if BasisNetworkCommons::is_delta_control_header(header) {
            return;
        }
        let quality = BasisNetworkCommons::delta_header_quality(header);
        let Some(q) = BitQuality::from_byte(quality) else { return };
        if !BasisAvatarBitPacking::is_valid_quality(quality) {
            return;
        }
        let has_additional = BasisNetworkCommons::delta_header_has_additional_data(header);
        let large_id = BasisNetworkCommons::delta_header_large_id(header);

        let player_id = if large_id {
            let Some(id) = inner.try_get_ushort() else { return };
            id
        } else {
            let Some(b) = inner.try_get_byte() else { return };
            b as u16
        };
        if inner.try_get_byte().is_none() {
            return; // interval
        }
        if inner.try_get_byte().is_none() {
            return; // sequence
        }
        if inner.try_get_byte().is_none() {
            return; // baseSeq
        }

        let Some(body_len) = BasisAvatarDeltaCompression::delta_body_length(inner.raw_data(), inner.position(), inner.available_bytes(), q) else {
            inc(&PARSE_FAILURES);
            return;
        };
        if body_len > inner.available_bytes() {
            inc(&PARSE_FAILURES);
            return;
        }
        inner.skip_bytes(body_len);

        if !has_additional {
            inc(&POSE_ONLY_DELTAS);
            if inner.available_bytes() != 0 {
                inc(&PARSE_FAILURES);
            }
            return;
        }

        let mut lasm = LocalAvatarSyncMessage::default();
        if let Err(e) = lasm.deserialize_additional_data(&mut inner) {
            inc(&PARSE_FAILURES);
            BNL::log_error(format!("[FaceObserver] delta sniff failed: {e}"));
            return;
        }
        if inner.available_bytes() != 0 {
            inc(&PARSE_FAILURES);
            BNL::log_error(format!("[FaceObserver] delta left {} unread bytes after additional section", inner.available_bytes()));
            return;
        }
        if via_bundle {
            inc(&FACE_VIA_BUNDLE_DELTAS);
        } else {
            inc(&FACE_DELTAS);
        }
        Self::report_additional(client_index, player_id, &lasm, if via_bundle { "BUNDLE-DELTA" } else { "DELTA" });
    }

    fn report_additional(client_index: usize, from_player: u16, lasm: &LocalAvatarSyncMessage, path: &str) {
        let Some(datas) = lasm.additional_avatar_datas.as_ref().filter(|d| !d.is_empty() && lasm.additional_avatar_data_size != 0) else {
            inc(&PARSE_FAILURES);
            BNL::log_error(format!("[FaceObserver] {path} frame flagged additional but section was empty"));
            return;
        };

        if from_player > u8::MAX as u16 {
            inc(&LARGE_SENDER_FACE_RECEIPTS);
        }

        let ad = &datas[0];
        let counter = match ad.array.as_deref() {
            Some(a) if a.len() >= 4 => (a[2] as i32) | ((a[3] as i32) << 8),
            _ => -1,
        };

        // Strictly-increasing check per (observer, sender). Counters wrap at 65536 — treat a huge
        // backward jump as the wrap, anything else as a violation.
        if counter >= 0 {
            let key = ((client_index as i64) << 32) | from_player as i64;
            let mut entry = LAST_COUNTER_PER_PAIR.entry(key).or_insert(counter);
            let prev = *entry;
            if prev != counter && counter <= prev && prev - counter < 30000 {
                inc(&MONOTONIC_VIOLATIONS);
                BNL::log_error(format!("[FaceObserver] counter regressed for observer#{client_index} sender {from_player}: {prev} -> {counter} ({path})"));
            }
            *entry = counter;
        }

        // Log at most ~1/s so a healthy stream doesn't flood the console.
        let now = CLOCK.elapsed().as_nanos() as i64;
        let last_log = LAST_FACE_LOG_TICKS.load(Ordering::Relaxed);
        if now.saturating_sub(last_log) < 1_000_000_000 {
            return;
        }
        if LAST_FACE_LOG_TICKS.compare_exchange(last_log, now, Ordering::Relaxed, Ordering::Relaxed).is_err() {
            return;
        }
        BNL::log(format!("[FaceObserver] client#{client_index} sender={from_player} via {path} counter={counter} linked={} | {}", lasm.linked_avatar_index, Self::summary()));
    }

    pub fn auth_identity_message(identity: &ConsoleClientIdentity, peer: &NetPeerRef, reader: &mut NetDataReader) {
        match identity.try_respond_to_challenge(reader) {
            Some(writer) => {
                if let Err(e) = peer.send_writer(&writer, BasisNetworkCommons::AUTH_IDENTITY_CHANNEL, DeliveryMethod::ReliableOrdered) {
                    BNL::log_error(format!("Failed to send the auth response: {e}"));
                }
            }
            None => BNL::log_error("Failed to respond to auth challenge!"),
        }
    }

    /// Voice range, derived from the server's own distance tiering instead of decoding positions.
    /// High and Medium avatar quality are only sent to peers inside MediumQualityDistance, which
    /// is the voice radius, so receiving that tier is proof the sender is close enough to hear.
    fn note_voice_range(client_index: usize, reader: &NetDataReader, channel: u8) {
        if !ConfigManager::current().simulate_voice {
            return;
        }
        let near_tier = matches!(
            channel,
            BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL | BasisNetworkCommons::PLAYER_AVATAR_MEDIUM_CHANNEL | BasisNetworkCommons::PLAYER_AVATAR_HIGH_LARGE_CHANNEL | BasisNetworkCommons::PLAYER_AVATAR_MEDIUM_LARGE_CHANNEL
        );
        if !near_tier {
            return;
        }
        let large = BasisNetworkCommons::is_large_player_id_channel(channel);
        let pos = reader.position();
        let raw = reader.raw_data();
        if pos + if large { 2 } else { 1 } > raw.len() {
            return;
        }
        let player_id = if large { u16::from_le_bytes([raw[pos], raw[pos + 1]]) } else { raw[pos] as u16 };
        VoiceSender::note_audible(client_index, player_id);
        Self::note_sender_seen(player_id);
    }

    /// Books one relayed voice frame against its sender's sequence, which is what turns "the
    /// server says it sent voice" into "this receiver could actually have played it".
    /// Wire: [playerId:1|2][sequence:1][silence:1][opus].
    fn note_voice_delivery(client_index: usize, reader: &NetDataReader, large_id: bool) {
        if !VoiceDeliveryStats::enabled() {
            return;
        }
        let pos = reader.position();
        let raw = reader.raw_data();
        let id_bytes = if large_id { 2 } else { 1 };
        if pos + id_bytes + 1 > raw.len() {
            return;
        }
        let sender_id = if large_id { u16::from_le_bytes([raw[pos], raw[pos + 1]]) as i32 } else { raw[pos] as i32 };
        let sequence = raw[pos + id_bytes];
        VoiceDeliveryStats::note(client_index, sender_id, sequence);
    }

    pub fn note_sender_seen(player_id: u16) {
        SENDER_SEEN[player_id as usize].fetch_add(1, Ordering::Relaxed);
    }

    /// Distribution of received frames per sender — the fairness check.
    pub fn sender_fairness() -> String {
        let mut counts: Vec<i64> = SENDER_SEEN.iter().map(|c| c.load(Ordering::Relaxed)).filter(|c| *c > 0).collect();
        if counts.is_empty() {
            return "[Fairness] no avatar frames seen yet.".to_string();
        }
        counts.sort_unstable();
        let total: i64 = counts.iter().sum();
        let mean = total as f64 / counts.len() as f64;
        let variance: f64 = counts.iter().map(|c| (*c as f64 - mean).powi(2)).sum::<f64>() / counts.len() as f64;
        let stddev = variance.sqrt();
        let p01 = counts[(counts.len() as f64 * 0.01) as usize];
        let p50 = counts[counts.len() / 2];
        let p99 = counts[(counts.len() - 1).min((counts.len() as f64 * 0.99) as usize)];
        // Starved = receiving under a tenth of the median. On a fairly-degrading server this is
        // zero however hard it is shedding.
        let starved = counts.iter().filter(|c| **c < p50 / 10).count();
        format!(
            "[Fairness] {} senders seen | min={} p1={p01} median={p50} p99={p99} max={} | stddev/mean={:.2} | starved(<10% of median)={starved}",
            counts.len(),
            counts[0],
            counts[counts.len() - 1],
            if mean > 0.0 { stddev / mean } else { 0.0 }
        )
    }
}
