use crate::mathematics::MathExtensions;

/// Functions to Compress Quaternions and Floats.
pub struct BasisNetworkPrimitiveCompression;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BasisRangedUshortFloatData {
    pub precision: f32,
    pub inverse_precision: f32,
    pub min_value: f32,
    pub max_value: f32,
    pub required_bits: i32,
    pub mask: u16,
}

impl BasisRangedUshortFloatData {
    pub const fn new(min_value: f32, max_value: f32, precision: f32) -> Self {
        let inverse_precision = 1.0 / precision;
        let required_bits = Self::calculate_required_bits(min_value, max_value, inverse_precision);
        Self {
            precision,
            inverse_precision,
            min_value,
            max_value,
            required_bits,
            mask: ((1u32 << required_bits) - 1) as u16,
        }
    }

    pub fn compress(&self, value: f32) -> u16 {
        let value = MathExtensions::clamp_f32(value, self.min_value, self.max_value);
        let normalized_value = (value - self.min_value) * self.inverse_precision;
        // C#: (ushort)(normalizedValue + 0.5f) — a truncating cast.
        (((normalized_value + 0.5) as u32) as u16) & self.mask
    }

    pub fn decompress(&self, compressed_value: u16) -> f32 {
        let decompressed_value = (f32::from(compressed_value) * self.precision) + self.min_value;
        MathExtensions::clamp_f32(decompressed_value, self.min_value, self.max_value)
    }

    const fn calculate_required_bits(min_value: f32, max_value: f32, inverse_precision: f32) -> i32 {
        let range = max_value - min_value;
        let max_value_in_range = range * inverse_precision;
        Self::fast_log2((max_value_in_range + 0.5) as u32) + 1
    }

    /// floor(log2(value)); 0 for 0. The C# used a de Bruijn table because .NET Standard had no
    /// intrinsic for it; `leading_zeros` compiles to one instruction (BSR/LZCNT) and gives the
    /// same answer for every input.
    pub const fn fast_log2(value: u32) -> i32 {
        if value == 0 { 0 } else { (31 - value.leading_zeros()) as i32 }
    }
}
