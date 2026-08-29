/// One implementation of "read/write N bits at bit offset P in a byte[]", LSB-first, for every
/// bitstream in the avatar pipeline.
///
/// Reads take a single unaligned 64-bit load when the buffer has room for one; writes stay
/// byte-at-a-time on purpose (measured faster in C#, and the store-forwarding argument holds
/// here too). The narrow read path serves fields too close to the end of the buffer to load a
/// whole word over.
pub struct BasisBitCodec;

impl BasisBitCodec {
    /// Widest field the single-load path can serve.
    pub const MAX_WIDE_BITS: u32 = 57;
    const WORD_BYTES: usize = 8;

    #[inline]
    fn load_word(buffer: &[u8], byte_pos: usize) -> u64 {
        u64::from_le_bytes(buffer[byte_pos..byte_pos + 8].try_into().unwrap())
    }

    #[inline]
    fn low_mask(bit_count: u32) -> u64 {
        if bit_count >= 64 { u64::MAX } else { (1u64 << bit_count) - 1 }
    }

    /// Reads `bit_count` bits starting at `bit_pos`. Bits above the count are zero in the result.
    #[inline]
    pub fn read(src: &[u8], bit_pos: usize, bit_count: u32) -> u64 {
        let byte_pos = bit_pos >> 3;
        let bit_in_byte = (bit_pos & 7) as u32;
        if bit_count <= Self::MAX_WIDE_BITS && byte_pos + Self::WORD_BYTES <= src.len() {
            return (Self::load_word(src, byte_pos) >> bit_in_byte) & Self::low_mask(bit_count);
        }
        Self::read_narrow(src, byte_pos, bit_in_byte, bit_count)
    }

    /// ORs `value`'s low `bit_count` bits into the buffer. The destination range must already be
    /// zero. Use [`Self::replace`] when the destination may be dirty.
    #[inline]
    pub fn or(dst: &mut [u8], bit_pos: usize, value: u64, bit_count: u32) {
        Self::or_narrow(dst, bit_pos >> 3, (bit_pos & 7) as u32, value, bit_count);
    }

    /// Overwrites the bit range with `value`'s low `bit_count` bits, clearing whatever was there.
    #[inline]
    pub fn replace(dst: &mut [u8], bit_pos: usize, value: u64, bit_count: u32) {
        Self::replace_narrow(dst, bit_pos >> 3, (bit_pos & 7) as u32, value, bit_count);
    }

    fn read_narrow(src: &[u8], mut byte_pos: usize, mut bit_in_byte: u32, bit_count: u32) -> u64 {
        let mut result = 0u64;
        let mut out_shift = 0u32;
        let mut bits_left = bit_count;
        while bits_left > 0 {
            let room = 8 - bit_in_byte;
            let take = bits_left.min(room);
            let chunk = (u64::from(src[byte_pos]) >> bit_in_byte) & ((1u64 << take) - 1);
            result |= chunk << out_shift;
            out_shift += take;
            bits_left -= take;
            byte_pos += 1;
            bit_in_byte = 0;
        }
        result
    }

    fn or_narrow(dst: &mut [u8], mut byte_pos: usize, mut bit_in_byte: u32, mut value: u64, bit_count: u32) {
        let mut bits_left = bit_count;
        while bits_left > 0 {
            let room = 8 - bit_in_byte;
            let take = bits_left.min(room);
            let chunk = (value & ((1u64 << take) - 1)) as u8;
            dst[byte_pos] |= chunk << bit_in_byte;
            value >>= take;
            bits_left -= take;
            byte_pos += 1;
            bit_in_byte = 0;
        }
    }

    fn replace_narrow(dst: &mut [u8], mut byte_pos: usize, mut bit_in_byte: u32, mut value: u64, bit_count: u32) {
        let mut bits_left = bit_count;
        while bits_left > 0 {
            let room = 8 - bit_in_byte;
            let take = bits_left.min(room);
            let low_mask = (1u32 << take) - 1;
            let clear = (low_mask << bit_in_byte) as u8;
            let chunk = (((value as u32) & low_mask) << bit_in_byte) as u8;
            dst[byte_pos] = (dst[byte_pos] & !clear) | chunk;
            value >>= take;
            bits_left -= take;
            byte_pos += 1;
            bit_in_byte = 0;
        }
    }
}
