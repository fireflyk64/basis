//! Sender-side deadband primitives: raw-value comparisons that let the uplink drop frames whose
//! every field moved less than a sub-visibility threshold since the last frame actually sent.

use basis_network_core::compression::BasisAvatarDeadband;

fn yaw_quat(degrees: f64) -> [f32; 4] {
    let half = degrees * std::f64::consts::PI / 180.0 * 0.5;
    [0.0, half.sin() as f32, 0.0, half.cos() as f32]
}

#[test]
fn quats_within_passes_under_threshold_fails_over() {
    let min_dot = BasisAvatarDeadband::min_abs_dot_for_angle_degrees(0.10);
    let base = yaw_quat(0.0);
    assert!(BasisAvatarDeadband::quats_within(&base, &yaw_quat(0.0), min_dot), "identical");
    assert!(BasisAvatarDeadband::quats_within(&base, &yaw_quat(0.05), min_dot), "under threshold");
    assert!(BasisAvatarDeadband::quats_within(&base, &yaw_quat(0.099), min_dot), "just under");
    assert!(!BasisAvatarDeadband::quats_within(&base, &yaw_quat(0.12), min_dot), "over threshold");
    assert!(!BasisAvatarDeadband::quats_within(&base, &yaw_quat(5.0), min_dot), "far over");
}

#[test]
fn quats_within_is_double_cover_insensitive() {
    let min_dot = BasisAvatarDeadband::min_abs_dot_for_angle_degrees(0.10);
    let q = yaw_quat(30.0);
    let negated = [-q[0], -q[1], -q[2], -q[3]];
    assert!(BasisAvatarDeadband::quats_within(&q, &negated, min_dot), "-q represents the same rotation");
}

#[test]
fn quats_within_checks_every_quat_in_the_span() {
    let min_dot = BasisAvatarDeadband::min_abs_dot_for_angle_degrees(0.10);
    let mut a = [0f32; 8];
    let mut b = [0f32; 8];
    a[..4].copy_from_slice(&yaw_quat(0.0));
    b[..4].copy_from_slice(&yaw_quat(0.0));
    a[4..].copy_from_slice(&yaw_quat(0.0));
    b[4..].copy_from_slice(&yaw_quat(1.0));
    assert!(!BasisAvatarDeadband::quats_within(&a, &b, min_dot), "second quat differs");
    assert!(!BasisAvatarDeadband::quats_within(&a[..7], &b[..7], min_dot), "non-multiple-of-4 rejected");
}

#[test]
fn tight_root_threshold_is_resolvable_in_double() {
    // 0.05° → cos(0.025°) sits within float-epsilon of 1.0; the double path must still separate
    // under from over.
    let min_dot = BasisAvatarDeadband::min_abs_dot_for_angle_degrees(BasisAvatarDeadband::ROOT_ANGLE_DEGREES);
    assert!(BasisAvatarDeadband::quats_within(&yaw_quat(0.0), &yaw_quat(0.03), min_dot));
    assert!(!BasisAvatarDeadband::quats_within(&yaw_quat(0.0), &yaw_quat(0.08), min_dot));
}

#[test]
fn values_within_bounds_absolute_delta_and_rejects_non_finite() {
    let a = [1.0f32, -2.0, 0.5];
    assert!(BasisAvatarDeadband::values_within(&a, &[1.0015, -2.0015, 0.5], 0.002));
    assert!(!BasisAvatarDeadband::values_within(&a, &[1.0, -2.0, 0.503], 0.002));
    assert!(!BasisAvatarDeadband::values_within(&a, &[1.0, f32::NAN, 0.5], 0.002), "NaN must not wedge suppression");
    assert!(!BasisAvatarDeadband::values_within(&a, &[1.0, -2.0], 0.002), "length mismatch");
}

#[test]
fn drift_against_fixed_baseline_eventually_exceeds() {
    // Compare-vs-last-SENT semantics: tiny per-frame drift accumulates against the fixed baseline
    // until it crosses the threshold and forces a send. 0.03°/frame crosses 0.1° on the 4th frame.
    let min_dot = BasisAvatarDeadband::min_abs_dot_for_angle_degrees(BasisAvatarDeadband::BONE_ANGLE_DEGREES);
    let baseline = yaw_quat(0.0);
    let mut sent_at = -1;
    for frame in 1..=6 {
        if !BasisAvatarDeadband::quats_within(&baseline, &yaw_quat(0.03 * frame as f64), min_dot) {
            sent_at = frame;
            break;
        }
    }
    assert_eq!(sent_at, 4);
}
