//! Port of `Reduction/AvatarQualityRepacker.cs`: converts HIGH quality bone rotation data into
//! medium/low/very-low quality by re-quantizing each bone's smallest-three components at a
//! lower bits-per-component.

use std::sync::LazyLock;

use basis_error::{BasisError, BasisResult, ErrorCode};
use basis_network_core::SerializableBasis::LocalAvatarSyncMessage;
use basis_network_core::compression::{BasisAvatarBitPacking, BasisBitCodec, BasisBoneRotationCompression, BitQuality};

use crate::reduction::QuantRescaleTable;

struct Tables {
    high_offs: Vec<usize>,
    med_offs: Vec<usize>,
    low_offs: Vec<usize>,
    vlow_offs: Vec<usize>,
}

static TABLES: LazyLock<Tables> = LazyLock::new(|| Tables {
    high_offs: AvatarQualityRepacker::build_bit_offsets(BitQuality::High),
    med_offs: AvatarQualityRepacker::build_bit_offsets(BitQuality::Medium),
    low_offs: AvatarQualityRepacker::build_bit_offsets(BitQuality::Low),
    vlow_offs: AvatarQualityRepacker::build_bit_offsets(BitQuality::VeryLow),
});

pub struct AvatarQualityRepacker;

impl AvatarQualityRepacker {
    const BONE_SLOTS: usize = BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT; // 21
    const FINGER_SLOTS: usize = BasisBoneRotationCompression::FINGER_CHANNEL_COUNT; // 10
    const POS_BYTES: usize = BasisAvatarBitPacking::WRITE_POSITION;

    fn build_bit_offsets(q: BitQuality) -> Vec<usize> {
        let mut offs = vec![0usize; BasisBoneRotationCompression::ROTATION_FIELD_COUNT];
        BasisBoneRotationCompression::build_rotation_field_offsets(q, &mut offs);
        offs
    }

    fn rot_bytes(q: BitQuality) -> usize {
        BasisAvatarBitPacking::muscle_bytes(q)
    }

    fn payload_size(q: BitQuality) -> usize {
        Self::POS_BYTES + Self::rot_bytes(q) + BasisAvatarBitPacking::TAIL_BYTES
    }

    fn offsets(q: BitQuality) -> &'static [usize] {
        match q {
            BitQuality::High => &TABLES.high_offs,
            BitQuality::Medium => &TABLES.med_offs,
            BitQuality::Low => &TABLES.low_offs,
            BitQuality::VeryLow => &TABLES.vlow_offs,
        }
    }

    /// Builds the three lower tiers from a High payload. An absent or undersized High payload is
    /// an error, as the C# threw.
    pub fn build_all_lower_from_high_into(
        src_high: &LocalAvatarSyncMessage,
        medium: &mut LocalAvatarSyncMessage,
        low: &mut LocalAvatarSyncMessage,
        very_low: &mut LocalAvatarSyncMessage,
    ) -> BasisResult<()> {
        let Some(src) = src_high.array.as_deref() else {
            return Err(BasisError::permanent(ErrorCode::InvalidArgument, "High payload is missing"));
        };
        let high_size = Self::payload_size(BitQuality::High);
        if src.len() < high_size {
            return Err(BasisError::permanent(ErrorCode::InvalidArgument, format!("High payload too small. Need >= {high_size}, got {}", src.len())));
        }
        Self::ensure_buffer(medium, BitQuality::Medium);
        Self::ensure_buffer(low, BitQuality::Low);
        Self::ensure_buffer(very_low, BitQuality::VeryLow);
        let (Some(med_arr), Some(low_arr), Some(vlow_arr)) = (medium.array.as_deref_mut(), low.array.as_deref_mut(), very_low.array.as_deref_mut()) else {
            return Err(BasisError::permanent(ErrorCode::Internal, "lower tier buffers were not allocated"));
        };
        let mut targets: [(&mut [u8], BitQuality); 3] = [(med_arr, BitQuality::Medium), (low_arr, BitQuality::Low), (vlow_arr, BitQuality::VeryLow)];

        let rot_base = Self::POS_BYTES;
        let src_rot_base = Self::POS_BYTES;
        for (dst, q) in targets.iter_mut() {
            // Position: int24-mm at every tier, so it copies across untouched.
            dst[..Self::POS_BYTES].copy_from_slice(&src[..Self::POS_BYTES]);
            // Clear rotation regions (the bit writer ORs into bytes).
            let rot = Self::rot_bytes(*q);
            dst[rot_base..rot_base + rot].fill(0);
        }

        let high_offs = &TABLES.high_offs;
        let high_bpc = &BasisBoneRotationCompression::BPC_HIGH;
        // Repack each explicit bone. 3-DOF slots: read smallest-three at HIGH BPC, rescale to
        // the lower BPC. Restricted slots: one or two uniformly quantized angles whose ranges are
        // quality-invariant, so they rescale on the same integer ladder as fingers.
        for slot in 0..Self::BONE_SLOTS {
            if BasisBoneRotationCompression::BONE_DOF[slot] == 3 {
                let bpc_src = u32::from(high_bpc[slot]);
                let total_bits_src = 2 + 3 * bpc_src;
                let raw = Self::read_bits(src, src_rot_base, high_offs[slot], total_bits_src);
                let idx = (raw & 3) as u32;
                let mask_src = (1u64 << bpc_src) - 1;
                let qa = ((raw >> 2) & mask_src) as u32;
                let qb = ((raw >> (2 + bpc_src)) & mask_src) as u32;
                let qc = ((raw >> (2 + 2 * bpc_src)) & mask_src) as u32;
                for (dst, q) in targets.iter_mut() {
                    let bpc_dst = u32::from(BasisBoneRotationCompression::get_bpc_table(*q)[slot]);
                    Self::repack_bone(dst, rot_base, Self::offsets(*q)[slot], bpc_dst, idx, qa, qb, qc, bpc_src);
                }
            } else {
                for (dst, q) in targets.iter_mut() {
                    Self::repack_restricted_bone(src, src_rot_base, slot, dst, rot_base, Self::offsets(*q)[slot], *q);
                }
            }
        }
        // Finger channels: two independent signed-unit scalars per finger, rescaled on the same
        // integer ladder the quaternion components use.
        let src_curl = BasisBoneRotationCompression::curl_bits(BitQuality::High);
        let src_splay = BasisBoneRotationCompression::splay_bits(BitQuality::High);
        for finger in 0..Self::FINGER_SLOTS {
            let field = Self::BONE_SLOTS + finger;
            let src_bit = high_offs[field];
            let curl = Self::read_bits(src, src_rot_base, src_bit, src_curl) as u32;
            let splay = Self::read_bits(src, src_rot_base, src_bit + src_curl as usize, src_splay) as u32;
            for (dst, q) in targets.iter_mut() {
                Self::repack_finger(dst, rot_base, Self::offsets(*q)[field], *q, curl, splay, src_curl, src_splay);
            }
        }
        // Copy tail (scale + body rotation).
        let src_tail_offset = Self::POS_BYTES + Self::rot_bytes(BitQuality::High);
        let tail = BasisAvatarBitPacking::TAIL_BYTES;
        for (dst, q) in targets.iter_mut() {
            let dst_tail = rot_base + Self::rot_bytes(*q);
            dst[dst_tail..dst_tail + tail].copy_from_slice(&src[src_tail_offset..src_tail_offset + tail]);
        }
        Ok(())
    }

    /// Rescales a restricted (1/2-DOF) bone's angle field(s) from High to a lower tier.
    fn repack_restricted_bone(src: &[u8], src_rot_base: usize, slot: usize, dst: &mut [u8], dst_rot_base: usize, dst_bit_offset: usize, dst_quality: BitQuality) {
        let src_bit = TABLES.high_offs[slot];
        if BasisBoneRotationCompression::BONE_DOF[slot] == 1 {
            let src_bits = BasisBoneRotationCompression::single_axis_bits(BitQuality::High);
            let dst_bits = BasisBoneRotationCompression::single_axis_bits(dst_quality);
            let v = Self::read_bits(src, src_rot_base, src_bit, src_bits) as u32;
            Self::write_bits(dst, dst_rot_base, dst_bit_offset, u64::from(Self::rescale_quant(v, src_bits, dst_bits)), dst_bits);
            return;
        }
        let src_hinge = BasisBoneRotationCompression::hinge_bits(BitQuality::High);
        let src_twist = BasisBoneRotationCompression::twist_bits(BitQuality::High);
        let dst_hinge = BasisBoneRotationCompression::hinge_bits(dst_quality);
        let dst_twist = BasisBoneRotationCompression::twist_bits(dst_quality);
        let hinge = Self::read_bits(src, src_rot_base, src_bit, src_hinge) as u32;
        let twist = Self::read_bits(src, src_rot_base, src_bit + src_hinge as usize, src_twist) as u32;
        let packed = u64::from(Self::rescale_quant(hinge, src_hinge, dst_hinge)) | (u64::from(Self::rescale_quant(twist, src_twist, dst_twist)) << dst_hinge);
        Self::write_bits(dst, dst_rot_base, dst_bit_offset, packed, dst_hinge + dst_twist);
    }

    #[allow(clippy::too_many_arguments)]
    fn repack_finger(dst: &mut [u8], base_byte_offset: usize, bit_offset: usize, dst_quality: BitQuality, curl: u32, splay: u32, src_curl_bits: u32, src_splay_bits: u32) {
        let dst_curl_bits = BasisBoneRotationCompression::curl_bits(dst_quality);
        let dst_splay_bits = BasisBoneRotationCompression::splay_bits(dst_quality);
        let dst_curl = Self::rescale_quant(curl, src_curl_bits, dst_curl_bits);
        let dst_splay = Self::rescale_quant(splay, src_splay_bits, dst_splay_bits);
        let packed = u64::from(dst_curl) | (u64::from(dst_splay) << dst_curl_bits);
        Self::write_bits(dst, base_byte_offset, bit_offset, packed, dst_curl_bits + dst_splay_bits);
    }

    #[allow(clippy::too_many_arguments)]
    fn repack_bone(dst: &mut [u8], base_byte_offset: usize, bit_offset: usize, bpc_dst: u32, idx: u32, qa: u32, qb: u32, qc: u32, bpc_src: u32) {
        let da = Self::rescale_quant(qa, bpc_src, bpc_dst);
        let db = Self::rescale_quant(qb, bpc_src, bpc_dst);
        let dc = Self::rescale_quant(qc, bpc_src, bpc_dst);
        // Pack: [idx:2][da:bpcDst][db:bpcDst][dc:bpcDst]
        let packed = u64::from(idx) | (u64::from(da) << 2) | (u64::from(db) << (2 + bpc_dst)) | (u64::from(dc) << (2 + 2 * bpc_dst));
        Self::write_bits(dst, base_byte_offset, bit_offset, packed, 2 + 3 * bpc_dst);
    }

    fn ensure_buffer(msg: &mut LocalAvatarSyncMessage, q: BitQuality) {
        msg.data_quality_level = q as u8;
        let size = Self::payload_size(q);
        match msg.array.as_mut() {
            Some(array) if array.len() >= size => {}
            _ => msg.array = Some(vec![0u8; size]),
        }
    }

    /// The three lower tiers as fresh messages.
    pub fn build_all_lower_from_high(src_high: &LocalAvatarSyncMessage) -> BasisResult<(LocalAvatarSyncMessage, LocalAvatarSyncMessage, LocalAvatarSyncMessage)> {
        let mut med = LocalAvatarSyncMessage::default();
        let mut low = LocalAvatarSyncMessage::default();
        let mut vlow = LocalAvatarSyncMessage::default();
        Self::build_all_lower_from_high_into(src_high, &mut med, &mut low, &mut vlow)?;
        Ok((med, low, vlow))
    }

    /// Rescales one quantized value from `b_src` bits to `b_dst` bits, rounding to nearest.
    #[inline]
    fn rescale_quant(q_src: u32, b_src: u32, b_dst: u32) -> u32 {
        if b_src == b_dst {
            return q_src;
        }
        if b_dst == 0 {
            return 0;
        }
        QuantRescaleTable::rescale(q_src, b_src as usize, b_dst as usize)
    }

    #[inline]
    fn read_bits(src: &[u8], base_byte_offset: usize, bit_pos: usize, bit_count: u32) -> u64 {
        BasisBitCodec::read(src, (base_byte_offset << 3) + bit_pos, bit_count)
    }

    /// The destination rotation region is cleared before any of this runs, so OR is correct.
    #[inline]
    fn write_bits(dst: &mut [u8], base_byte_offset: usize, bit_pos: usize, value: u64, bit_count: u32) {
        BasisBitCodec::or(dst, (base_byte_offset << 3) + bit_pos, value, bit_count);
    }
}
