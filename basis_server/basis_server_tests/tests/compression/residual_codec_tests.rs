//! The primitives both avatar codecs are built on. These are the invariants that, if they break,
//! break everything downstream silently rather than loudly.

use basis_network_core::compression::{BasisAvatarDeltaCompression, BasisResidualCodec, BitReader, BitWriter, BitQuality};
use basis_server_tests::support::DeltaTestSupport as S;
use basis_server_tests::support::delta_test_support::TestRng;

// ── Channel map ──

/// The channel list must be a TOTAL PARTITION of the payload — contiguous, non-overlapping, and
/// covering every bit including structural padding.
#[test]
fn channel_map_totally_partitions_the_payload() {
    for q in BitQuality::ALL {
        let layout = S::layout(q);
        let mut expected = 0usize;
        for ch in &layout.channels {
            assert_eq!(expected, ch.bit_offset);
            assert!((1..=BasisResidualCodec::MAX_WIDTH).contains(&(ch.width as u32)));
            expected += ch.width as usize;
        }
        assert_eq!(layout.payload_bits, expected);
        assert_eq!(layout.payload_bits, layout.total_channel_bits);
        assert_eq!(S::payload_size(q), layout.payload_bytes);
    }
}

#[test]
fn channel_map_field_bounds_are_contiguous_and_cover_every_channel() {
    for q in BitQuality::ALL {
        let layout = S::layout(q);
        assert_eq!(BasisAvatarDeltaCompression::FIELD_COUNT, layout.field_count());
        assert_eq!(0, layout.field_channel_start(0));
        for f in 0..layout.field_count() {
            assert_eq!(layout.field_channel_end(f), layout.field_channel_start(f + 1));
        }
        assert_eq!(layout.channels.len(), layout.field_channel_end(layout.field_count() - 1));

        // The end-effector field is empty below High, where the block is not sent at all.
        let eff_field = BasisAvatarDeltaCompression::FIELD_COUNT - 1;
        let eff_channels = layout.field_channel_end(eff_field) - layout.field_channel_start(eff_field);
        if S::end_effector_bytes(q) > 0 {
            assert!(eff_channels > 0);
        } else {
            assert_eq!(eff_channels, 0);
        }
    }
}

#[test]
fn read_channel_write_channel_round_trip_every_channel() {
    for q in BitQuality::ALL {
        let mut rng = TestRng::new(31 + q as u64);
        let layout = S::layout(q);
        let mut payload = S::make_payload(q, &mut rng);
        for ch in &layout.channels {
            let v = (rng.next_u64() as u32) & ch.mask();
            BasisAvatarDeltaCompression::write_channel(&mut payload, ch, v);
            assert_eq!(v, BasisAvatarDeltaCompression::read_channel(&payload, ch));
        }
        // Writing every channel back must not have disturbed a neighbour: read them all again.
        let expected: Vec<u32> = layout.channels.iter().map(|ch| BasisAvatarDeltaCompression::read_channel(&payload, ch)).collect();
        let mut rebuilt = vec![0u8; payload.len()];
        for (i, ch) in layout.channels.iter().enumerate() {
            BasisAvatarDeltaCompression::write_channel(&mut rebuilt, ch, expected[i]);
        }
        assert_eq!(payload, rebuilt);
    }
}

// ── Exponential-Golomb ──

#[test]
fn signed_eg_round_trips_and_cost_matches_the_advertised_bit_count() {
    let mut buf = vec![0u8; 64];
    let mut values = vec![0, 1, -1, 2, -2, 3, -3, 6, -6, 7, -7, 100, -100, 65535, -65535, i32::MAX / 4, -(i32::MAX / 4)];
    let mut rng = TestRng::new(7);
    for _ in 0..5000 {
        values.push(rng.next_range(-(1 << 24), 1 << 24));
    }
    for v in values {
        buf.fill(0);
        let position = {
            let mut w = BitWriter::new(&mut buf, 0);
            w.write_signed_eg(v);
            assert_eq!(BasisResidualCodec::signed_eg_bits(v) as usize, w.bit_position());
            w.bit_position()
        };
        let mut r = BitReader::new(&buf, 0, position);
        assert_eq!(v, r.read_signed_eg());
        assert!(!r.failed());
        assert_eq!(position, r.bit_position());
    }
}

#[test]
fn signed_eg_zero_costs_one_bit_and_cost_grows_with_magnitude() {
    assert_eq!(BasisResidualCodec::signed_eg_bits(0), 1);
    assert_eq!(BasisResidualCodec::signed_eg_bits(1), 3);
    assert_eq!(BasisResidualCodec::signed_eg_bits(-1), 3);
    assert_eq!(BasisResidualCodec::signed_eg_bits(2), 5);
    assert_eq!(BasisResidualCodec::signed_eg_bits(-2), 5);
    let mut prev = 0;
    for v in 0..4096 {
        let bits = BasisResidualCodec::signed_eg_bits(v);
        assert!(bits >= prev, "cost must be non-decreasing in magnitude");
        prev = bits;
    }
}

#[test]
fn bit_reader_past_the_end_fails_instead_of_panicking() {
    let buf = [0u8; 4];
    let mut r = BitReader::new(&buf, 0, 8);
    r.read_bits(8);
    assert!(!r.failed());
    r.read_bits(1);
    assert!(r.failed());

    // An all-zero buffer is an unterminated Exp-Golomb prefix; it must give up, not spin.
    let zeros = vec![0u8; 512];
    let mut r2 = BitReader::new(&zeros, 0, 512 * 8);
    r2.read_signed_eg();
    assert!(r2.failed());
}

// ── Exactness ──

/// Residual coding must be LOSSLESS for every channel width and every possible pair of values.
#[test]
fn residual_coding_is_lossless_for_every_width_and_value_pair() {
    let mut rng = TestRng::new(4242);
    let mut buf = vec![0u8; 16];
    for w in 2..=BasisResidualCodec::MAX_WIDTH {
        let mask = (1u32 << w) - 1;
        for _ in 0..4000 {
            let cur = (rng.next_u64() as u32) & mask;
            let est = (rng.next_u64() as u32) & mask;
            let residual = BasisResidualCodec::wrap_signed(cur as i32 - est as i32, w);

            buf.fill(0);
            let position = {
                let mut wtr = BitWriter::new(&mut buf, 0);
                wtr.write_signed_eg(residual);
                wtr.bit_position()
            };
            let mut rdr = BitReader::new(&buf, 0, position);
            let decoded = rdr.read_signed_eg();
            assert!(!rdr.failed());
            assert_eq!(residual, decoded);

            // The reconstruction the codecs perform must land exactly on the sender's value.
            assert_eq!(cur, ((est as i32).wrapping_add(decoded) as u32) & mask);
        }
    }
}

/// A residual can cost more than the value it describes, so both codecs fall back to a verbatim
/// field: a field is never worse than its own width plus one mode bit.
#[test]
fn verbatim_fallback_bounds_the_worst_case_residual() {
    for w in 2..=BasisResidualCodec::MAX_WIDTH {
        let limit = 1i32 << (w - 1);
        let mut worst = 0;
        for v in [-limit, -limit + 1, -1, 0, 1, limit - 1] {
            worst = worst.max(BasisResidualCodec::signed_eg_bits(v));
        }
        assert!(worst > w, "w={w}: worst residual {worst} bits should exceed the {w}-bit raw form");
        assert!(worst <= 2 * w + 1);
    }
}

#[test]
fn wrap_signed_round_trips_through_masked_reconstruction() {
    let mut rng = TestRng::new(99);
    for w in 2..=BasisResidualCodec::MAX_WIDTH {
        let mask = (1u32 << w) - 1;
        for _ in 0..2000 {
            let a = (rng.next_u64() as u32) & mask;
            let b = (rng.next_u64() as u32) & mask;
            let diff = BasisResidualCodec::wrap_signed(a as i32 - b as i32, w);
            assert!((-(1i32 << (w - 1))..=(1i32 << (w - 1)) - 1).contains(&diff));
            assert_eq!(a, ((b as i32).wrapping_add(diff) as u32) & mask);
        }
    }
}
