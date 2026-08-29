//! Regression: `quats_within` must reject a NaN dot (force a send), matching `values_within`.
//! Before the fix a NaN rotation component with finite positions passed the deadband, so a
//! glitched frame could be suppressed and the receiver would hold a stale pose.

use basis_network_core::compression::BasisAvatarDeadband;

fn dot() -> f64 {
    BasisAvatarDeadband::min_abs_dot_for_angle_degrees(BasisAvatarDeadband::BONE_ANGLE_DEGREES)
}

#[test]
fn quats_within_nan_component_forces_send() {
    assert!(!BasisAvatarDeadband::quats_within(&[0.0, 0.0, 0.0, f32::NAN], &[0.0, 0.0, 0.0, 1.0], dot()));
}

#[test]
fn quats_within_nan_in_second_quat_of_pair_forces_send() {
    let cur = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, f32::NAN];
    let last = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    assert!(!BasisAvatarDeadband::quats_within(&cur, &last, dot()));
}

#[test]
fn quats_within_identical_quat_within() {
    let q = [0.0, 0.0, 0.0, 1.0];
    assert!(BasisAvatarDeadband::quats_within(&q, &q.clone(), dot()));
}

#[test]
fn quats_within_large_angle_not_within() {
    // 90° about Z: (0,0,sin45,cos45); |dot| with identity ≈ 0.707, well below the sub-degree threshold.
    let s = std::f32::consts::FRAC_1_SQRT_2;
    assert!(!BasisAvatarDeadband::quats_within(&[0.0, 0.0, s, s], &[0.0, 0.0, 0.0, 1.0], dot()));
}

#[test]
fn both_predicates_reject_nan() {
    assert!(!BasisAvatarDeadband::values_within(&[f32::NAN], &[0.0], 0.01));
    assert!(!BasisAvatarDeadband::quats_within(&[f32::NAN, 0.0, 0.0, 1.0], &[0.0, 0.0, 0.0, 1.0], dot()));
}
