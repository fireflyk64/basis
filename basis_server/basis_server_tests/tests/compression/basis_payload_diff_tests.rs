//! Pins `BasisPayloadDiff`, and the property the delta encoder actually relies on: a word the
//! scanner reports as clean contains no differing byte.

use basis_network_core::compression::{BasisAvatarChannelMap, BasisPayloadDiff, BitQuality};
use basis_server_tests::support::delta_test_support::TestRng;

fn oracle(a: &[u8], b: &[u8], length: usize) -> u64 {
    let mut mask = 0u64;
    for i in 0..length {
        if a[i] != b[i] {
            mask |= 1u64 << (i >> 3);
        }
    }
    mask
}

#[test]
fn identical_payloads_report_nothing_dirty() {
    let mut rng = TestRng::new(4242);
    for length in 1..=200 {
        let a = rng.bytes(length);
        let b = a.clone();
        assert_eq!(BasisPayloadDiff::word_diff_mask(&a, &b, length), 0);
    }
}

/// One byte changed at a time, across every length and every position — this is where a
/// vector-block boundary or a ragged tail gets a byte wrong.
#[test]
fn single_byte_differences_land_in_the_right_word() {
    let mut rng = TestRng::new(99);
    for length in 1..=200 {
        let a = rng.bytes(length);
        for i in 0..length {
            let mut b = a.clone();
            b[i] ^= 0x01;
            assert_eq!(BasisPayloadDiff::word_diff_mask(&a, &b, length), 1u64 << (i >> 3), "length {length} byte {i}");
        }
    }
}

#[test]
fn matches_the_oracle_on_random_differences() {
    let mut rng = TestRng::new(20260819);
    for _ in 0..400 {
        let length = 1 + rng.next(200);
        let a = rng.bytes(length);
        let mut b = a.clone();
        let changes = rng.next(6);
        for _ in 0..changes {
            let i = rng.next(length);
            b[i] ^= (1 + rng.next(255)) as u8;
        }
        assert_eq!(oracle(&a, &b, length), BasisPayloadDiff::word_diff_mask(&a, &b, length));
    }
}

/// The safety property, stated as the encoder uses it: for every word the scanner leaves clear,
/// all eight of its bytes really are equal.
#[test]
fn a_word_reported_clean_contains_no_difference() {
    let mut rng = TestRng::new(777);
    for _ in 0..400 {
        let length = 1 + rng.next(400);
        let a = rng.bytes(length);
        let mut b = a.clone();
        for _ in 0..rng.next(8) {
            let i = rng.next(length);
            b[i] ^= (1 + rng.next(255)) as u8;
        }
        let mask = BasisPayloadDiff::word_diff_mask(&a, &b, length);
        for i in 0..length {
            if mask & (1u64 << (i >> 3)) != 0 {
                continue;
            }
            assert_eq!(a[i], b[i]);
        }
    }
}

/// The scanner is only ever handed the first `length` bytes. Buffers are pooled and routinely
/// longer than the payload, so trailing junk must not register as motion.
#[test]
fn ignores_bytes_past_the_stated_length() {
    let a = TestRng::new(5).bytes(128);
    let mut b = a.clone();
    for byte in b.iter_mut().skip(100) {
        *byte = !*byte;
    }
    assert_eq!(BasisPayloadDiff::word_diff_mask(&a, &b, 100), 0);
}

/// Every avatar payload must fit the single-u64 word map, or the layout silently loses the
/// prefilter.
#[test]
fn every_quality_layout_fits_the_word_map() {
    for quality in BitQuality::ALL {
        let layout = BasisAvatarChannelMap::for_quality(quality);
        assert!(layout.payload_bytes <= BasisPayloadDiff::MAX_PAYLOAD_BYTES, "{quality:?} payload is {} B, past the {} B word-map ceiling", layout.payload_bytes, BasisPayloadDiff::MAX_PAYLOAD_BYTES);
        assert!(layout.word_mask_usable);
    }
}

/// The map must cover every bit each field owns; recomputed from the channels rather than
/// trusting the constructor.
#[test]
fn field_word_mask_covers_every_bit_of_every_field() {
    for quality in BitQuality::ALL {
        let layout = BasisAvatarChannelMap::for_quality(quality);
        for f in 0..layout.field_count() {
            let words = layout.field_word_mask[f];
            for c in layout.field_channel_start(f)..layout.field_channel_end(f) {
                let channel = layout.channels[c];
                for bit in channel.bit_offset..channel.bit_offset + channel.width as usize {
                    let word = bit >> 6;
                    assert!(words & (1u64 << word) != 0, "{quality:?} field {f}: bit {bit} (word {word}) is not covered by its word mask");
                }
            }
        }
    }
}
