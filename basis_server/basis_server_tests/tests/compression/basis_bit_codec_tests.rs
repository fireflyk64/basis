//! Pins `BasisBitCodec` against a bit-at-a-time oracle written independently below. The codec has
//! two paths — a single unaligned 64-bit access, and the byte-walking loop for fields too close to
//! the end of the buffer — and both are checked against the oracle rather than against each other.

use basis_network_core::compression::BasisBitCodec;
use basis_server_tests::support::delta_test_support::TestRng;

const BUFFER_BYTES: usize = 32;
const BUFFER_BITS: usize = BUFFER_BYTES * 8;

// ── Oracle: the definition of the format, one bit at a time. LSB-first within a byte. ──

fn oracle_read(src: &[u8], bit_pos: usize, bit_count: u32) -> u64 {
    let mut value = 0u64;
    for k in 0..bit_count as usize {
        let b = bit_pos + k;
        if src[b >> 3] & (1 << (b & 7)) != 0 {
            value |= 1u64 << k;
        }
    }
    value
}

fn oracle_or(dst: &mut [u8], bit_pos: usize, value: u64, bit_count: u32) {
    for k in 0..bit_count as usize {
        if (value >> k) & 1 == 0 {
            continue;
        }
        let b = bit_pos + k;
        dst[b >> 3] |= 1 << (b & 7);
    }
}

fn oracle_replace(dst: &mut [u8], bit_pos: usize, value: u64, bit_count: u32) {
    for k in 0..bit_count as usize {
        let b = bit_pos + k;
        let bit = 1u8 << (b & 7);
        if (value >> k) & 1 != 0 {
            dst[b >> 3] |= bit;
        } else {
            dst[b >> 3] &= !bit;
        }
    }
}

fn pattern(seed: u64) -> Vec<u8> {
    TestRng::new(seed).bytes(BUFFER_BYTES)
}

#[test]
fn read_matches_the_oracle_at_every_offset_and_width() {
    let source = pattern(12345);
    for bit_pos in 0..BUFFER_BITS {
        let max_width = 64.min(BUFFER_BITS - bit_pos) as u32;
        for width in 1..=max_width {
            assert_eq!(oracle_read(&source, bit_pos, width), BasisBitCodec::read(&source, bit_pos, width), "pos {bit_pos} width {width}");
        }
    }
}

#[test]
fn or_matches_the_oracle_at_every_offset_and_width() {
    // Values with bits set above the width prove the codec masks rather than bleeding into the
    // neighbouring field — the failure mode that corrupts an adjacent bone silently.
    let values = [0u64, 1, 0x5555555555555555, 0xAAAAAAAAAAAAAAAA, u64::MAX];
    for bit_pos in 0..BUFFER_BITS {
        let max_width = 64.min(BUFFER_BITS - bit_pos) as u32;
        for width in 1..=max_width {
            for &value in &values {
                let mut actual = pattern(777);
                let mut expected = actual.clone();
                BasisBitCodec::or(&mut actual, bit_pos, value, width);
                oracle_or(&mut expected, bit_pos, value, width);
                assert_eq!(expected, actual, "pos {bit_pos} width {width} value {value:#x}");
            }
        }
    }
}

#[test]
fn replace_matches_the_oracle_at_every_offset_and_width() {
    let values = [0u64, 1, 0x5555555555555555, 0xAAAAAAAAAAAAAAAA, u64::MAX];
    for bit_pos in 0..BUFFER_BITS {
        let max_width = 64.min(BUFFER_BITS - bit_pos) as u32;
        for width in 1..=max_width {
            for &value in &values {
                let mut actual = pattern(999);
                let mut expected = actual.clone();
                BasisBitCodec::replace(&mut actual, bit_pos, value, width);
                oracle_replace(&mut expected, bit_pos, value, width);
                assert_eq!(expected, actual, "pos {bit_pos} width {width} value {value:#x}");
            }
        }
    }
}

/// A round trip through the exact-sized buffers the codecs really use. Every field lands at the
/// very end of its buffer at some point, which is the case the narrow path exists for.
#[test]
fn round_trips_at_the_end_of_an_exact_sized_buffer() {
    for size_bytes in [1usize, 2, 3, 5, 8, 9] {
        for width in 1..=57.min(size_bytes * 8) as u32 {
            for bit_pos in 0..=(size_bytes * 8 - width as usize) {
                let value = 0xDEADBEEFCAFEF00Du64 & if width >= 64 { u64::MAX } else { (1u64 << width) - 1 };
                let mut buffer = vec![0u8; size_bytes];
                BasisBitCodec::replace(&mut buffer, bit_pos, value, width);
                assert_eq!(value, BasisBitCodec::read(&buffer, bit_pos, width));

                // Replace must leave every other bit alone.
                let mut neighbours = vec![0xFFu8; size_bytes];
                let mut expected = neighbours.clone();
                BasisBitCodec::replace(&mut neighbours, bit_pos, value, width);
                oracle_replace(&mut expected, bit_pos, value, width);
                assert_eq!(expected, neighbours);
            }
        }
    }
}

/// Guards the coverage itself: if the buffer above were ever sized so that everything fell down
/// the narrow path, the tests would still pass and the wide path would ship unchecked.
#[test]
fn both_paths_are_actually_exercised() {
    const WORD_BYTES: usize = 8;
    let (mut wide, mut narrow) = (0, 0);
    for bit_pos in 0..BUFFER_BITS {
        if (bit_pos >> 3) + WORD_BYTES <= BUFFER_BYTES {
            wide += 1;
        } else {
            narrow += 1;
        }
    }
    assert!(wide > 0, "no offset in the fixture reaches the wide path");
    assert!(narrow > 0, "no offset in the fixture reaches the narrow path");
}

/// Widths past `MAX_WIDE_BITS` cannot use a single word load and must still be correct.
#[test]
fn widths_beyond_the_wide_limit_still_round_trip() {
    for width in BasisBitCodec::MAX_WIDE_BITS + 1..=64 {
        for bit_pos in 0..16 {
            let mask = if width >= 64 { u64::MAX } else { (1u64 << width) - 1 };
            let value = 0x0123456789ABCDEFu64 & mask;
            let mut buffer = vec![0u8; BUFFER_BYTES];
            BasisBitCodec::replace(&mut buffer, bit_pos, value, width);
            assert_eq!(value, BasisBitCodec::read(&buffer, bit_pos, width));
        }
    }
}

/// A field that runs past the buffer is clipped to the bits that exist — never read or written
/// past the end, and never a panic.
#[test]
fn a_field_past_the_end_is_clipped_not_overrun() {
    let mut buffer = vec![0u8; 4];
    assert_eq!(BasisBitCodec::read(&buffer, 30, 8), 0);
    BasisBitCodec::replace(&mut buffer, 30, u64::MAX, 8);
    assert_eq!(buffer, [0, 0, 0, 0xC0], "only the two bits inside the buffer may be written");
    BasisBitCodec::or(&mut buffer, 40, u64::MAX, 8);
    assert_eq!(buffer, [0, 0, 0, 0xC0], "a field entirely past the end writes nothing");
    assert_eq!(BasisBitCodec::read(&buffer, 40, 8), 0);
}
