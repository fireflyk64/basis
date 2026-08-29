//! The v52 restricted-DOF bone codec: 2-DOF joints carry a hinge+twist angle pair and 1-DOF toes
//! a single angle, instead of a smallest-three quaternion. These pin the factorization, the
//! quantization error bounds, and the DOF tables themselves.

use basis_network_core::compression::{BasisBoneRotationCompression, BitQuality};
use basis_server_tests::support::delta_test_support::TestRng;

type Q = (f32, f32, f32, f32);

fn axis_angle(axis: u8, angle: f32) -> Q {
    let (s, c) = ((angle * 0.5).sin(), (angle * 0.5).cos());
    (if axis == 0 { s } else { 0.0 }, if axis == 1 { s } else { 0.0 }, if axis == 2 { s } else { 0.0 }, c)
}

fn mul(a: Q, b: Q) -> Q {
    (
        a.3 * b.0 + a.0 * b.3 + a.1 * b.2 - a.2 * b.1,
        a.3 * b.1 - a.0 * b.2 + a.1 * b.3 + a.2 * b.0,
        a.3 * b.2 + a.0 * b.1 - a.1 * b.0 + a.2 * b.3,
        a.3 * b.3 - a.0 * b.0 - a.1 * b.1 - a.2 * b.2,
    )
}

/// Relative rotation angle between two unit quaternions, in radians (2*atan2(|v|,|w|)).
fn angle_between(a: Q, b: Q) -> f32 {
    let conj_a = (-a.0, -a.1, -a.2, a.3);
    let r = mul(conj_a, b);
    let v = (r.0 * r.0 + r.1 * r.1 + r.2 * r.2).sqrt();
    2.0 * v.atan2(r.3.abs())
}

#[test]
fn dof_tables_are_consistent() {
    let n = BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT;
    assert_eq!(BasisBoneRotationCompression::BONE_DOF.len(), n);
    assert_eq!(BasisBoneRotationCompression::BONE_AXIS_A.len(), n);
    assert_eq!(BasisBoneRotationCompression::BONE_AXIS_B.len(), n);
    assert_eq!(BasisBoneRotationCompression::BONE_RANGE_A.len(), n);
    assert_eq!(BasisBoneRotationCompression::BONE_RANGE_B.len(), n);

    for slot in 0..n {
        let dof = BasisBoneRotationCompression::BONE_DOF[slot];
        assert!((1..=3).contains(&dof));
        if dof < 3 {
            assert!((0.1..=std::f32::consts::PI).contains(&BasisBoneRotationCompression::BONE_RANGE_A[slot]));
            assert!(BasisBoneRotationCompression::BONE_AXIS_A[slot] <= 2);
        }
        if dof == 2 {
            assert!((0.1..=std::f32::consts::PI).contains(&BasisBoneRotationCompression::BONE_RANGE_B[slot]));
            // A hinge/twist pair about the same axis would be degenerate.
            assert_ne!(BasisBoneRotationCompression::BONE_AXIS_A[slot], BasisBoneRotationCompression::BONE_AXIS_B[slot]);
        }
    }
}

#[test]
fn hinge_twist_factorization_is_exact_for_two_axis_rotations() {
    // Any rotation genuinely of the form R_A(a) * R_B(b) must extract those angles exactly.
    for axis_a in 0..3u8 {
        for axis_b in 0..3u8 {
            if axis_a == axis_b {
                continue;
            }
            let mut a = -2.6f32;
            while a <= 2.6 {
                let mut b = -1.5f32;
                while b <= 1.5 {
                    let q = mul(axis_angle(axis_a, a), axis_angle(axis_b, b));
                    let (ea, eb) = BasisBoneRotationCompression::extract_hinge_twist(q.0, q.1, q.2, q.3, axis_a, axis_b);
                    assert!((a - ea).abs() <= 1e-3, "a={a} ea={ea}");
                    assert!((b - eb).abs() <= 1e-3, "b={b} eb={eb}");
                    let (rx, ry, rz, rw) = BasisBoneRotationCompression::compose_hinge_twist(axis_a, ea, axis_b, eb);
                    assert!(angle_between(q, (rx, ry, rz, rw)) < 1e-3);
                    b += 0.23;
                }
                a += 0.37;
            }
        }
    }
}

#[test]
fn restricted_codec_round_trip_within_quantization_step() {
    for quality in BitQuality::ALL {
        for slot in 9..BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT {
            let dof = BasisBoneRotationCompression::BONE_DOF[slot];
            let axis_a = BasisBoneRotationCompression::BONE_AXIS_A[slot];
            let axis_b = BasisBoneRotationCompression::BONE_AXIS_B[slot];
            let range_a = BasisBoneRotationCompression::BONE_RANGE_A[slot];
            let range_b = BasisBoneRotationCompression::BONE_RANGE_B[slot];

            let bits_a = if dof == 1 { BasisBoneRotationCompression::single_axis_bits(quality) } else { BasisBoneRotationCompression::hinge_bits(quality) };
            let step_a = 2.0 * range_a / ((1u32 << bits_a) - 1) as f32;
            let step_b = if dof == 2 { 2.0 * range_b / ((1u32 << BasisBoneRotationCompression::twist_bits(quality)) - 1) as f32 } else { 0.0 };
            // Half a step per angle, plus float slack.
            let bound = 0.5 * (step_a + step_b) + 1e-3;

            let mut rng = TestRng::new((slot * 31 + quality as usize) as u64);
            for _ in 0..200 {
                let a = (rng.next_f64() as f32 * 2.0 - 1.0) * range_a * 0.98;
                let b = if dof == 2 { (rng.next_f64() as f32 * 2.0 - 1.0) * range_b * 0.98 } else { 0.0 };
                let q = if dof == 2 { mul(axis_angle(axis_a, a), axis_angle(axis_b, b)) } else { axis_angle(axis_a, a) };

                let packed = BasisBoneRotationCompression::encode_restricted(q.0, q.1, q.2, q.3, slot, quality);
                assert!(packed < 1u64 << BasisBoneRotationCompression::bone_field_width(quality, slot));

                let (dx, dy, dz, dw) = BasisBoneRotationCompression::decode_restricted(packed, slot, quality);
                let err = angle_between(q, (dx, dy, dz, dw));
                assert!(err <= bound, "slot {slot} {quality:?}: error {err} > bound {bound} (a={a}, b={b})");
            }
        }
    }
}

#[test]
fn off_axis_content_is_projected_away() {
    // A rotation with content on a joint's impossible axis decodes to the nearest representable
    // two-axis rotation — bounded by the off-axis contamination, never garbage.
    let slot = 9; // left lower arm: hinge Y, twist X, dropped Z
    let quality = BitQuality::High;
    let pure = mul(axis_angle(1, 1.1), axis_angle(0, 0.6));
    let contaminated = mul(pure, axis_angle(2, 0.1)); // ~5.7° of impossible motion

    let packed = BasisBoneRotationCompression::encode_restricted(contaminated.0, contaminated.1, contaminated.2, contaminated.3, slot, quality);
    let (dx, dy, dz, dw) = BasisBoneRotationCompression::decode_restricted(packed, slot, quality);
    // Decoded pose stays close to the anatomically-possible part of the input.
    assert!(angle_between(pure, (dx, dy, dz, dw)) < 0.12);
}

#[test]
fn non_finite_input_encodes_to_midpoint_not_garbage() {
    for quality in BitQuality::ALL {
        let packed = BasisBoneRotationCompression::encode_restricted(f32::NAN, f32::NAN, f32::NAN, f32::NAN, 19, quality);
        assert!(packed < 1u64 << BasisBoneRotationCompression::bone_field_width(quality, 19));
        let (dx, dy, dz, dw) = BasisBoneRotationCompression::decode_restricted(packed, 19, quality);
        assert!(dx.is_finite() && dy.is_finite() && dz.is_finite() && dw.is_finite());
    }
}
