/// Bit-level primitives used by the avatar delta codec: a bounds-checked LSB-first bit cursor and
/// zig-zag Exponential-Golomb. Exp-Golomb spends bits in proportion to magnitude — 1 bit for
/// zero, 3 for ±1, 5 for ±2..3, and two more per octave after that. Residuals are exact, never
/// companded.
pub struct BasisResidualCodec;

impl BasisResidualCodec {
    /// Widest channel the codec addresses (the int24 position axes).
    pub const MAX_WIDTH: u32 = 24;

    /// Reduces a difference modulo 2^width into the signed range [-2^(width-1), 2^(width-1)).
    #[inline]
    pub fn wrap_signed(diff: i32, width: u32) -> i32 {
        if width >= 32 {
            return diff;
        }
        let shift = 32 - width;
        (diff << shift) >> shift
    }

    /// Number of significant bits in `v`; 0 for 0. One LZCNT instead of the C# shift loop.
    #[inline]
    pub fn bit_length(v: u32) -> u32 {
        32 - v.leading_zeros()
    }

    /// Exact bit cost of [`BitWriter::write_signed_eg`], without writing anything.
    pub fn signed_eg_bits(value: i32) -> u32 {
        let zz = ((value << 1) ^ (value >> 31)) as u32;
        2 * Self::bit_length(zz.wrapping_add(1)) - 1
    }
}

/// Overwriting LSB-first bit writer. Does not require a pre-cleared buffer. A write past the
/// end of the buffer is dropped and latches [`overflowed`](Self::overflowed) — callers size
/// their buffers from the layout, so this only ever guards a bug, but it never faults.
pub struct BitWriter<'a> {
    buf: &'a mut [u8],
    bit: usize,
    overflowed: bool,
}

impl<'a> BitWriter<'a> {
    pub fn new(buffer: &'a mut [u8], start_bit: usize) -> Self {
        Self { buf: buffer, bit: start_bit, overflowed: false }
    }

    pub fn bit_position(&self) -> usize {
        self.bit
    }

    /// True once a write did not fit the buffer; the output is then incomplete.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    pub fn write_bits(&mut self, mut value: u64, count: u32) {
        let mut byte_pos = self.bit >> 3;
        let mut in_byte = (self.bit & 7) as u32;
        let mut left = count;
        self.bit += count as usize;
        while left > 0 {
            let room = 8 - in_byte;
            let take = left.min(room);
            let low_mask = (1u32 << take) - 1;
            let clear = (low_mask << in_byte) as u8;
            let chunk = (((value as u32) & low_mask) << in_byte) as u8;
            let Some(byte) = self.buf.get_mut(byte_pos) else {
                self.overflowed = true;
                return;
            };
            *byte = (*byte & !clear) | chunk;
            value >>= take;
            left -= take;
            byte_pos += 1;
            in_byte = 0;
        }
    }

    #[inline]
    pub fn write_bit(&mut self, b: u32) {
        self.write_bits(u64::from(b & 1), 1);
    }

    /// Zig-zag then unsigned Exp-Golomb: 1 bit for 0, 3 for ±1, 5 for ±2..3, and so on.
    pub fn write_signed_eg(&mut self, value: i32) {
        let zz = ((value << 1) ^ (value >> 31)) as u32; // 0,-1,1,-2,2 -> 0,1,2,3,4
        let m = zz.wrapping_add(1);
        let num_bits = BasisResidualCodec::bit_length(m);
        if num_bits > 1 {
            self.write_bits(0, num_bits - 1); // prefix zeros
        }
        self.write_bit(1); // the leading 1 of m
        if num_bits > 1 {
            self.write_bits_msb_first(m, num_bits - 1);
        }
    }

    // Emits the low (count) bits of value, most significant first.
    fn write_bits_msb_first(&mut self, value: u32, count: u32) {
        for i in (0..count).rev() {
            self.write_bit((value >> i) & 1);
        }
    }
}

/// Bounds-checked LSB-first bit reader. Every read past `end_bit` latches `failed` and yields
/// zero instead of panicking — the delta receive path parses attacker-reachable bytes.
pub struct BitReader<'a> {
    buf: &'a [u8],
    bit: usize,
    end: usize,
    failed: bool,
}

impl<'a> BitReader<'a> {
    pub fn new(buffer: &'a [u8], start_bit: usize, end_bit: usize) -> Self {
        Self { buf: buffer, bit: start_bit, end: end_bit, failed: false }
    }

    pub fn bit_position(&self) -> usize {
        self.bit
    }

    pub fn end_bit(&self) -> usize {
        self.end
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn read_bits(&mut self, count: u32) -> u64 {
        if self.failed || self.bit + count as usize > self.end {
            self.failed = true;
            return 0;
        }
        let mut byte_pos = self.bit >> 3;
        let mut in_byte = (self.bit & 7) as u32;
        let mut left = count;
        let mut shift = 0u32;
        let mut out = 0u64;
        self.bit += count as usize;
        while left > 0 {
            let room = 8 - in_byte;
            let take = left.min(room);
            let mask_val = (1u64 << take) - 1;
            // A byte index past the buffer means end_bit overstated the buffer; treat as failed.
            let Some(&b) = self.buf.get(byte_pos) else {
                self.failed = true;
                return 0;
            };
            out |= ((u64::from(b) >> in_byte) & mask_val) << shift;
            shift += take;
            left -= take;
            byte_pos += 1;
            in_byte = 0;
        }
        out
    }

    #[inline]
    pub fn read_bit(&mut self) -> u32 {
        self.read_bits(1) as u32
    }

    pub fn read_signed_eg(&mut self) -> i32 {
        let mut zeros = 0;
        loop {
            if self.failed || self.bit >= self.end {
                self.failed = true;
                return 0;
            }
            if self.read_bit() == 1 {
                break;
            }
            zeros += 1;
            // A valid code never has more prefix zeros than a uint has bits.
            if zeros > 32 {
                self.failed = true;
                return 0;
            }
        }
        let mut m: u32 = 1;
        for _ in 0..zeros {
            m = (m << 1) | self.read_bit();
        }
        if self.failed {
            return 0;
        }
        let zz = m.wrapping_sub(1);
        ((zz >> 1) ^ ((zz & 1).wrapping_neg())) as i32
    }
}
