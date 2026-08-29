//! Core build_delta / try_apply_delta / delta_body_length round-trip behavior.

use basis_network_core::compression::{BasisAvatarBitPacking, BasisAvatarDeltaCompression, BasisBoneRotationCompression, BitQuality};
use basis_server_tests::support::DeltaTestSupport as S;
use basis_server_tests::support::delta_test_support::TestRng;

#[test]
fn round_trip_random_payloads() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(1000 + q as u64);
        for _ in 0..500 {
            S::assert_round_trip(&S::make_payload(q, &mut rng), &S::make_payload(q, &mut rng), q);
        }
    }
}

#[test]
fn round_trip_realistic_quaternion_payloads() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(2000 + q as u64);
        for _ in 0..500 {
            S::assert_round_trip(&S::make_realistic_payload(q, &mut rng), &S::make_realistic_payload(q, &mut rng), q);
        }
    }
}

#[test]
fn round_trip_realistic_small_motion() {
    // A small pose nudge: re-encode each bone from a slightly rotated quaternion. Many bones
    // quantize to the same bits (unchanged), which is the common in-session case.
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(3000 + q as u64);
        let bpc = S::bpc(q);
        for _ in 0..100 {
            let kf = S::make_realistic_payload(q, &mut rng);
            let mut cur = kf.clone();
            for s in 0..S::WIRE_BONE_SLOTS {
                let (x, y, z, w) = BasisBoneRotationCompression::decode_smallest_three(S::get_bone(&kf, q, s), bpc[s] as u32, BasisBoneRotationCompression::MAX_COMPONENT[s]);
                let nudge = 0.002f32;
                let nx = x + (rng.next_f64() * 2.0 - 1.0) as f32 * nudge;
                let ny = y + (rng.next_f64() * 2.0 - 1.0) as f32 * nudge;
                let nz = z + (rng.next_f64() * 2.0 - 1.0) as f32 * nudge;
                let nw = w + (rng.next_f64() * 2.0 - 1.0) as f32 * nudge;
                let packed = BasisBoneRotationCompression::encode_smallest_three(nx, ny, nz, nw, bpc[s] as u32, BasisBoneRotationCompression::MAX_COMPONENT[s]);
                S::set_bone(&mut cur, q, s, packed);
            }
            S::assert_round_trip(&kf, &cur, q);
        }
    }
}

#[test]
fn build_delta_is_deterministic() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(42);
        let kf = S::make_payload(q, &mut rng);
        let cur = S::make_payload(q, &mut rng);
        let mut a = vec![0u8; BasisAvatarDeltaCompression::max_delta_size(q)];
        let mut b = vec![0u8; BasisAvatarDeltaCompression::max_delta_size(q)];
        let la = BasisAvatarDeltaCompression::build_delta(&kf, &cur, q, &mut a, 0).unwrap();
        let lb = BasisAvatarDeltaCompression::build_delta(&kf, &cur, q, &mut b, 0).unwrap();
        assert_eq!(la, lb);
        assert_eq!(&a[..la], &b[..lb]);
    }
}

#[test]
fn build_delta_honors_dst_start_offset() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(7);
        let kf = S::make_payload(q, &mut rng);
        let cur = S::make_payload(q, &mut rng);
        const START: usize = 37;
        let mut dst = vec![0u8; START + BasisAvatarDeltaCompression::max_delta_size(q)];
        dst[..START].fill(0xEE); // sentinel that must survive
        let len = BasisAvatarDeltaCompression::build_delta(&kf, &cur, q, &mut dst, START).unwrap();
        assert!(len > 0);
        assert!(dst[..START].iter().all(|&b| b == 0xEE));
        assert_eq!(BasisAvatarDeltaCompression::delta_body_length(&dst, START, len, q), Some(len));
        let mut recon = vec![0u8; S::payload_size(q)];
        assert!(BasisAvatarDeltaCompression::try_apply_delta(&kf, &dst, START, len, q, &mut recon));
        assert_eq!(cur, recon);
    }
}

#[test]
fn try_apply_delta_is_idempotent() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(11);
        let kf = S::make_payload(q, &mut rng);
        let cur = S::make_payload(q, &mut rng);
        let mut dst = vec![0u8; BasisAvatarDeltaCompression::max_delta_size(q)];
        let len = BasisAvatarDeltaCompression::build_delta(&kf, &cur, q, &mut dst, 0).unwrap();
        let mut r1 = vec![0u8; S::payload_size(q)];
        let mut r2 = vec![0u8; S::payload_size(q)];
        assert!(BasisAvatarDeltaCompression::try_apply_delta(&kf, &dst, 0, len, q, &mut r1));
        assert!(BasisAvatarDeltaCompression::try_apply_delta(&kf, &dst, 0, len, q, &mut r2));
        assert_eq!(r1, r2);
        assert_eq!(cur, r1);
    }
}

#[test]
fn payload_sizes_match_expected_ladder() {
    // Position is int24 mm (9B) at every tier and the hips tail is 21B (13-bit hips delta).
    // Rotation bytes shrank in v52: restricted-DOF bones ship angles, not quaternions.
    assert_eq!(S::payload_size(BitQuality::VeryLow), 74); // 9 pos + 44 rot + 21 tail
    assert_eq!(S::payload_size(BitQuality::Low), 83); // 9 pos + 53 rot + 21 tail
    assert_eq!(S::payload_size(BitQuality::Medium), 97); // 9 pos + 67 rot + 21 tail
    assert_eq!(S::payload_size(BitQuality::High), 159); // 9 pos + 94 rot + 21 tail + 35 effector
    assert_eq!(BasisAvatarDeltaCompression::DIRTY_MASK_BYTES, 5);
    assert_eq!(BasisAvatarDeltaCompression::FIELD_COUNT, 37);
}

fn assert_close(expected: f32, actual: f32, tolerance: f32) {
    assert!((expected - actual).abs() <= tolerance, "expected {expected} ± {tolerance}, got {actual}");
}

#[test]
fn quantized_position_round_trips_within_half_millimetre() {
    let mut buf = vec![0u8; BasisAvatarBitPacking::WRITE_POSITION];
    for v in [0f32, 0.001, -0.001, 1.2345, -987.654, 8000.0, -8000.0] {
        assert!(BasisAvatarBitPacking::encode_axis_mm(v, &mut buf, 0));
        assert_close(v, BasisAvatarBitPacking::decode_axis_mm(&buf, 0).unwrap(), 0.0006);
    }

    // Out-of-range and non-finite inputs clamp instead of wrapping.
    BasisAvatarBitPacking::encode_axis_mm(99999.0, &mut buf, 0);
    assert_close(8388.607, BasisAvatarBitPacking::decode_axis_mm(&buf, 0).unwrap(), 0.001);
    BasisAvatarBitPacking::encode_axis_mm(-99999.0, &mut buf, 0);
    assert_close(-8388.607, BasisAvatarBitPacking::decode_axis_mm(&buf, 0).unwrap(), 0.001);
    BasisAvatarBitPacking::encode_axis_mm(f32::NAN, &mut buf, 0);
    assert_close(0.0, BasisAvatarBitPacking::decode_axis_mm(&buf, 0).unwrap(), 0.0001);

    // The whole-block helpers lay the three axes out at 0/3/6 and read them back the same way.
    let mut block = vec![0u8; BasisAvatarBitPacking::WRITE_POSITION];
    assert!(BasisAvatarBitPacking::encode_position(1.5, -2.25, 300.125, &mut block, 0));
    let (px, py, pz) = BasisAvatarBitPacking::decode_position(&block, 0).unwrap();
    assert_close(1.5, px, 0.0006);
    assert_close(-2.25, py, 0.0006);
    assert_close(300.125, pz, 0.0006);
    assert_close(1.5, BasisAvatarBitPacking::decode_axis_mm(&block, 0).unwrap(), 0.0006);
    assert_close(-2.25, BasisAvatarBitPacking::decode_axis_mm(&block, 3).unwrap(), 0.0006);
    assert_close(300.125, BasisAvatarBitPacking::decode_axis_mm(&block, 6).unwrap(), 0.0006);

    // A buffer too short for the field is refused, never written past.
    let mut short = vec![0u8; 2];
    assert!(!BasisAvatarBitPacking::encode_axis_mm(1.0, &mut short, 0));
    assert!(BasisAvatarBitPacking::decode_axis_mm(&short, 0).is_none());
}

#[test]
fn hips_delta_round_trips_within_a_quarter_millimetre() {
    let mut buf = vec![0u8; BasisAvatarBitPacking::WRITE_HIPS_DELTA];
    for (x, y, z) in [(0f32, 0f32, 0f32), (0.001, -0.001, 0.5), (-0.25, 0.75, -0.999), (1.0, -1.0, 0.0), (0.3333, -0.6667, 0.1234)] {
        assert!(BasisAvatarBitPacking::encode_hips_delta(x, y, z, &mut buf, 0));
        let (ox, oy, oz) = BasisAvatarBitPacking::decode_hips_delta(&buf, 0).unwrap();
        assert_close(x, ox, 0.00025);
        assert_close(y, oy, 0.00025);
        assert_close(z, oz, 0.00025);
    }

    // An all-zero field must decode to a zero delta — the console test client leaves it unwritten.
    buf.fill(0);
    let (zx, zy, zz) = BasisAvatarBitPacking::decode_hips_delta(&buf, 0).unwrap();
    assert_eq!((zx, zy, zz), (0.0, 0.0, 0.0));

    // Out-of-range and non-finite inputs clamp to the envelope rather than wrapping.
    BasisAvatarBitPacking::encode_hips_delta(99.0, -99.0, f32::NAN, &mut buf, 0);
    let (cx, cy, cz) = BasisAvatarBitPacking::decode_hips_delta(&buf, 0).unwrap();
    assert_close(BasisAvatarBitPacking::HIPS_DELTA_RANGE, cx, 0.00025);
    assert_close(-BasisAvatarBitPacking::HIPS_DELTA_RANGE, cy, 0.00025);
    assert_eq!(cz, 0.0);

    // Encoding overwrites the whole field: no residue survives from a previous value.
    BasisAvatarBitPacking::encode_hips_delta(0.9, -0.9, 0.9, &mut buf, 0);
    BasisAvatarBitPacking::encode_hips_delta(0.0, 0.0, 0.0, &mut buf, 0);
    assert!(buf.iter().all(|&b| b == 0));
}

#[test]
fn max_delta_size_is_upper_bound() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(555 + q as u64);
        let max = BasisAvatarDeltaCompression::max_delta_size(q);
        let mut dst = vec![0u8; max];
        for _ in 0..300 {
            let len = BasisAvatarDeltaCompression::build_delta(&S::make_payload(q, &mut rng), &S::make_payload(q, &mut rng), q, &mut dst, 0).unwrap();
            assert!((BasisAvatarDeltaCompression::DIRTY_MASK_BYTES..=max).contains(&len));
        }
        // Raw mode caps each field at its own verbatim width, so the worst case is the mask plus
        // one mode bit per field plus the payload itself.
        let expected = BasisAvatarDeltaCompression::DIRTY_MASK_BYTES + ((BasisAvatarDeltaCompression::FIELD_COUNT + S::payload_size(q) * 8 + 7) >> 3);
        assert_eq!(max, expected);
        assert!(max - S::payload_size(q) <= BasisAvatarDeltaCompression::DIRTY_MASK_BYTES + 5);
    }
}
