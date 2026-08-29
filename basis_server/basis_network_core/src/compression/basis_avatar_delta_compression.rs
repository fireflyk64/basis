use std::sync::LazyLock;

use super::basis_avatar_bit_packing::BitQuality;
use super::basis_avatar_channel_map::{BasisAvatarChannel, BasisAvatarChannelLayout, BasisAvatarChannelMap, BasisChannelKind};
use super::basis_bit_codec::BasisBitCodec;
use super::basis_bone_rotation_compression::BasisBoneRotationCompression;
use super::basis_payload_diff::BasisPayloadDiff;
use super::basis_residual_codec::{BasisResidualCodec, BitReader, BitWriter};

/// Avatar delta compression against a keyframe baseline. A keyframe is the full fixed-size
/// payload; a delta encodes only the fields that changed since the last keyframe, preceded by a
/// per-field dirty mask. Deltas reference the keyframe, never the previous frame, so a single
/// delta reconstructs the full pose on its own regardless of which intermediate frames a given
/// receiver was never sent or lost.
///
/// Delta body layout:
///   [dirtyMask : DIRTY_MASK_BYTES][bitstream, LSB-first]
/// and within the bitstream, for each dirty field in field order:
///   [1 bit mode][raw: every channel verbatim | residual: Raw channels verbatim, Delta channels as se(v)]
pub struct BasisAvatarDeltaCompression;

struct QualityGeometry {
    layout: &'static BasisAvatarChannelLayout,
    payload_size: usize,
    max_delta_size: usize,
}

static GEO: LazyLock<[QualityGeometry; 4]> = LazyLock::new(|| {
    let build = |q: BitQuality| {
        let layout = BasisAvatarChannelMap::for_quality(q);
        // Raw mode caps every field at its own verbatim width, so the worst case is one mode
        // bit per field plus the whole payload — five bytes over the old fixed bound.
        let max_body_bits = BasisAvatarDeltaCompression::FIELD_COUNT + layout.total_channel_bits;
        QualityGeometry {
            layout,
            payload_size: layout.payload_bytes,
            max_delta_size: BasisAvatarDeltaCompression::DIRTY_MASK_BYTES + ((max_body_bits + 7) >> 3),
        }
    };
    [build(BitQuality::VeryLow), build(BitQuality::Low), build(BitQuality::Medium), build(BitQuality::High)]
});

impl BasisAvatarDeltaCompression {
    pub const BONE_FIELD_START: usize = 1;
    pub const FIELD_COUNT: usize = 1 + BasisBoneRotationCompression::ROTATION_FIELD_COUNT + 5; // 37 (incl. end-effector)
    pub const DIRTY_MASK_BYTES: usize = (Self::FIELD_COUNT + 7) >> 3; // 5

    const MODE_RESIDUAL: u32 = 0;
    const MODE_RAW: u32 = 1;

    pub fn payload_size(q: BitQuality) -> usize {
        GEO[q.index()].payload_size
    }

    /// Worst-case delta body length for a quality. Callers size scratch buffers with this.
    pub fn max_delta_size(q: BitQuality) -> usize {
        GEO[q.index()].max_delta_size
    }

    /// Builds a delta of `current` against `keyframe` into `dst` at `dst_start`. Both payloads
    /// must be at least `payload_size(q)` bytes; dst must have room for `max_delta_size(q)` from
    /// dst_start. Returns the delta body length written, or `None` on bad input (the C# -1).
    pub fn build_delta(keyframe: &[u8], current: &[u8], q: BitQuality, dst: &mut [u8], dst_start: usize) -> Option<usize> {
        let g = &GEO[q.index()];
        if keyframe.len() < g.payload_size || current.len() < g.payload_size {
            return None;
        }
        if dst.len() < dst_start || dst.len() - dst_start < g.max_delta_size {
            return None;
        }

        let layout = g.layout;
        let channels = &layout.channels;

        let mut mask = [0u8; Self::DIRTY_MASK_BYTES];

        // Which 8-byte words moved at all. A field none of whose words moved cannot have changed,
        // so it skips the per-channel unpack entirely.
        let dirty_words = if layout.word_mask_usable {
            BasisPayloadDiff::word_diff_mask(current, keyframe, g.payload_size)
        } else {
            u64::MAX
        };
        let field_words = &layout.field_word_mask;

        for f in 0..Self::FIELD_COUNT {
            if (dirty_words & field_words[f]) == 0 {
                continue;
            }
            let (start, end) = (layout.field_channel_start(f), layout.field_channel_end(f));
            for c in start..end {
                if Self::read_channel(current, &channels[c]) != Self::read_channel(keyframe, &channels[c]) {
                    Self::set_bit(&mut mask, f);
                    break;
                }
            }
        }

        let mask_dst = dst.get_mut(dst_start..dst_start + Self::DIRTY_MASK_BYTES)?;
        mask_dst.copy_from_slice(&mask);

        let mut w = BitWriter::new(dst, (dst_start + Self::DIRTY_MASK_BYTES) * 8);
        let body_start_bit = w.bit_position();

        for f in 0..Self::FIELD_COUNT {
            if !Self::get_bit(&mask, f) {
                continue;
            }
            let (start, end) = (layout.field_channel_start(f), layout.field_channel_end(f));

            let mut residual_bits = 0u32;
            let mut raw_bits = 0u32;
            for c in start..end {
                let ch = &channels[c];
                raw_bits += u32::from(ch.width);
                if ch.kind == BasisChannelKind::Raw {
                    residual_bits += u32::from(ch.width);
                    continue;
                }
                let diff = BasisResidualCodec::wrap_signed(
                    (Self::read_channel(current, ch) as i32).wrapping_sub(Self::read_channel(keyframe, ch) as i32),
                    u32::from(ch.width),
                );
                residual_bits += BasisResidualCodec::signed_eg_bits(diff);
            }

            let raw = raw_bits < residual_bits;
            w.write_bit(if raw { Self::MODE_RAW } else { Self::MODE_RESIDUAL });

            for c in start..end {
                let ch = &channels[c];
                let cur = Self::read_channel(current, ch);
                if raw || ch.kind == BasisChannelKind::Raw {
                    w.write_bits(u64::from(cur), u32::from(ch.width));
                    continue;
                }
                let diff = BasisResidualCodec::wrap_signed((cur as i32).wrapping_sub(Self::read_channel(keyframe, ch) as i32), u32::from(ch.width));
                w.write_signed_eg(diff);
            }
        }

        // Zero the unused bits of the final partial byte: the body is compared and hashed
        // elsewhere, and dst is a reused scratch buffer that is not guaranteed clean.
        let body_bits = w.bit_position() - body_start_bit;
        let pad = (8 - (body_bits & 7)) & 7;
        if pad > 0 {
            w.write_bits(0, pad as u32);
        }

        Some(Self::DIRTY_MASK_BYTES + ((body_bits + 7) >> 3))
    }

    /// Reconstructs the full payload from `baseline` (last keyframe) plus the delta body in
    /// `delta[delta_start .. delta_start + delta_len)`. Writes `payload_size(q)` bytes into
    /// `out_full`. Returns false if the delta is malformed/truncated. Never mutates the baseline.
    pub fn try_apply_delta(baseline: &[u8], delta: &[u8], delta_start: usize, delta_len: usize, q: BitQuality, out_full: &mut [u8]) -> bool {
        let g = &GEO[q.index()];
        if baseline.len() < g.payload_size || out_full.len() < g.payload_size {
            return false;
        }
        if delta_len < Self::DIRTY_MASK_BYTES || delta_start + delta_len > delta.len() {
            return false;
        }

        let layout = g.layout;
        let channels = &layout.channels;
        let mask = &delta[delta_start..delta_start + Self::DIRTY_MASK_BYTES];

        out_full[..g.payload_size].copy_from_slice(&baseline[..g.payload_size]);

        let mut r = BitReader::new(delta, (delta_start + Self::DIRTY_MASK_BYTES) * 8, (delta_start + delta_len) * 8);

        for f in 0..Self::FIELD_COUNT {
            if !Self::get_bit(mask, f) {
                continue;
            }
            let raw = r.read_bit() == Self::MODE_RAW;
            let (start, end) = (layout.field_channel_start(f), layout.field_channel_end(f));
            for c in start..end {
                let ch = &channels[c];
                if raw || ch.kind == BasisChannelKind::Raw {
                    let v = r.read_bits(u32::from(ch.width)) as u32;
                    if r.failed() {
                        return false;
                    }
                    Self::write_channel(out_full, ch, v);
                    continue;
                }
                let diff = r.read_signed_eg();
                if r.failed() {
                    return false;
                }
                let base = Self::read_channel(baseline, ch) as i32;
                Self::write_channel(out_full, ch, (base.wrapping_add(diff) as u32) & ch.mask());
            }
        }
        if r.failed() {
            return false;
        }

        // The body must occupy exactly the bytes it was given.
        let consumed = r.bit_position() - (delta_start + Self::DIRTY_MASK_BYTES) * 8;
        Self::DIRTY_MASK_BYTES + ((consumed + 7) >> 3) == delta_len
    }

    /// Reads the dirty mask and codes at the start of a delta body and returns the total body
    /// length in bytes (mask + encoded fields), or `None` if the body is truncated or malformed.
    pub fn delta_body_length(delta: &[u8], start: usize, available: usize, q: BitQuality) -> Option<usize> {
        if available < Self::DIRTY_MASK_BYTES || start + Self::DIRTY_MASK_BYTES > delta.len() {
            return None;
        }
        let limit = available.min(delta.len() - start);
        let g = &GEO[q.index()];
        let layout = g.layout;
        let channels = &layout.channels;
        let mask = &delta[start..start + Self::DIRTY_MASK_BYTES];

        let mut r = BitReader::new(delta, (start + Self::DIRTY_MASK_BYTES) * 8, (start + limit) * 8);

        for f in 0..Self::FIELD_COUNT {
            if !Self::get_bit(mask, f) {
                continue;
            }
            let raw = r.read_bit() == Self::MODE_RAW;
            let (cs, ce) = (layout.field_channel_start(f), layout.field_channel_end(f));
            for c in cs..ce {
                let ch = &channels[c];
                if raw || ch.kind == BasisChannelKind::Raw {
                    r.read_bits(u32::from(ch.width));
                } else {
                    r.read_signed_eg();
                }
                if r.failed() {
                    return None;
                }
            }
        }
        if r.failed() {
            return None;
        }

        let consumed = r.bit_position() - (start + Self::DIRTY_MASK_BYTES) * 8;
        Some(Self::DIRTY_MASK_BYTES + ((consumed + 7) >> 3))
    }

    #[inline]
    pub fn read_channel(payload: &[u8], ch: &BasisAvatarChannel) -> u32 {
        BasisBitCodec::read(payload, ch.bit_offset, u32::from(ch.width)) as u32
    }

    #[inline]
    pub fn write_channel(payload: &mut [u8], ch: &BasisAvatarChannel, value: u32) {
        BasisBitCodec::replace(payload, ch.bit_offset, u64::from(value), u32::from(ch.width));
    }

    #[inline]
    fn set_bit(mask: &mut [u8], field: usize) {
        mask[field >> 3] |= 1 << (field & 7);
    }

    #[inline]
    fn get_bit(mask: &[u8], field: usize) -> bool {
        (mask[field >> 3] & (1 << (field & 7))) != 0
    }
}
