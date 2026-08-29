use std::sync::LazyLock;

use super::basis_avatar_bit_packing::{BasisAvatarBitPacking, BitQuality, round_half_even};
use super::basis_bit_codec::BasisBitCodec;

/// Bone rotation compression using "smallest three" quaternion encoding. Pure arithmetic —
/// runs on the server, the headless client and the Unity client alike.
///
/// Each bone is assigned a bits-per-component (BPC) value based on its DOF; restricted
/// (1/2-DOF) joints ship quantized angles about fixed anatomical axes instead (v52), and the
/// thirty finger joints ship as ten curl/splay channels (v47).
pub struct BasisBoneRotationCompression;

impl BasisBoneRotationCompression {
    /// Number of bones synced. Excludes Hips (0), LeftEye (21), RightEye (22), Jaw (23).
    pub const SYNC_BONE_COUNT: usize = 51;

    /// Inverse of sqrt(2), the max magnitude of any non-dropped smallest-three component.
    pub const INV_SQRT2: f32 = std::f32::consts::FRAC_1_SQRT_2;

    pub const WRITE_POSITION: usize = BasisAvatarBitPacking::WRITE_POSITION; // 9
    pub const WRITE_SCALE: usize = BasisAvatarBitPacking::WRITE_SCALE; // 2
    pub const WRITE_ROTATION: usize = BasisAvatarBitPacking::WRITE_ROTATION; // 7
    pub const WRITE_HIPS_DELTA: usize = BasisAvatarBitPacking::WRITE_HIPS_DELTA; // 5
    pub const WRITE_HIPS_ROTATION: usize = BasisAvatarBitPacking::WRITE_HIPS_ROTATION; // 7
    pub const TAIL_BYTES: usize = BasisAvatarBitPacking::TAIL_BYTES; // 21

    /// Maps slot index (0..50) to HumanBodyBones enum value. Excludes Hips(0), LeftEye(21),
    /// RightEye(22), Jaw(23). Grouped: 3-DOF body → 2-DOF limbs → 2-DOF extremities → toes →
    /// finger proximal → finger mid/distal.
    pub const BONE_WRITE_ORDER: [i32; 51] = [
        // 3-DOF body (9 bones): Spine, Chest, UpperChest, Neck, Head, UpperArms, UpperLegs
        7, 8, 54, 9, 10, 13, 14, 1, 2,
        // 2-DOF limbs (4 bones): LowerArms, LowerLegs
        15, 16, 3, 4,
        // 2-DOF extremities (6 bones): Shoulders, Hands, Feet
        11, 12, 17, 18, 5, 6,
        // toes (2 bones) — eyes/jaw excluded (driven by face system)
        19, 20,
        // 2-DOF finger proximal (10 bones)
        24, 27, 30, 33, 36, 39, 42, 45, 48, 51,
        // 1-DOF finger intermediate (10 bones)
        25, 28, 31, 34, 37, 40, 43, 46, 49, 52,
        // 1-DOF finger distal (10 bones)
        26, 29, 32, 35, 38, 41, 44, 47, 50, 53,
    ];

    /// Reverse lookup: HumanBodyBones enum value → slot index. Index 0 (Hips) = -1.
    pub fn bone_to_slot() -> &'static [i32; 55] {
        static TABLE: LazyLock<[i32; 55]> = LazyLock::new(|| {
            let mut t = [-1i32; 55];
            for (slot, bone) in BasisBoneRotationCompression::BONE_WRITE_ORDER.iter().enumerate() {
                t[*bone as usize] = slot as i32;
            }
            t
        });
        &TABLE
    }

    /// HIGH quality. Bone slots 0..20 = 606 bits; + 140-bit finger block = 746 bits = 94 rotation bytes.
    pub const BPC_HIGH: [u8; 51] = [
        12, 12, 12, 12, 12, 12, 12, 12, 12,
        12, 12, 12, 12,
        12, 12, 12, 12, 12, 12,
        5, 5,
        6, 6, 6, 6, 5, 6, 6, 6, 6, 5,
        6, 6, 5, 5, 5, 6, 6, 5, 5, 5,
        5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
    ];

    /// MEDIUM quality. 414 bone bits + 120-bit finger block = 534 bits = 67 rotation bytes.
    pub const BPC_MEDIUM: [u8; 51] = [
        8, 8, 8, 8, 8, 8, 8, 8, 8,
        8, 8, 8, 8,
        8, 8, 8, 8, 6, 6,
        3, 3,
        6, 6, 5, 5, 4, 6, 6, 5, 5, 4,
        5, 5, 4, 4, 4, 5, 5, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    ];

    /// LOW quality. 318 bone bits + 100-bit finger block = 418 bits = 53 rotation bytes.
    pub const BPC_LOW: [u8; 51] = [
        6, 6, 6, 6, 6, 6, 6, 6, 6,
        6, 6, 6, 6,
        6, 6, 6, 6, 5, 5,
        3, 3,
        5, 5, 4, 4, 3, 5, 5, 4, 4, 3,
        4, 4, 3, 3, 3, 4, 4, 3, 3, 3,
        3, 3, 3, 3, 3, 3, 3, 3, 3, 3,
    ];

    /// VERY LOW quality. 271 bone bits + 80-bit finger block = 351 bits = 44 rotation bytes.
    pub const BPC_VERY_LOW: [u8; 51] = [
        5, 5, 5, 5, 5, 5, 5, 5, 5,
        5, 5, 5, 5,
        5, 5, 5, 5, 4, 4,
        2, 2,
        4, 4, 3, 3, 2, 4, 4, 3, 3, 2,
        3, 3, 2, 2, 2, 3, 3, 2, 2, 2,
        2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    ];

    /// Maximum quaternion component magnitude per bone slot. After dropping the largest
    /// component in smallest-three, the remaining 3 are quantized within [-maxComp, maxComp].
    pub const MAX_COMPONENT: [f32; 51] = [
        // 3-DOF body (9): Spine, Chest, UpperChest, Neck, Head, UpperArms, UpperLegs
        Self::INV_SQRT2, Self::INV_SQRT2, 0.50, Self::INV_SQRT2, Self::INV_SQRT2,
        Self::INV_SQRT2, Self::INV_SQRT2, Self::INV_SQRT2, Self::INV_SQRT2,
        // 2-DOF limbs (4): LowerArms, LowerLegs
        Self::INV_SQRT2, Self::INV_SQRT2, Self::INV_SQRT2, Self::INV_SQRT2,
        // 2-DOF extremities (6): Shoulders, Hands, Feet
        0.50, 0.50, Self::INV_SQRT2, Self::INV_SQRT2, 0.60, 0.60,
        // toes (2)
        0.50, 0.50,
        // finger proximal (10)
        0.68, 0.68, 0.68, 0.68, 0.68, 0.68, 0.68, 0.68, 0.68, 0.68,
        // finger intermediate (10)
        0.58, 0.58, 0.58, 0.58, 0.58, 0.58, 0.58, 0.58, 0.58, 0.58,
        // finger distal (10)
        0.65, 0.65, 0.65, 0.65, 0.65, 0.65, 0.65, 0.65, 0.65, 0.65,
    ];

    /// Degrees of freedom per wire bone slot (0..20). 3 = smallest-three quaternion;
    /// 2 = hinge+twist angle pair; 1 = single hinge angle.
    pub const BONE_DOF: [u8; 21] = [
        3, 3, 3, 3, 3, 3, 3, 3, 3,
        2, 2, 2, 2,
        2, 2, 2, 2, 2, 2,
        1, 1,
    ];

    pub const AXIS_X: u8 = 0;
    pub const AXIS_Y: u8 = 1;
    pub const AXIS_Z: u8 = 2;

    /// Primary (hinge) rotation axis per restricted slot, in the anatomical generic frame.
    pub const BONE_AXIS_A: [u8; 21] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0,
        Self::AXIS_Y, Self::AXIS_Y, // LowerArms: elbow flexion swings the forearm forward
        Self::AXIS_X, Self::AXIS_X, // LowerLegs: knee flexion
        Self::AXIS_Z, Self::AXIS_Z, // Shoulders: clavicle up-down (shrug)
        Self::AXIS_Z, Self::AXIS_Z, // Hands: wrist flexion/extension
        Self::AXIS_X, Self::AXIS_X, // Feet: dorsi/plantar flexion
        Self::AXIS_X, Self::AXIS_X, // Toes: up-down curl
    ];

    /// Secondary (twist / second swing) axis per 2-DOF slot. Unused for 1/3-DOF.
    pub const BONE_AXIS_B: [u8; 21] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0,
        Self::AXIS_X, Self::AXIS_X, // LowerArms: pronation/supination along the arm
        Self::AXIS_Y, Self::AXIS_Y, // LowerLegs: tibial twist along the shin
        Self::AXIS_Y, Self::AXIS_Y, // Shoulders: clavicle front-back
        Self::AXIS_Y, Self::AXIS_Y, // Hands: radial/ulnar deviation
        Self::AXIS_Y, Self::AXIS_Y, // Feet: in-out twist
        0, 0,
    ];

    /// Half-range in radians for the primary angle, per slot.
    // The literals are the exact values the C# client quantizes against; substituting the
    // library constants would change the wire quantization by a few ulps.
    #[allow(clippy::approx_constant)]
    pub const BONE_RANGE_A: [f32; 21] = [
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        2.7925, 2.7925, // elbows ±160°
        2.7925, 2.7925, // knees ±160°
        1.0472, 1.0472, // shoulders ±60°
        1.7453, 1.7453, // wrists ±100°
        1.3963, 1.3963, // ankles ±80°
        1.0472, 1.0472, // toes ±60°
    ];

    /// Half-range in radians for the secondary angle, per 2-DOF slot.
    #[allow(clippy::approx_constant)]
    pub const BONE_RANGE_B: [f32; 21] = [
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        1.7453, 1.7453, // forearm twist ±100°
        1.0472, 1.0472, // tibial twist ±60°
        1.0472, 1.0472, // shoulder front-back ±60°
        1.0472, 1.0472, // wrist deviation ±60°
        1.0472, 1.0472, // ankle in-out ±60°
        0.0, 0.0,
    ];

    // Angle bits per quality (VeryLow, Low, Medium, High).
    const HINGE_BITS: [u8; 4] = [6, 7, 9, 13];
    const TWIST_BITS: [u8; 4] = [5, 6, 8, 12];
    const SINGLE_BITS: [u8; 4] = [4, 4, 5, 7];

    pub fn hinge_bits(q: BitQuality) -> u32 {
        u32::from(Self::HINGE_BITS[q.index()])
    }
    pub fn twist_bits(q: BitQuality) -> u32 {
        u32::from(Self::TWIST_BITS[q.index()])
    }
    pub fn single_axis_bits(q: BitQuality) -> u32 {
        u32::from(Self::SINGLE_BITS[q.index()])
    }

    /// Wire width in bits of one explicit bone slot at the given quality.
    pub fn bone_field_width(q: BitQuality, slot: usize) -> u32 {
        match Self::BONE_DOF[slot] {
            3 => 2 + 3 * u32::from(Self::get_bpc_table(q)[slot]),
            2 => Self::hinge_bits(q) + Self::twist_bits(q),
            _ => Self::single_axis_bits(q),
        }
    }

    // ── Hinge/twist factorization ──

    #[inline]
    fn get_component(qx: f32, qy: f32, qz: f32, axis: u8) -> f32 {
        if axis == 0 { qx } else if axis == 1 { qy } else { qz }
    }

    /// Factorizes a unit quaternion as R_axisA(angleA) * R_axisB(angleB). Returns `(angle_a, angle_b)`.
    pub fn extract_hinge_twist(mut qx: f32, mut qy: f32, mut qz: f32, mut qw: f32, axis_a: u8, axis_b: u8) -> (f32, f32) {
        if qw < 0.0 {
            qx = -qx;
            qy = -qy;
            qz = -qz;
            qw = -qw;
        }

        // Twist about axisB: normalize the (q[axisB], w) projection.
        let pb = Self::get_component(qx, qy, qz, axis_b);
        let len = f64::from(pb * pb + qw * qw).sqrt() as f32;
        let (angle_b, tb, tw) = if len > 1e-6 {
            let angle_b = 2.0 * (f64::from(pb).atan2(f64::from(qw)) as f32);
            let inv = 1.0 / len;
            (angle_b, pb * inv, qw * inv)
        } else {
            // Pure 180° rotation about an axis orthogonal to axisB — outside every restricted
            // joint's range. Treat as no twist.
            (0.0, 0.0, 1.0)
        };

        // swing = q * conj(twist). conj(twist) has -tb on axisB and w = tw.
        let cx = if axis_b == 0 { -tb } else { 0.0 };
        let cy = if axis_b == 1 { -tb } else { 0.0 };
        let cz = if axis_b == 2 { -tb } else { 0.0 };
        let mut sw = qw * tw - qx * cx - qy * cy - qz * cz;
        let mut sx = qw * cx + qx * tw + qy * cz - qz * cy;
        let mut sy = qw * cy - qx * cz + qy * tw + qz * cx;
        let mut sz = qw * cz + qx * cy - qy * cx + qz * tw;

        if sw < 0.0 {
            sx = -sx;
            sy = -sy;
            sz = -sz;
            sw = -sw;
        }
        let angle_a = 2.0 * (f64::from(Self::get_component(sx, sy, sz, axis_a)).atan2(f64::from(sw)) as f32);
        (angle_a, angle_b)
    }

    /// Rebuilds q = R_axisA(angleA) * R_axisB(angleB). Returns `(qx, qy, qz, qw)`.
    pub fn compose_hinge_twist(axis_a: u8, angle_a: f32, axis_b: u8, angle_b: f32) -> (f32, f32, f32, f32) {
        let sa = f64::from(angle_a * 0.5).sin() as f32;
        let ca = f64::from(angle_a * 0.5).cos() as f32;
        let sb = f64::from(angle_b * 0.5).sin() as f32;
        let cb = f64::from(angle_b * 0.5).cos() as f32;
        let (ax, ay, az) = (
            if axis_a == 0 { sa } else { 0.0 },
            if axis_a == 1 { sa } else { 0.0 },
            if axis_a == 2 { sa } else { 0.0 },
        );
        let (bx, by, bz) = (
            if axis_b == 0 { sb } else { 0.0 },
            if axis_b == 1 { sb } else { 0.0 },
            if axis_b == 2 { sb } else { 0.0 },
        );
        let qw = ca * cb - ax * bx - ay * by - az * bz;
        let qx = ca * bx + ax * cb + ay * bz - az * by;
        let qy = ca * by - ax * bz + ay * cb + az * bx;
        let qz = ca * bz + ax * by - ay * bx + az * cb;
        (qx, qy, qz, qw)
    }

    /// Signed rotation angle about a single fixed axis (1-DOF joints).
    pub fn extract_single_axis(mut qx: f32, mut qy: f32, mut qz: f32, mut qw: f32, axis_a: u8) -> f32 {
        if qw < 0.0 {
            qx = -qx;
            qy = -qy;
            qz = -qz;
            qw = -qw;
        }
        2.0 * (f64::from(Self::get_component(qx, qy, qz, axis_a)).atan2(f64::from(qw)) as f32)
    }

    /// Encodes a restricted (1/2-DOF) bone slot's rotation into its wire field.
    /// Layout LSB-first: [angleA][angleB]. Use [`Self::bone_field_width`] for the width.
    pub fn encode_restricted(qx: f32, qy: f32, qz: f32, qw: f32, slot: usize, q: BitQuality) -> u64 {
        if Self::BONE_DOF[slot] == 1 {
            let angle = Self::extract_single_axis(qx, qy, qz, qw, Self::BONE_AXIS_A[slot]);
            return u64::from(Self::encode_signed_unit(angle / Self::BONE_RANGE_A[slot], Self::single_axis_bits(q)));
        }

        let (angle_a, angle_b) = Self::extract_hinge_twist(qx, qy, qz, qw, Self::BONE_AXIS_A[slot], Self::BONE_AXIS_B[slot]);
        let bits_a = Self::hinge_bits(q);
        let ea = u64::from(Self::encode_signed_unit(angle_a / Self::BONE_RANGE_A[slot], bits_a));
        let eb = u64::from(Self::encode_signed_unit(angle_b / Self::BONE_RANGE_B[slot], Self::twist_bits(q)));
        ea | (eb << bits_a)
    }

    /// Decodes a restricted bone field back into a unit quaternion `(qx, qy, qz, qw)`.
    pub fn decode_restricted(packed: u64, slot: usize, q: BitQuality) -> (f32, f32, f32, f32) {
        if Self::BONE_DOF[slot] == 1 {
            let bits = Self::single_axis_bits(q);
            let angle = Self::decode_signed_unit((packed & ((1u64 << bits) - 1)) as u32, bits) * Self::BONE_RANGE_A[slot];
            let s = f64::from(angle * 0.5).sin() as f32;
            let qw = f64::from(angle * 0.5).cos() as f32;
            let axis = Self::BONE_AXIS_A[slot];
            return (
                if axis == 0 { s } else { 0.0 },
                if axis == 1 { s } else { 0.0 },
                if axis == 2 { s } else { 0.0 },
                qw,
            );
        }

        let bits_a = Self::hinge_bits(q);
        let bits_b = Self::twist_bits(q);
        let angle_a = Self::decode_signed_unit((packed & ((1u64 << bits_a) - 1)) as u32, bits_a) * Self::BONE_RANGE_A[slot];
        let angle_b = Self::decode_signed_unit(((packed >> bits_a) & ((1u64 << bits_b) - 1)) as u32, bits_b) * Self::BONE_RANGE_B[slot];
        Self::compose_hinge_twist(Self::BONE_AXIS_A[slot], angle_a, Self::BONE_AXIS_B[slot], angle_b)
    }

    // ── Finger block (v47) ──

    /// Bone slots that still carry an explicit smallest-three rotation: 0..20 are the body,
    /// limbs, extremities and toes; the thirty finger joints ride as ten curl/splay channels.
    pub const WIRE_BONE_SLOT_COUNT: usize = 21;
    /// One curl/splay pair per finger, ordered L thumb→little then R thumb→little.
    pub const FINGER_CHANNEL_COUNT: usize = 10;
    /// Wire fields in the rotation region: explicit bone rotations, then finger channels.
    pub const ROTATION_FIELD_COUNT: usize = Self::WIRE_BONE_SLOT_COUNT + Self::FINGER_CHANNEL_COUNT;

    const CURL_BITS: [u8; 4] = [5, 6, 7, 8];
    const SPLAY_BITS: [u8; 4] = [3, 4, 5, 6];

    pub fn curl_bits(q: BitQuality) -> u32 {
        u32::from(Self::CURL_BITS[q.index()])
    }
    pub fn splay_bits(q: BitQuality) -> u32 {
        u32::from(Self::SPLAY_BITS[q.index()])
    }
    pub fn finger_field_width(q: BitQuality) -> u32 {
        Self::curl_bits(q) + Self::splay_bits(q)
    }

    /// Bit width of every wire field in the rotation region, in write order.
    pub fn build_rotation_field_widths(q: BitQuality) -> Vec<u32> {
        let mut widths = vec![0u32; Self::ROTATION_FIELD_COUNT];
        for slot in 0..Self::WIRE_BONE_SLOT_COUNT {
            widths[slot] = Self::bone_field_width(q, slot);
        }
        let finger_width = Self::finger_field_width(q);
        for f in 0..Self::FINGER_CHANNEL_COUNT {
            widths[Self::WIRE_BONE_SLOT_COUNT + f] = finger_width;
        }
        widths
    }

    /// Start bit of every rotation field, relative to the rotation region. Returns total bits.
    pub fn build_rotation_field_offsets(q: BitQuality, out_offsets: &mut [usize]) -> usize {
        let widths = Self::build_rotation_field_widths(q);
        let mut pos = 0usize;
        for (i, w) in widths.iter().enumerate() {
            out_offsets[i] = pos;
            pos += *w as usize;
        }
        pos
    }

    /// Quantizes a signed unit scalar. Values outside [-1, 1] clamp rather than wrap, and a
    /// non-finite input encodes as the midpoint.
    #[inline]
    pub fn encode_signed_unit(value: f32, bits: u32) -> u32 {
        let max_q = (1u32 << bits) - 1;
        if value.is_nan() {
            return (max_q + 1) >> 1;
        }
        let clamped = value.clamp(-1.0, 1.0);
        (round_half_even((clamped * 0.5 + 0.5) * max_q as f32) as u32).min(max_q)
    }

    #[inline]
    pub fn decode_signed_unit(quantized: u32, bits: u32) -> f32 {
        let max_q = (1u32 << bits) - 1;
        quantized as f32 / max_q as f32 * 2.0 - 1.0
    }

    pub fn get_bpc_table(q: BitQuality) -> &'static [u8; 51] {
        match q {
            BitQuality::High => &Self::BPC_HIGH,
            BitQuality::Medium => &Self::BPC_MEDIUM,
            BitQuality::Low => &Self::BPC_LOW,
            BitQuality::VeryLow => &Self::BPC_VERY_LOW,
        }
    }

    // ── Size calculations ──

    pub fn rotation_bits(q: BitQuality) -> usize {
        Self::build_rotation_field_widths(q).iter().map(|w| *w as usize).sum()
    }

    pub fn rotation_bytes(q: BitQuality) -> usize {
        (Self::rotation_bits(q) + 7) >> 3
    }

    /// End-effector anchoring block (hand/foot world targets), High quality only.
    pub const END_EFFECTOR_BLOCK_BYTES: usize = 35;
    pub fn end_effector_bytes(q: BitQuality) -> usize {
        if q == BitQuality::High { Self::END_EFFECTOR_BLOCK_BYTES } else { 0 }
    }

    pub fn convert_to_size(q: BitQuality) -> usize {
        BasisAvatarBitPacking::position_bytes(q) + Self::rotation_bytes(q) + Self::TAIL_BYTES + Self::end_effector_bytes(q)
    }

    // ── Smallest-Three Encode / Decode ──

    /// Encodes a unit quaternion (x,y,z,w) using "smallest three" compression within
    /// [-max_range, max_range]. Use INV_SQRT2 for full-range joints.
    #[inline]
    pub fn encode_smallest_three(mut qx: f32, mut qy: f32, mut qz: f32, mut qw: f32, bpc: u32, max_range: f32) -> u64 {
        let (ax, ay, az, aw) = (qx.abs(), qy.abs(), qz.abs(), qw.abs());

        // Find largest absolute component
        let mut max_idx = 0u64;
        let mut max_val = ax;
        if ay > max_val {
            max_idx = 1;
            max_val = ay;
        }
        if az > max_val {
            max_idx = 2;
            max_val = az;
        }
        if aw > max_val {
            max_idx = 3;
        }

        // Negate quaternion if largest is negative
        let negative = match max_idx {
            0 => qx < 0.0,
            1 => qy < 0.0,
            2 => qz < 0.0,
            _ => qw < 0.0,
        };
        if negative {
            qx = -qx;
            qy = -qy;
            qz = -qz;
            qw = -qw;
        }

        // Extract the 3 remaining components
        let (a, b, c) = match max_idx {
            0 => (qy, qz, qw),
            1 => (qx, qz, qw),
            2 => (qx, qy, qw),
            _ => (qx, qy, qz),
        };

        let inv_range = 1.0 / max_range;
        let max_q = (1u32 << bpc) - 1;
        let quant = |v: f32| -> u64 {
            let n = round_half_even(((v * inv_range).clamp(-1.0, 1.0) * 0.5 + 0.5) * max_q as f32) as u32;
            u64::from(n.min(max_q))
        };
        max_idx | (quant(a) << 2) | (quant(b) << (2 + bpc)) | (quant(c) << (2 + 2 * bpc))
    }

    /// Decodes a "smallest three" compressed quaternion into (x,y,z,w). `max_range` must match
    /// the value used during encoding.
    #[inline]
    pub fn decode_smallest_three(packed: u64, bpc: u32, max_range: f32) -> (f32, f32, f32, f32) {
        let mask = (1u64 << bpc) - 1;
        let max_idx = packed & 3;
        let qa = (packed >> 2) & mask;
        let qb = (packed >> (2 + bpc)) & mask;
        let qc = (packed >> (2 + 2 * bpc)) & mask;

        let f_max = mask as f32;
        let a = (qa as f32 / f_max * 2.0 - 1.0) * max_range;
        let b = (qb as f32 / f_max * 2.0 - 1.0) * max_range;
        let c = (qc as f32 / f_max * 2.0 - 1.0) * max_range;

        let d2 = 1.0 - a * a - b * b - c * c;
        let d = if d2 > 0.0 { f64::from(d2).sqrt() as f32 } else { 0.0 };

        let (mut qx, mut qy, mut qz, mut qw) = match max_idx {
            0 => (d, a, b, c),
            1 => (a, d, b, c),
            2 => (a, b, d, c),
            _ => (a, b, c, d),
        };

        // Normalize
        let len = f64::from(qx * qx + qy * qy + qz * qz + qw * qw).sqrt() as f32;
        if len > 1e-8 {
            let inv = 1.0 / len;
            qx *= inv;
            qy *= inv;
            qz *= inv;
            qw *= inv;
        } else {
            qx = 0.0;
            qy = 0.0;
            qz = 0.0;
            qw = 1.0;
        }
        (qx, qy, qz, qw)
    }

    // ── Bitstream read/write ──

    /// Writes into a region assumed already zero; see [`BasisBitCodec::or`].
    #[inline]
    pub fn write_bits(dst: &mut [u8], bit_pos: usize, value: u64, bit_count: u32) {
        BasisBitCodec::or(dst, bit_pos, value, bit_count);
    }

    #[inline]
    pub fn read_bits(src: &[u8], bit_pos: &mut usize, bit_count: u32) -> u64 {
        let value = BasisBitCodec::read(src, *bit_pos, bit_count);
        *bit_pos += bit_count as usize;
        value
    }
}
