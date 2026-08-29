//! `BasisServerReductionSystemEvents.Bundling.cs`: packs a receiver's pending sends into
//! compressed bundles that fit one datagram.

use std::sync::LazyLock;

use basis_network_core::compression::BasisAvatarBundleZstd;
use basis_network_core::{BasisNetworkCommons, NetPeerRef};

use super::{BasisServerReductionSystemEvents, now_ticks};
use crate::reduction::{BSRProfiler, BSRThreadCounters, PendingAvatarSend, ReceiverData};

const CHANNEL_HISTOGRAM_SIZE: usize = 64;

/// The order channel groups are written to the peer in, most expendable first.
///
/// This ordering is a delivery guarantee: the receiver's unreliable queue discards from the
/// FRONT when it is over budget, so whatever is written LAST is what survives an overloaded
/// tick. Groups go High → delta → Medium → Low → VeryLow, and the two id widths of a tier swap
/// places each tick so neither is permanently the one cut into.
static CHANNEL_FLUSH_ORDER: LazyLock<[[u8; CHANNEL_HISTOGRAM_SIZE]; 2]> =
    LazyLock::new(|| [BasisServerReductionSystemEvents::build_channel_flush_order(false), BasisServerReductionSystemEvents::build_channel_flush_order(true)]);

impl BasisServerReductionSystemEvents {
    pub(super) fn emit_greedy_bundles(recv: &mut ReceiverData, peer: &NetPeerRef, bundle_count: &mut i64, bundle_bytes: &mut i64) -> usize {
        let budget = peer.mtu() - Self::BUNDLE_MTU_HEADROOM - Self::BUNDLE_HEADER_SIZE as i32;
        if budget <= 0 {
            return 0;
        }
        let budget = budget as usize;
        let count = recv.pending_sends.len();
        let min_messages = usize::try_from(Self::avatar_bundle_min_messages()).unwrap_or(0);
        let min_bytes = usize::try_from(Self::avatar_bundle_min_bytes()).unwrap_or(0);
        let zstd_path = Self::zstd_path_available();
        let zstd_delta_bundles = Self::avatar_bundle_zstd_delta_bundles();

        let mut fill_margin = recv.bundle_fill_margin;
        if !(Self::MIN_BUNDLE_FILL_MARGIN..=Self::MAX_BUNDLE_FILL_MARGIN).contains(&fill_margin) {
            fill_margin = Self::MAX_BUNDLE_FILL_MARGIN;
        }

        let mut cursor = 0;
        while count - cursor >= min_messages.max(1) && count - cursor >= min_messages {
            // Codec is picked BEFORE the chunk is sized, because the size prediction needs the
            // ratio EMA belonging to the codec that will run.
            let mut use_zstd = zstd_path && (zstd_delta_bundles || recv.pending_sends[cursor].channel != BasisNetworkCommons::DELTA_AVATAR_CHANNEL);
            let mut ratio = if use_zstd { recv.last_bundle_zstd_ratio } else { recv.last_bundle_ratio };
            if !(0.05..=0.95).contains(&ratio) {
                ratio = if use_zstd { Self::INITIAL_BUNDLE_ZSTD_RATIO_GUESS } else { Self::INITIAL_BUNDLE_RATIO_GUESS };
            }
            let target_raw = ((budget as f32 * fill_margin) / ratio) as usize;
            let chunk_end = Self::pick_chunk_end(&recv.pending_sends, cursor, count, target_raw);
            if chunk_end <= cursor {
                break;
            }
            // Settle the class properly now the range is known.
            if use_zstd && !zstd_delta_bundles && Self::chunk_is_delta_only(&recv.pending_sends, cursor, chunk_end) {
                use_zstd = false;
            }
            let raw_len = Self::build_raw_for_range(recv, cursor, chunk_end);
            if raw_len < min_bytes {
                break;
            }
            match Self::try_deflate_and_emit(recv, peer, cursor, chunk_end, raw_len, budget, use_zstd, bundle_count, bundle_bytes) {
                Ok(compressed_len) => {
                    Self::update_ratio_ema(if use_zstd { &mut recv.last_bundle_zstd_ratio } else { &mut recv.last_bundle_ratio }, compressed_len, raw_len, 0.3);
                    cursor = chunk_end;
                    if fill_margin < Self::MAX_BUNDLE_FILL_MARGIN {
                        fill_margin = Self::MAX_BUNDLE_FILL_MARGIN.min(fill_margin + Self::BUNDLE_FILL_MARGIN_RECOVER);
                    }
                    continue;
                }
                Err(compressed_len) => {
                    // Overshoot — recompute the target from the observed ratio and retry smaller.
                    Self::update_ratio_ema(if use_zstd { &mut recv.last_bundle_zstd_ratio } else { &mut recv.last_bundle_ratio }, compressed_len, raw_len, 0.7);
                    fill_margin = Self::MIN_BUNDLE_FILL_MARGIN.max(fill_margin - Self::BUNDLE_FILL_MARGIN_BACKOFF);
                    let observed = (compressed_len as f32 / raw_len.max(1) as f32).clamp(0.05, 0.99);
                    let retry_target_raw = ((budget as f32 * 0.92) / observed) as usize;
                    let mut retry_end = Self::pick_chunk_end(&recv.pending_sends, cursor, chunk_end, retry_target_raw);
                    if retry_end >= chunk_end {
                        retry_end = cursor + ((chunk_end - cursor) * 3 / 4).max(1);
                    }
                    if retry_end <= cursor {
                        break;
                    }
                    let retry_use_zstd = use_zstd && (zstd_delta_bundles || !Self::chunk_is_delta_only(&recv.pending_sends, cursor, retry_end));
                    let retry_raw_len = Self::build_raw_for_range(recv, cursor, retry_end);
                    if retry_raw_len < min_bytes {
                        break;
                    }
                    if BSRProfiler::enabled() {
                        BSRProfiler::local(|c| BSRThreadCounters::add(&c.bundle_retries, 1));
                    }
                    match Self::try_deflate_and_emit(recv, peer, cursor, retry_end, retry_raw_len, budget, retry_use_zstd, bundle_count, bundle_bytes) {
                        Ok(retry_compressed) => {
                            Self::update_ratio_ema(
                                if retry_use_zstd { &mut recv.last_bundle_zstd_ratio } else { &mut recv.last_bundle_ratio },
                                retry_compressed,
                                retry_raw_len,
                                0.5,
                            );
                            cursor = retry_end;
                        }
                        // Two failures in a row — give up on bundling for this receiver this tick.
                        Err(_) => break,
                    }
                }
            }
        }
        recv.bundle_fill_margin = fill_margin;
        cursor
    }

    #[inline]
    fn pick_chunk_end(pending: &[PendingAvatarSend], cursor: usize, hard_end: usize, target_raw: usize) -> usize {
        let mut chunk_end = cursor;
        let mut raw_accum = 0usize;
        while chunk_end < hard_end {
            // Grouped layout: [len:2][bytes] per entry, plus a [chan:1][n:1] header each time the
            // channel changes.
            let mut entry_size = 2 + pending[chunk_end].length;
            if chunk_end == cursor || pending[chunk_end].channel != pending[chunk_end - 1].channel {
                entry_size += 2;
            }
            if chunk_end > cursor && raw_accum + entry_size > target_raw {
                break;
            }
            raw_accum += entry_size;
            chunk_end += 1;
        }
        chunk_end
    }

    pub(super) fn build_raw_for_range(recv: &mut ReceiverData, start: usize, end: usize) -> usize {
        let ReceiverData { pending_sends: pending, bundle_raw_scratch: raw, .. } = recv;
        // 4 not 3: worst case is one group per entry ([ch][n]) plus its [len:2].
        let upper_bound: usize = pending[start..end].iter().map(|p| 4 + p.length).sum();
        if raw.len() < upper_bound {
            raw.resize(upper_bound.max(4096), 0);
        }
        let mut raw_pos = 0usize;
        let mut i2 = start;
        while i2 < end {
            let channel = pending[i2].channel;
            // Extend the run while the channel holds; n is a byte on the wire, so a run is capped
            // at 255 and simply continues as a second group with the same channel.
            let mut run_end = i2;
            let mut n = 0usize;
            while run_end < end && pending[run_end].channel == channel && n < 255 {
                if pending[run_end].length > usize::from(pending[run_end].interval_offset) {
                    n += 1;
                }
                run_end += 1;
            }
            if n == 0 {
                i2 = run_end;
                continue;
            }
            raw[raw_pos] = channel;
            raw[raw_pos + 1] = n as u8;
            raw_pos += 2;
            for p in &pending[i2..run_end] {
                if p.length <= usize::from(p.interval_offset) {
                    continue;
                }
                raw[raw_pos..raw_pos + 2].copy_from_slice(&(p.length as u16).to_le_bytes());
                raw_pos += 2;
            }
            if channel != BasisNetworkCommons::DELTA_AVATAR_CHANNEL {
                for p in &pending[i2..run_end] {
                    if p.length <= usize::from(p.interval_offset) {
                        continue;
                    }
                    raw[raw_pos..raw_pos + p.length].copy_from_slice(&p.source[..p.length]);
                    // Patch the per-receiver interval byte in our copy (source is shared).
                    raw[raw_pos + usize::from(p.interval_offset)] = p.interval;
                    raw_pos += p.length;
                }
            } else {
                // Delta groups are written column-major (byte j of every entry, then byte j+1) so
                // the codec sees the shared header structure lined up.
                let max_len = pending[i2..run_end].iter().filter(|p| p.length > usize::from(p.interval_offset)).map(|p| p.length).max().unwrap_or(0);
                for j in 0..max_len {
                    for p in &pending[i2..run_end] {
                        if p.length <= usize::from(p.interval_offset) || j >= p.length {
                            continue;
                        }
                        raw[raw_pos] = if j == usize::from(p.interval_offset) { p.interval } else { p.source[j] };
                        raw_pos += 1;
                    }
                }
            }
            i2 = run_end;
        }
        raw_pos
    }

    fn build_channel_flush_order(large_id_first: bool) -> [u8; CHANNEL_HISTOGRAM_SIZE] {
        let mut avatar = [0u8; 17];
        let mut written = 0;
        for quality in (0..=3u8).rev() {
            let small = BasisNetworkCommons::PLAYER_AVATAR_VERY_LOW_CHANNEL + quality * 2;
            let large = BasisNetworkCommons::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL + quality * 2;
            let (first, second) = if large_id_first { (large, small) } else { (small, large) };
            avatar[written] = first;
            avatar[written + 1] = first + 1;
            avatar[written + 2] = second;
            avatar[written + 3] = second + 1;
            written += 4;
            if quality == 3 {
                avatar[written] = BasisNetworkCommons::DELTA_AVATAR_CHANNEL;
                written += 1;
            }
        }
        let mut is_avatar = [false; CHANNEL_HISTOGRAM_SIZE];
        for channel in avatar {
            is_avatar[usize::from(channel)] = true;
        }
        let mut order = [0u8; CHANNEL_HISTOGRAM_SIZE];
        let mut position = 0;
        for (channel, avatar_channel) in is_avatar.iter().enumerate() {
            if !avatar_channel {
                order[position] = channel as u8;
                position += 1;
            }
        }
        for channel in avatar {
            order[position] = channel;
            position += 1;
        }
        order
    }

    /// Groups a receiver's pending sends by channel so each bundle carries a few long runs
    /// instead of an interleaved stream. Sorts in place from the caller's point of view.
    pub(super) fn sort_pending_by_channel(recv: &mut ReceiverData, count: usize, sender_rotation: u32) {
        if count < 2 {
            return;
        }
        let ReceiverData { pending_sends: pending, pending_sort_scratch: dst, .. } = recv;
        let mut offsets = [0usize; CHANNEL_HISTOGRAM_SIZE];
        for p in &pending[..count] {
            if usize::from(p.channel) >= CHANNEL_HISTOGRAM_SIZE {
                return; // not an avatar channel — leave as-is
            }
            offsets[usize::from(p.channel)] += 1;
        }
        let flush_order = &CHANNEL_FLUSH_ORDER[(sender_rotation & 1) as usize];
        let mut running = 0;
        for &c in flush_order.iter() {
            let n = offsets[usize::from(c)];
            offsets[usize::from(c)] = running;
            running += n;
        }
        dst.clear();
        dst.extend(pending[..count].iter().cloned());
        for p in dst.iter() {
            let slot = &mut offsets[usize::from(p.channel)];
            pending[*slot] = p.clone();
            *slot += 1;
        }
        // Drop the scratch's copies of the source references; see the field's doc comment.
        dst.clear();
    }

    /// `Ok(compressed_len)` when the chunk was emitted; `Err(compressed_len)` when it did not
    /// fit the budget and the caller should retry with a smaller chunk.
    #[allow(clippy::too_many_arguments)]
    fn try_deflate_and_emit(
        recv: &mut ReceiverData,
        peer: &NetPeerRef,
        chunk_start: usize,
        chunk_end: usize,
        raw_len: usize,
        budget: usize,
        use_zstd: bool,
        bundle_count: &mut i64,
        bundle_bytes: &mut i64,
    ) -> Result<usize, usize> {
        // rawLen rides the bundle header as a u16, so an oversized chunk cannot be framed at all.
        if raw_len > usize::from(u16::MAX) {
            return Err(raw_len);
        }
        let ReceiverData { bundle_raw_scratch: raw, bundle_compressed_scratch: compressed, .. } = recv;
        // Size for whichever codec could run so an incompressible chunk cannot overrun the scratch.
        let comp_capacity_needed = Self::BUNDLE_HEADER_SIZE
            + lz4_flex::block::get_maximum_output_size(raw_len).max(if use_zstd { BasisAvatarBundleZstd::maximum_output_size(raw_len) } else { 0 });
        if compressed.len() < comp_capacity_needed {
            compressed.resize(comp_capacity_needed.max(4096), 0);
        }
        let profiling = BSRProfiler::enabled();
        let deflate_start = if profiling { now_ticks() } else { 0 };

        // Encode directly into the wire packet's payload region.
        let (codec, compressed_len) = {
            let (header, payload) = compressed.split_at_mut(Self::BUNDLE_HEADER_SIZE);
            let _ = header;
            let zstd_len = if use_zstd { BasisAvatarBundleZstd::try_compress(&raw[..raw_len], payload) } else { None };
            match zstd_len {
                Some(len) => (BasisAvatarBundleZstd::CODEC_ZSTD_DICT, len),
                // LZ4 is always a valid encoding of any bundle body.
                None => (BasisAvatarBundleZstd::CODEC_LZ4, lz4_flex::block::compress_into(&raw[..raw_len], payload).unwrap_or(0)),
            }
        };
        let deflate_ticks = if profiling { now_ticks() - deflate_start } else { 0 };
        if profiling {
            BSRProfiler::local(|c| BSRThreadCounters::add(&c.bundle_deflate_ticks, deflate_ticks));
        }
        if compressed_len == 0 || compressed_len > budget {
            return Err(compressed_len);
        }
        let wire_len = Self::BUNDLE_HEADER_SIZE + compressed_len;
        let chunk_count = (chunk_end - chunk_start) as i64;
        compressed[0] = BasisAvatarBundleZstd::pack_flags(codec, if codec == BasisAvatarBundleZstd::CODEC_ZSTD_DICT { BasisAvatarBundleZstd::dictionary_generation() } else { 0 });
        compressed[1..3].copy_from_slice(&(raw_len as u16).to_le_bytes());

        if peer.send_unreliable_raw_merge(compressed, 0, wire_len, BasisNetworkCommons::COMPRESSED_AVATAR_BUNDLE_CHANNEL, -1, 0).is_err() {
            return Err(compressed_len);
        }
        *bundle_count += 1;
        *bundle_bytes += wire_len as i64;
        if profiling {
            BSRProfiler::local(|c| {
                BSRThreadCounters::add(&c.bundles_emitted, 1);
                BSRThreadCounters::add(&c.bundle_messages, chunk_count);
                BSRThreadCounters::add(&c.bundle_raw_bytes, raw_len as i64);
                BSRThreadCounters::add(&c.bundle_compressed_bytes, compressed_len as i64);
                if codec == BasisAvatarBundleZstd::CODEC_ZSTD_DICT {
                    BSRThreadCounters::add(&c.bundle_zstd_emitted, 1);
                    BSRThreadCounters::add(&c.bundle_zstd_raw_bytes, raw_len as i64);
                    BSRThreadCounters::add(&c.bundle_zstd_compressed_bytes, compressed_len as i64);
                    BSRThreadCounters::add(&c.bundle_zstd_ticks, deflate_ticks);
                }
            });
        }
        Ok(compressed_len)
    }

    fn chunk_is_delta_only(pending: &[PendingAvatarSend], start: usize, end: usize) -> bool {
        pending[start..end].iter().all(|p| p.channel == BasisNetworkCommons::DELTA_AVATAR_CHANNEL)
    }

    fn zstd_path_available() -> bool {
        Self::enable_avatar_bundle_zstd() && BasisAvatarBundleZstd::available() && Self::load_shed_tier() <= Self::avatar_bundle_zstd_max_shed_tier()
    }

    #[inline]
    fn update_ratio_ema(ema: &mut f32, compressed: usize, raw: usize, weight_on_observed: f32) {
        if raw == 0 {
            return;
        }
        let observed = (compressed as f32 / raw as f32).clamp(0.05, 0.99);
        let mut prev = *ema;
        if !(0.05..=0.95).contains(&prev) {
            prev = observed; // unseeded → adopt
        }
        *ema = prev * (1.0 - weight_on_observed) + observed * weight_on_observed;
    }
}
