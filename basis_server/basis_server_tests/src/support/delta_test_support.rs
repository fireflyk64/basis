//! Port of `DeltaTestSupport.cs`: payload construction (random and realistic), field geometry
//! accessors, and build/apply round-trip assertions for the avatar delta codec tests.
//!
//! "Bone" here means a ROTATION WIRE FIELD, of which there are
//! `BasisBoneRotationCompression::ROTATION_FIELD_COUNT` = 31: the 21 explicit bone slots followed
//! by the 10 finger curl/splay channels.

use basis_network_core::compression::{BasisAvatarBitPacking, BasisAvatarChannelLayout, BasisAvatarChannelMap, BasisAvatarDeltaCompression, BasisBoneRotationCompression, BitQuality};
use rand::{Rng, RngExt, SeedableRng};

/// A seeded generator with the `System.Random`-shaped helpers the C# tests used.
pub struct TestRng(pub rand::rngs::StdRng);

impl TestRng {
    pub fn new(seed: u64) -> Self {
        Self(rand::rngs::StdRng::seed_from_u64(seed))
    }

    /// `rng.Next(max)`: 0..max.
    pub fn next(&mut self, max: usize) -> usize {
        if max == 0 { 0 } else { self.0.random_range(0..max) }
    }

    /// `rng.Next(min, max)`: min..max.
    pub fn next_range(&mut self, min: i32, max: i32) -> i32 {
        if max <= min { min } else { self.0.random_range(min..max) }
    }

    pub fn next_f64(&mut self) -> f64 {
        self.0.random::<f64>()
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0.random::<u64>()
    }

    pub fn next_bytes(&mut self, dst: &mut [u8]) {
        self.0.fill_bytes(dst);
    }

    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        let mut b = vec![0u8; n];
        self.next_bytes(&mut b);
        b
    }
}

pub struct DeltaTestSupport;

impl DeltaTestSupport {
    pub const ALL_QUALITIES: [BitQuality; 4] = [BitQuality::VeryLow, BitQuality::Low, BitQuality::Medium, BitQuality::High];

    /// Number of addressable rotation wire fields (21 bone slots + 10 finger channels).
    pub const BONE_COUNT: usize = BasisBoneRotationCompression::ROTATION_FIELD_COUNT; // 31
    /// Of those, how many are true bone slots carrying a smallest-three rotation.
    pub const WIRE_BONE_SLOTS: usize = BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT; // 21

    pub fn pos_bytes(q: BitQuality) -> usize {
        BasisAvatarBitPacking::position_bytes(q)
    }

    pub fn bone_base_bit(q: BitQuality) -> usize {
        Self::pos_bytes(q) * 8
    }

    pub fn payload_size(q: BitQuality) -> usize {
        BasisAvatarBitPacking::convert_to_size(q)
    }

    pub fn rot_bytes(q: BitQuality) -> usize {
        BasisBoneRotationCompression::rotation_bytes(q)
    }

    pub fn tail_start(q: BitQuality) -> usize {
        Self::pos_bytes(q) + Self::rot_bytes(q)
    }

    pub fn scale_offset(q: BitQuality) -> usize {
        Self::tail_start(q)
    }

    pub fn body_rot_offset(q: BitQuality) -> usize {
        Self::tail_start(q) + BasisBoneRotationCompression::WRITE_SCALE
    }

    pub fn hips_delta_offset(q: BitQuality) -> usize {
        Self::body_rot_offset(q) + BasisBoneRotationCompression::WRITE_ROTATION
    }

    pub fn hips_rot_offset(q: BitQuality) -> usize {
        Self::hips_delta_offset(q) + BasisBoneRotationCompression::WRITE_HIPS_DELTA
    }

    pub fn end_effector_offset(q: BitQuality) -> usize {
        Self::tail_start(q) + BasisBoneRotationCompression::TAIL_BYTES
    }

    pub fn end_effector_bytes(q: BitQuality) -> usize {
        BasisBoneRotationCompression::end_effector_bytes(q)
    }

    pub fn layout(q: BitQuality) -> &'static BasisAvatarChannelLayout {
        BasisAvatarChannelMap::for_quality(q)
    }

    /// Flip every byte of the end-effector block (High only), guaranteeing it differs.
    pub fn flip_end_effector(payload: &mut [u8], q: BitQuality) {
        let off = Self::end_effector_offset(q);
        for i in 0..Self::end_effector_bytes(q) {
            payload[off + i] ^= 0xFF;
        }
    }

    /// Raw per-slot bits-per-component table (51 entries; only the first 21 reach the wire).
    pub fn bpc(q: BitQuality) -> &'static [u8; 51] {
        BasisBoneRotationCompression::get_bpc_table(q)
    }

    /// Bit width of rotation wire field `field` (0..30).
    pub fn bone_width(q: BitQuality, field: usize) -> u32 {
        Self::rotation_field_widths(q)[field]
    }

    pub fn rotation_field_widths(q: BitQuality) -> Vec<u32> {
        BasisBoneRotationCompression::build_rotation_field_widths(q)
    }

    /// Start bit of each rotation wire field, relative to the rotation region.
    pub fn bone_bit_offsets(q: BitQuality) -> Vec<usize> {
        let mut offs = vec![0usize; BasisBoneRotationCompression::ROTATION_FIELD_COUNT];
        let _ = BasisBoneRotationCompression::build_rotation_field_offsets(q, &mut offs);
        offs
    }

    pub fn get_bone(payload: &[u8], q: BitQuality, field: usize) -> u64 {
        let mut pos = Self::bone_base_bit(q) + Self::bone_bit_offsets(q)[field];
        BasisBoneRotationCompression::read_bits(payload, &mut pos, Self::bone_width(q, field))
    }

    pub fn set_bone(payload: &mut [u8], q: BitQuality, field: usize, value: u64) {
        let offset = Self::bone_base_bit(q) + Self::bone_bit_offsets(q)[field];
        let width = Self::bone_width(q, field);
        for i in 0..width as usize {
            let b = offset + i;
            let (byte_pos, bit) = (b >> 3, b & 7);
            if (value >> i) & 1 != 0 {
                payload[byte_pos] |= 1 << bit;
            } else {
                payload[byte_pos] &= !(1 << bit);
            }
        }
    }

    /// Flip every bit of a rotation field, guaranteeing it differs from its current value.
    pub fn flip_bone(payload: &mut [u8], q: BitQuality, field: usize) {
        let maxv = (1u64 << Self::bone_width(q, field)) - 1;
        let current = Self::get_bone(payload, q, field);
        Self::set_bone(payload, q, field, current ^ maxv);
    }

    /// Valid quantized payload: random position/tail bytes + random rotation-field bits.
    pub fn make_payload(q: BitQuality, rng: &mut TestRng) -> Vec<u8> {
        let mut arr = vec![0u8; Self::payload_size(q)];
        Self::fill_non_rotation(&mut arr, q, rng);
        let widths = Self::rotation_field_widths(q);
        let offs = Self::bone_bit_offsets(q);
        for (f, &width) in widths.iter().enumerate() {
            let maxv = if width >= 64 { u64::MAX } else { (1u64 << width) - 1 };
            BasisBoneRotationCompression::write_bits(&mut arr, Self::bone_base_bit(q) + offs[f], rng.next_u64() & maxv, width);
        }
        arr
    }

    /// Realistic payload: the 21 bone slots are true smallest-three encodings of random unit
    /// quaternions, the 10 finger channels are quantized curl/splay pairs.
    pub fn make_realistic_payload(q: BitQuality, rng: &mut TestRng) -> Vec<u8> {
        let mut arr = vec![0u8; Self::payload_size(q)];
        Self::fill_non_rotation(&mut arr, q, rng);
        let bpc = Self::bpc(q);
        let offs = Self::bone_bit_offsets(q);

        for slot in 0..Self::WIRE_BONE_SLOTS {
            let (x, y, z, w) = Self::random_quat(rng);
            let packed = BasisBoneRotationCompression::encode_smallest_three(x, y, z, w, bpc[slot] as u32, BasisBoneRotationCompression::MAX_COMPONENT[slot]);
            BasisBoneRotationCompression::write_bits(&mut arr, Self::bone_base_bit(q) + offs[slot], packed, 2 + 3 * bpc[slot] as u32);
        }

        let curl_bits = BasisBoneRotationCompression::curl_bits(q);
        let splay_bits = BasisBoneRotationCompression::splay_bits(q);
        for f in 0..BasisBoneRotationCompression::FINGER_CHANNEL_COUNT {
            let curl = BasisBoneRotationCompression::encode_signed_unit((rng.next_f64() * 2.0 - 1.0) as f32, curl_bits);
            let splay = BasisBoneRotationCompression::encode_signed_unit((rng.next_f64() * 2.0 - 1.0) as f32, splay_bits);
            let b = Self::bone_base_bit(q) + offs[Self::WIRE_BONE_SLOTS + f];
            BasisBoneRotationCompression::write_bits(&mut arr, b, curl as u64, curl_bits);
            BasisBoneRotationCompression::write_bits(&mut arr, b + curl_bits as usize, splay as u64, splay_bits);
        }
        arr
    }

    // Position and the tail are opaque byte regions to these tests; random bytes exercise the
    // codec just as well as real ones.
    fn fill_non_rotation(arr: &mut [u8], q: BitQuality, rng: &mut TestRng) {
        let pos = Self::pos_bytes(q);
        rng.next_bytes(&mut arr[..pos]);
        let tail = Self::tail_start(q);
        rng.next_bytes(&mut arr[tail..tail + BasisBoneRotationCompression::TAIL_BYTES]);
        let ee = Self::end_effector_bytes(q);
        if ee > 0 {
            let off = Self::end_effector_offset(q);
            rng.next_bytes(&mut arr[off..off + ee]);
        }
    }

    pub fn random_quat(rng: &mut TestRng) -> (f32, f32, f32, f32) {
        let x = (rng.next_f64() * 2.0 - 1.0) as f32;
        let y = (rng.next_f64() * 2.0 - 1.0) as f32;
        let z = (rng.next_f64() * 2.0 - 1.0) as f32;
        let w = (rng.next_f64() * 2.0 - 1.0) as f32;
        let len = (x * x + y * y + z * z + w * w).sqrt();
        if len < 1e-6 { (0.0, 0.0, 0.0, 1.0) } else { (x / len, y / len, z / len, w / len) }
    }

    /// Builds a delta, verifies the length probe agrees and the bound holds, then applies it.
    pub fn build_apply(kf: &[u8], cur: &[u8], q: BitQuality) -> (usize, Vec<u8>) {
        let mut dst = vec![0u8; BasisAvatarDeltaCompression::max_delta_size(q)];
        let len = BasisAvatarDeltaCompression::build_delta(kf, cur, q, &mut dst, 0).expect("build_delta returned None");
        assert!(len > 0, "build_delta returned an empty body");
        assert!(len <= BasisAvatarDeltaCompression::max_delta_size(q), "delta exceeded max_delta_size");
        assert_eq!(Some(len), BasisAvatarDeltaCompression::delta_body_length(&dst, 0, len, q));
        let mut recon = vec![0u8; Self::payload_size(q)];
        assert!(BasisAvatarDeltaCompression::try_apply_delta(kf, &dst, 0, len, q, &mut recon), "try_apply_delta rejected a valid delta");
        (len, recon)
    }

    pub fn assert_round_trip(kf: &[u8], cur: &[u8], q: BitQuality) {
        let (_, recon) = Self::build_apply(kf, cur, q);
        let n = Self::payload_size(q);
        assert_eq!(&cur[..n], &recon[..n]);
    }
}
