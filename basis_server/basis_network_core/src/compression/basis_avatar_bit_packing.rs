use super::basis_bone_rotation_compression::BasisBoneRotationCompression;

/// Expanded quality ladder (anchors preserved: Low/Medium/High).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
#[repr(u8)]
pub enum BitQuality {
    VeryLow = 0,
    Low = 1,
    Medium = 2,
    High = 3,
}

impl BitQuality {
    /// The C# cast `(BitQuality)byte`; out-of-range values map to `None`, which is what
    /// `IsValidQuality` reported for them.
    pub fn from_byte(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::VeryLow),
            1 => Some(Self::Low),
            2 => Some(Self::Medium),
            3 => Some(Self::High),
            _ => None,
        }
    }

    pub fn index(self) -> usize {
        self as usize
    }

    pub const ALL: [BitQuality; 4] = [Self::VeryLow, Self::Low, Self::Medium, Self::High];
}

/// Avatar payload geometry (position / scale / rotation / hips tail sizes) and the quality
/// ladder. The actual bone-rotation bitstream is produced by [`BasisBoneRotationCompression`];
/// the size helpers here delegate to it.
pub struct BasisAvatarBitPacking;

impl BasisAvatarBitPacking {
    /// Hips world position: 3 × signed int24 millimetres (±8388 m, 1 mm steps) at EVERY quality.
    pub const WRITE_POSITION: usize = 9;
    const POSITION_MM_LIMIT: i32 = (1 << 23) - 1;
    pub const WRITE_SCALE: usize = 2;
    pub const WRITE_ROTATION: usize = 7;
    /// Hips local-position delta vs TPose: 3 × signed 13-bit packed into 5 bytes.
    pub const WRITE_HIPS_DELTA: usize = 5;
    /// Hips local-rotation delta vs TPose, smallest-three, 7 bytes.
    pub const WRITE_HIPS_ROTATION: usize = 7;
    /// Per-axis ±1 m envelope for the hips delta.
    pub const HIPS_DELTA_RANGE: f32 = 1.0;
    pub const HIPS_DELTA_BITS: u32 = 13;
    const HIPS_DELTA_MAX_Q: i32 = (1 << (Self::HIPS_DELTA_BITS - 1)) - 1; // 4095
    const HIPS_DELTA_MASK: u32 = (1u32 << Self::HIPS_DELTA_BITS) - 1; // 0x1FFF

    pub const TAIL_BYTES: usize =
        Self::WRITE_SCALE + Self::WRITE_ROTATION + Self::WRITE_HIPS_DELTA + Self::WRITE_HIPS_ROTATION; // 21

    pub fn is_valid_quality(q: u8) -> bool {
        BitQuality::from_byte(q).is_some()
    }

    /// Byte count for the bone rotation bitstream at the given quality. Named MuscleBytes for
    /// backward compatibility with server code.
    pub fn muscle_bytes(q: BitQuality) -> usize {
        BasisBoneRotationCompression::rotation_bytes(q)
    }

    /// Position field size. Uniform across the ladder now, but kept as a per-quality query.
    pub fn position_bytes(_q: BitQuality) -> usize {
        Self::WRITE_POSITION
    }

    pub fn convert_to_size(q: BitQuality) -> usize {
        // Position (9) + BoneRotations (variable) + Posit16 Scale (2) + Rotation (7) + hips tail.
        BasisBoneRotationCompression::convert_to_size(q)
    }

    /// Encodes one world-space axis value (metres) as signed int24 millimetres, little-endian.
    pub fn encode_axis_mm(meters: f32, dst: &mut [u8], offset: usize) -> bool {
        let mm_f = meters * 1000.0;
        let mm: i32 = if mm_f.is_nan() {
            0
        } else if mm_f >= Self::POSITION_MM_LIMIT as f32 {
            Self::POSITION_MM_LIMIT
        } else if mm_f <= -(Self::POSITION_MM_LIMIT as f32) {
            -Self::POSITION_MM_LIMIT
        } else {
            round_half_even(mm_f) as i32
        };
        let Some(out) = dst.get_mut(offset..offset.saturating_add(3)) else {
            return false;
        };
        out.copy_from_slice(&[mm as u8, (mm >> 8) as u8, (mm >> 16) as u8]);
        true
    }

    /// Decodes one signed int24 millimetre axis back to metres. `None` when `src` is too short.
    pub fn decode_axis_mm(src: &[u8], offset: usize) -> Option<f32> {
        let bytes = src.get(offset..offset.checked_add(3)?)?;
        let mut mm = i32::from(bytes[0]) | (i32::from(bytes[1]) << 8) | (i32::from(bytes[2]) << 16);
        // Sign-extend from 24 bits.
        mm = (mm << 8) >> 8;
        Some(mm as f32 * 0.001)
    }

    /// Writes the whole 3-axis position block (metres) as int24 millimetres. False — and nothing
    /// written — when `dst` has no room for all nine bytes at `offset`.
    pub fn encode_position(x: f32, y: f32, z: f32, dst: &mut [u8], offset: usize) -> bool {
        if dst.get(offset..offset.saturating_add(9)).is_none() {
            return false;
        }
        Self::encode_axis_mm(x, dst, offset) && Self::encode_axis_mm(y, dst, offset + 3) && Self::encode_axis_mm(z, dst, offset + 6)
    }

    /// Reads the whole 3-axis position block back into metres as `(x, y, z)`; `None` when `src`
    /// is too short.
    pub fn decode_position(src: &[u8], offset: usize) -> Option<(f32, f32, f32)> {
        Some((
            Self::decode_axis_mm(src, offset)?,
            Self::decode_axis_mm(src, offset + 3)?,
            Self::decode_axis_mm(src, offset + 6)?,
        ))
    }

    /// Packs a hips local-position delta (metres) into `WRITE_HIPS_DELTA` bytes as three signed
    /// 13-bit axes. Overwrites the whole field, so callers need not pre-clear it.
    pub fn encode_hips_delta(x: f32, y: f32, z: f32, dst: &mut [u8], offset: usize) -> bool {
        let packed: u64 = u64::from(Self::quantize_hips_axis(x))
            | (u64::from(Self::quantize_hips_axis(y)) << Self::HIPS_DELTA_BITS)
            | (u64::from(Self::quantize_hips_axis(z)) << (2 * Self::HIPS_DELTA_BITS));
        let Some(out) = dst.get_mut(offset..offset.saturating_add(5)) else {
            return false;
        };
        out.copy_from_slice(&[packed as u8, (packed >> 8) as u8, (packed >> 16) as u8, (packed >> 24) as u8, (packed >> 32) as u8]);
        true
    }

    /// Unpacks the three signed 13-bit axes written by [`Self::encode_hips_delta`]; `None`
    /// when `src` is too short.
    pub fn decode_hips_delta(src: &[u8], offset: usize) -> Option<(f32, f32, f32)> {
        let b = src.get(offset..offset.checked_add(5)?)?;
        let packed: u64 = u64::from(b[0])
            | (u64::from(b[1]) << 8)
            | (u64::from(b[2]) << 16)
            | (u64::from(b[3]) << 24)
            | (u64::from(b[4]) << 32);
        Some((
            Self::dequantize_hips_axis((packed & u64::from(Self::HIPS_DELTA_MASK)) as u32),
            Self::dequantize_hips_axis(((packed >> Self::HIPS_DELTA_BITS) & u64::from(Self::HIPS_DELTA_MASK)) as u32),
            Self::dequantize_hips_axis(((packed >> (2 * Self::HIPS_DELTA_BITS)) & u64::from(Self::HIPS_DELTA_MASK)) as u32),
        ))
    }

    fn quantize_hips_axis(meters: f32) -> u32 {
        let scaled = meters * (Self::HIPS_DELTA_MAX_Q as f32 / Self::HIPS_DELTA_RANGE);
        let q: i32 = if scaled.is_nan() {
            0
        } else if scaled >= Self::HIPS_DELTA_MAX_Q as f32 {
            Self::HIPS_DELTA_MAX_Q
        } else if scaled <= -(Self::HIPS_DELTA_MAX_Q as f32) {
            -Self::HIPS_DELTA_MAX_Q
        } else {
            round_half_even(scaled) as i32
        };
        (q as u32) & Self::HIPS_DELTA_MASK
    }

    fn dequantize_hips_axis(q: u32) -> f32 {
        // Sign-extend from 13 bits.
        let s = ((q << (32 - Self::HIPS_DELTA_BITS)) as i32) >> (32 - Self::HIPS_DELTA_BITS);
        s as f32 * (Self::HIPS_DELTA_RANGE / Self::HIPS_DELTA_MAX_Q as f32)
    }
}

/// .NET `Math.Round` rounds half to even; Rust's `round` rounds half away from zero. The
/// quantizers here must produce the same integers on both ends, so every rounding of a float
/// that the C# did with `Math.Round` goes through this.
#[inline]
pub fn round_half_even(v: f32) -> f32 {
    v.round_ties_even()
}

#[inline]
pub fn round_half_even_f64(v: f64) -> f64 {
    v.round_ties_even()
}
