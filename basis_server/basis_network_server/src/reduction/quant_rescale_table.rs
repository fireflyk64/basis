//! Port of `Reduction/QuantRescaleTable.cs`: fixed reciprocals for the only division the avatar
//! repacker performs, so it can be a multiply.
//!
//! Rescaling a quantized value from `b_src` bits to `b_dst` bits divides by `2^b_src - 1`. Both
//! widths come from the wire layout, so the divisor is one of a handful of constants fixed before
//! any player connects. With `M = ceil(2^S / d)` and `e = M*d - 2^S`, `floor(n*M / 2^S) ==
//! floor(n / d)` holds for every `n < N` exactly when `(N-1)*e < 2^S`; a pair whose bound cannot
//! be met at any usable shift installs no reciprocal and divides.

use std::sync::LazyLock;

pub struct QuantRescaleTable;

struct Tables {
    /// `multiplier[slot] == 0` means the pair divides instead.
    multiplier: Vec<u64>,
    shift_for: Vec<u8>,
}

static TABLES: LazyLock<Tables> = LazyLock::new(|| {
    let stride = QuantRescaleTable::STRIDE;
    let mut multiplier = vec![0u64; stride * stride];
    let mut shift_for = vec![0u8; stride * stride];
    for b_src in 1..=QuantRescaleTable::MAX_BITS {
        for b_dst in 1..=QuantRescaleTable::MAX_BITS {
            if b_src == b_dst {
                continue; // identity; never reaches the table
            }
            let max_src = (1u64 << b_src) - 1;
            let max_dst = (1u64 << b_dst) - 1;
            let num_max = max_src * max_dst + (max_src >> 1);
            // High shifts first: a larger shift shrinks the reciprocal's error term.
            for shift in (32..=62).rev() {
                let pow = 1u64 << shift;
                let m = if max_src == 1 { pow } else { pow / max_src + 1 };
                // The product must stay inside 64 bits for every input in the domain.
                if m != 0 && num_max != 0 && m > u64::MAX / num_max {
                    continue;
                }
                let error = m * max_src - pow;
                if num_max != 0 && error != 0 && error > (pow - 1) / num_max {
                    continue;
                }
                multiplier[b_src * stride + b_dst] = m;
                shift_for[b_src * stride + b_dst] = shift as u8;
                break;
            }
        }
    }
    Tables { multiplier, shift_for }
});

impl QuantRescaleTable {
    /// Widest field the wire layout can produce. Current tables peak at 13.
    pub const MAX_BITS: usize = 16;
    const STRIDE: usize = Self::MAX_BITS + 1;

    /// True when this width pair rescales by multiply rather than by divide.
    pub fn has_reciprocal(b_src: usize, b_dst: usize) -> bool {
        b_src <= Self::MAX_BITS && b_dst <= Self::MAX_BITS && TABLES.multiplier[b_src * Self::STRIDE + b_dst] != 0
    }

    /// The exact scalar this is a fast path for: round-to-nearest rescale of `q_src` from `b_src`
    /// bits to `b_dst` bits.
    pub fn rescale_exact(q_src: u32, b_src: usize, b_dst: usize) -> u32 {
        let max_src = (1u64 << b_src) - 1;
        let max_dst = (1u64 << b_dst) - 1;
        ((u64::from(q_src) * max_dst + (max_src >> 1)) / max_src) as u32
    }

    /// Rescales `q_src`, which must already be masked to `b_src` bits. Anything outside that
    /// domain, or outside the modelled width range, takes the exact 64-bit path.
    #[inline]
    pub fn rescale(q_src: u32, b_src: usize, b_dst: usize) -> u32 {
        if b_src > Self::MAX_BITS || b_dst > Self::MAX_BITS || b_src == 0 {
            return Self::rescale_exact(q_src, b_src, b_dst);
        }
        let max_src = (1u32 << b_src) - 1;
        if q_src > max_src {
            return Self::rescale_exact(q_src, b_src, b_dst);
        }
        let max_dst = (1u32 << b_dst) - 1;
        let num = q_src * max_dst + (max_src >> 1);
        let slot = b_src * Self::STRIDE + b_dst;
        let m = TABLES.multiplier[slot];
        if m == 0 {
            return num / max_src;
        }
        ((u64::from(num) * m) >> TABLES.shift_for[slot]) as u32
    }
}
