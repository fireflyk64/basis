use std::sync::LazyLock;

use super::basis_avatar_bit_packing::{BasisAvatarBitPacking, BitQuality};
use super::basis_avatar_delta_compression::BasisAvatarDeltaCompression;
use super::basis_bone_rotation_compression::BasisBoneRotationCompression;
use super::basis_payload_diff::BasisPayloadDiff;

/// What a channel's bits mean, and therefore what may be done to them arithmetically.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum BasisChannelKind {
    /// A uniformly quantized scalar: differences are meaningful (residual coding).
    Delta = 0,
    /// Categorical or opaque bits — only ever carried verbatim.
    Raw = 1,
}

/// One channel: a contiguous run of bits in the payload with a single interpretation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BasisAvatarChannel {
    pub bit_offset: usize,
    pub width: u8,
    pub kind: BasisChannelKind,
}

impl BasisAvatarChannel {
    pub const fn new(bit_offset: usize, width: u32, kind: BasisChannelKind) -> Self {
        Self { bit_offset, width: width as u8, kind }
    }

    /// Low-bit mask for this channel's width.
    #[inline]
    pub fn mask(&self) -> u32 {
        if self.width >= 32 { u32::MAX } else { (1u32 << self.width) - 1 }
    }
}

/// The channel decomposition of one quality's avatar payload, grouped into the same dirty-mask
/// fields [`BasisAvatarDeltaCompression`] has always used. The channel list is a TOTAL PARTITION
/// of the payload: every bit belongs to exactly one channel, structural padding included.
#[derive(Debug)]
pub struct BasisAvatarChannelLayout {
    /// All channels, ordered by field then by bit offset (which is also payload order).
    pub channels: Vec<BasisAvatarChannel>,
    /// Prefix bounds: field f owns channels[field_first_channel[f] .. field_first_channel[f+1]).
    pub field_first_channel: Vec<usize>,
    pub payload_bytes: usize,
    pub payload_bits: usize,
    /// Total bits if every channel were written verbatim — the raw-mode worst case.
    pub total_channel_bits: usize,
    /// Widest channel in the layout.
    pub max_channel_width: u32,
    /// Which 8-byte words of the payload each field has bits in, one bit per word.
    pub field_word_mask: Vec<u64>,
    /// False when the payload has more 8-byte words than a `u64` mask can address.
    pub word_mask_usable: bool,
}

impl BasisAvatarChannelLayout {
    pub(crate) fn new(channels: Vec<BasisAvatarChannel>, field_first_channel: Vec<usize>, payload_bytes: usize) -> Self {
        let mut total = 0usize;
        let mut widest = 0u32;
        for c in &channels {
            total += c.width as usize;
            widest = widest.max(u32::from(c.width));
        }
        let word_mask_usable = payload_bytes <= BasisPayloadDiff::MAX_PAYLOAD_BYTES;
        let fields = field_first_channel.len() - 1;
        let mut field_word_mask = vec![0u64; fields];
        for f in 0..fields {
            if !word_mask_usable {
                field_word_mask[f] = u64::MAX;
                continue;
            }
            let mut words = 0u64;
            for c in field_first_channel[f]..field_first_channel[f + 1] {
                // A channel may straddle a word boundary, so mark the whole span it covers.
                let first_word = channels[c].bit_offset >> 6;
                let last_word = (channels[c].bit_offset + channels[c].width as usize - 1) >> 6;
                for w in first_word..=last_word {
                    words |= 1u64 << w;
                }
            }
            field_word_mask[f] = words;
        }
        Self {
            channels,
            field_first_channel,
            payload_bytes,
            payload_bits: payload_bytes * 8,
            total_channel_bits: total,
            max_channel_width: widest,
            field_word_mask,
            word_mask_usable,
        }
    }

    pub fn field_count(&self) -> usize {
        self.field_first_channel.len() - 1
    }

    #[inline]
    pub fn field_channel_start(&self, field: usize) -> usize {
        self.field_first_channel[field]
    }

    #[inline]
    pub fn field_channel_end(&self, field: usize) -> usize {
        self.field_first_channel[field + 1]
    }

    /// Sum of the widths of one field's channels — its verbatim cost.
    pub fn field_raw_bits(&self, field: usize) -> usize {
        (self.field_first_channel[field]..self.field_first_channel[field + 1])
            .map(|c| self.channels[c].width as usize)
            .sum()
    }
}

/// Builds the per-quality channel decomposition of an avatar payload.
///
/// Payload geometry:
///   [position      9 B]  3 x signed int24 millimetres, byte-aligned little-endian
///   [rotation    var B]  bit-packed LSB-first: 21 bone slots then 10 finger channels, then padding
///   [scale         2 B]  posit16
///   [body rot      7 B]  1 B largest index + 3 x uint16
///   [hips delta    5 B]  3 x signed 13-bit + 1 spare bit
///   [hips rot      7 B]  1 B largest index + 3 x uint16
///   [effectors    35 B]  High only: 8-bit mask + 4 x (3 x 12-bit position, 2-bit index, 3 x 10-bit rotation)
pub struct BasisAvatarChannelMap;

impl BasisAvatarChannelMap {
    const EFFECTOR_COUNT: usize = 4;
    const EFFECTOR_POS_BITS: usize = 12;
    const EFFECTOR_ROT_BPC: usize = 10;
    const EFFECTOR_MASK_BITS: usize = 8;
    const EFFECTOR_STRIDE: usize = 3 * Self::EFFECTOR_POS_BITS + 2 + 3 * Self::EFFECTOR_ROT_BPC; // 68
    const EFFECTOR_BLOCK_BITS: usize = Self::EFFECTOR_MASK_BITS + Self::EFFECTOR_COUNT * Self::EFFECTOR_STRIDE; // 280

    const FIELD_SCALE: usize = 1 + BasisBoneRotationCompression::ROTATION_FIELD_COUNT; // 32
    const FIELD_BODY_ROT: usize = Self::FIELD_SCALE + 1; // 33
    const FIELD_HIPS_DELTA: usize = Self::FIELD_SCALE + 2; // 34
    const FIELD_HIPS_ROT: usize = Self::FIELD_SCALE + 3; // 35
    const FIELD_END_EFFECTOR: usize = Self::FIELD_SCALE + 4; // 36

    pub fn for_quality(q: BitQuality) -> &'static BasisAvatarChannelLayout {
        static LAYOUTS: LazyLock<[BasisAvatarChannelLayout; 4]> = LazyLock::new(|| {
            [
                BasisAvatarChannelMap::build(BitQuality::VeryLow),
                BasisAvatarChannelMap::build(BitQuality::Low),
                BasisAvatarChannelMap::build(BitQuality::Medium),
                BasisAvatarChannelMap::build(BitQuality::High),
            ]
        });
        &LAYOUTS[q.index()]
    }

    fn build(q: BitQuality) -> BasisAvatarChannelLayout {
        let pos_bytes = BasisAvatarBitPacking::position_bytes(q);
        let rot_bytes = BasisBoneRotationCompression::rotation_bytes(q);
        let rot_bits = BasisBoneRotationCompression::rotation_bits(q);
        let payload_bytes = BasisBoneRotationCompression::convert_to_size(q);
        let eff_bytes = BasisBoneRotationCompression::end_effector_bytes(q);

        assert!(
            eff_bytes == 0 || eff_bytes * 8 == Self::EFFECTOR_BLOCK_BITS,
            "Effector block geometry drifted: {eff_bytes} B vs {} bits modelled.",
            Self::EFFECTOR_BLOCK_BITS
        );

        let field_count = BasisAvatarDeltaCompression::FIELD_COUNT;
        let mut channels: Vec<BasisAvatarChannel> = Vec::with_capacity(256);
        let mut field_first = vec![0usize; field_count + 1];

        // ── Field 0: hips world position, 3 x signed int24 millimetres ──
        field_first[0] = 0;
        for a in 0..3 {
            channels.push(BasisAvatarChannel::new(a * 24, 24, BasisChannelKind::Delta));
        }

        // ── Fields 1..31: the rotation region ──
        let rot_base = pos_bytes * 8;
        let bpc = BasisBoneRotationCompression::get_bpc_table(q);
        let mut field_offsets = vec![0usize; BasisBoneRotationCompression::ROTATION_FIELD_COUNT];
        BasisBoneRotationCompression::build_rotation_field_offsets(q, &mut field_offsets);
        let curl_bits = BasisBoneRotationCompression::curl_bits(q);
        let splay_bits = BasisBoneRotationCompression::splay_bits(q);

        for slot in 0..BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT {
            field_first[BasisAvatarDeltaCompression::BONE_FIELD_START + slot] = channels.len();
            let b = rot_base + field_offsets[slot];
            match BasisBoneRotationCompression::BONE_DOF[slot] {
                3 => {
                    // Smallest-three: the 2-bit index selects which component was dropped, so it
                    // changes what the other three MEAN. Differencing across an index change is
                    // nonsense — Raw.
                    channels.push(BasisAvatarChannel::new(b, 2, BasisChannelKind::Raw));
                    for c in 0..3 {
                        channels.push(BasisAvatarChannel::new(b + 2 + c * bpc[slot] as usize, u32::from(bpc[slot]), BasisChannelKind::Delta));
                    }
                }
                2 => {
                    // Hinge + twist angles: uniformly quantized scalars, both deltable.
                    let hinge_bits = BasisBoneRotationCompression::hinge_bits(q);
                    channels.push(BasisAvatarChannel::new(b, hinge_bits, BasisChannelKind::Delta));
                    channels.push(BasisAvatarChannel::new(b + hinge_bits as usize, BasisBoneRotationCompression::twist_bits(q), BasisChannelKind::Delta));
                }
                _ => {
                    channels.push(BasisAvatarChannel::new(b, BasisBoneRotationCompression::single_axis_bits(q), BasisChannelKind::Delta));
                }
            }
        }

        for f in 0..BasisBoneRotationCompression::FINGER_CHANNEL_COUNT {
            let field = BasisAvatarDeltaCompression::BONE_FIELD_START + BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT + f;
            field_first[field] = channels.len();
            let b = rot_base + field_offsets[BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT + f];
            channels.push(BasisAvatarChannel::new(b, curl_bits, BasisChannelKind::Delta));
            channels.push(BasisAvatarChannel::new(b + curl_bits as usize, splay_bits, BasisChannelKind::Delta));
        }

        // Rotation-region tail padding (VeryLow rounds up by 3 bits). Parked on the last finger
        // field so the partition stays total.
        let rot_pad = rot_bytes * 8 - rot_bits;
        if rot_pad > 0 {
            channels.push(BasisAvatarChannel::new(rot_base + rot_bits, rot_pad as u32, BasisChannelKind::Raw));
        }

        // ── Fields 32..35: the tail ──
        let tail_start = pos_bytes + rot_bytes;
        let scale_off = tail_start;
        let body_rot_off = scale_off + BasisBoneRotationCompression::WRITE_SCALE;
        let hips_delta_off = body_rot_off + BasisBoneRotationCompression::WRITE_ROTATION;
        let hips_rot_off = hips_delta_off + BasisBoneRotationCompression::WRITE_HIPS_DELTA;

        // Scale: posit16. Verbatim is both simpler and smaller.
        field_first[Self::FIELD_SCALE] = channels.len();
        channels.push(BasisAvatarChannel::new(scale_off * 8, 16, BasisChannelKind::Raw));

        field_first[Self::FIELD_BODY_ROT] = channels.len();
        Self::add_byte_aligned_quaternion(&mut channels, body_rot_off * 8);

        field_first[Self::FIELD_HIPS_DELTA] = channels.len();
        for a in 0..3 {
            channels.push(BasisAvatarChannel::new(
                hips_delta_off * 8 + a * BasisAvatarBitPacking::HIPS_DELTA_BITS as usize,
                BasisAvatarBitPacking::HIPS_DELTA_BITS,
                BasisChannelKind::Delta,
            ));
        }
        // The 40th bit of the 5-byte hips-delta field is spare and asserted zero; carry it so the
        // partition stays total.
        let hips_used = 3 * BasisAvatarBitPacking::HIPS_DELTA_BITS as usize;
        if hips_used < BasisBoneRotationCompression::WRITE_HIPS_DELTA * 8 {
            channels.push(BasisAvatarChannel::new(
                hips_delta_off * 8 + hips_used,
                (BasisBoneRotationCompression::WRITE_HIPS_DELTA * 8 - hips_used) as u32,
                BasisChannelKind::Raw,
            ));
        }

        field_first[Self::FIELD_HIPS_ROT] = channels.len();
        Self::add_byte_aligned_quaternion(&mut channels, hips_rot_off * 8);

        // ── Field 36: end-effector block (High only) ──
        field_first[Self::FIELD_END_EFFECTOR] = channels.len();
        if eff_bytes > 0 {
            let e_base = (tail_start + BasisBoneRotationCompression::TAIL_BYTES) * 8;
            channels.push(BasisAvatarChannel::new(e_base, Self::EFFECTOR_MASK_BITS as u32, BasisChannelKind::Raw));
            for e in 0..Self::EFFECTOR_COUNT {
                let b = e_base + Self::EFFECTOR_MASK_BITS + e * Self::EFFECTOR_STRIDE;
                for a in 0..3 {
                    channels.push(BasisAvatarChannel::new(b + a * Self::EFFECTOR_POS_BITS, Self::EFFECTOR_POS_BITS as u32, BasisChannelKind::Delta));
                }
                let r = b + 3 * Self::EFFECTOR_POS_BITS;
                channels.push(BasisAvatarChannel::new(r, 2, BasisChannelKind::Raw));
                for c in 0..3 {
                    channels.push(BasisAvatarChannel::new(r + 2 + c * Self::EFFECTOR_ROT_BPC, Self::EFFECTOR_ROT_BPC as u32, BasisChannelKind::Delta));
                }
            }
        }

        field_first[field_count] = channels.len();
        BasisAvatarChannelLayout::new(channels, field_first, payload_bytes)
    }

    /// The 7-byte compressed quaternion: a whole byte of "largest component" index followed by
    /// three little-endian uint16 components. Byte-aligned, not bit-packed, unlike the bone block.
    fn add_byte_aligned_quaternion(channels: &mut Vec<BasisAvatarChannel>, bit_offset: usize) {
        channels.push(BasisAvatarChannel::new(bit_offset, 8, BasisChannelKind::Raw));
        for c in 0..3 {
            channels.push(BasisAvatarChannel::new(bit_offset + 8 + c * 16, 16, BasisChannelKind::Delta));
        }
    }
}
