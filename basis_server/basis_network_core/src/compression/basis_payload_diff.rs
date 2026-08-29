use fearless_simd::{Level, Simd, SimdBase, SimdMask, dispatch, u8x32};

/// Finds which 8-byte words of an avatar payload differ from its keyframe, as a bitmap, so the
/// delta encoder can rule fields out wholesale instead of unpacking every channel of every one.
///
/// The result is a conservative superset: a field whose words are all clean is provably clean,
/// and the caller runs the exact per-channel comparison on whatever survives. The bulk compare
/// is where the vector width earns its keep — one `simd_eq` clears 32 bytes at whatever level
/// `fearless_simd` detected on the host, and a still player clears the whole payload in a
/// handful of them.
pub struct BasisPayloadDiff;

impl BasisPayloadDiff {
    /// Largest payload this can describe: one bit per 8-byte word in a single `u64`.
    pub const MAX_PAYLOAD_BYTES: usize = 64 * 8;

    /// Returns a bitmap whose bit `w` is set when bytes `[8w, 8w+8)` of the two buffers differ over
    /// the first `length` bytes. Both buffers must have at least `length` bytes; `length` must not
    /// exceed `MAX_PAYLOAD_BYTES`.
    pub fn word_diff_mask(current: &[u8], baseline: &[u8], length: usize) -> u64 {
        // Never read past either buffer: a short buffer compares only what it has.
        let length = length.min(current.len()).min(baseline.len());
        let level = Level::new();
        dispatch!(level, simd => Self::word_diff_mask_impl(simd, current, baseline, length))
    }

    #[inline(always)]
    fn word_diff_mask_impl<S: Simd>(simd: S, current: &[u8], baseline: &[u8], length: usize) -> u64 {
        let mut mask = 0u64;
        let mut i = 0usize;

        // Bulk scan. The point of the vector pass is the SKIP: a block that matches costs one
        // compare and one branch for all of its words, which is the common case by a wide margin.
        const STEP: usize = 32;
        while i + STEP <= length {
            let a = u8x32::from_slice(simd, &current[i..i + STEP]);
            let b = u8x32::from_slice(simd, &baseline[i..i + STEP]);
            if !a.simd_eq(b).all_true() {
                let mut p = i;
                while p < i + STEP {
                    mask |= Self::word_bit(current, baseline, p);
                    p += 8;
                }
            }
            i += STEP;
        }

        while i + 8 <= length {
            mask |= Self::word_bit(current, baseline, i);
            i += 8;
        }
        if i < length {
            mask |= Self::tail_bit(current, baseline, i, length);
        }
        mask
    }

    #[inline(always)]
    fn word_bit(current: &[u8], baseline: &[u8], byte_pos: usize) -> u64 {
        // Endianness is irrelevant: this only ever asks whether the eight bytes are equal.
        let same = match (current.get(byte_pos..byte_pos + 8), baseline.get(byte_pos..byte_pos + 8)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        if same { 0 } else { 1u64 << (byte_pos >> 3) }
    }

    fn tail_bit(current: &[u8], baseline: &[u8], byte_pos: usize, length: usize) -> u64 {
        for k in byte_pos..length {
            if current.get(k) != baseline.get(k) {
                return 1u64 << (byte_pos >> 3);
            }
        }
        0
    }
}
