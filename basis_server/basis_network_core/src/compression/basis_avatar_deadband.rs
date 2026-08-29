/// Sub-quantization deadband for the local avatar uplink. Byte-identical suppression
/// ([`super::BasisAvatarIdleSuppression`]) almost never fires in VR because sensor noise crosses
/// quantization steps every frame; these primitives let the sender compare RAW (pre-quantization)
/// values against the values of the last frame actually sent and drop the frame when every field
/// moved less than a visibility threshold.
pub struct BasisAvatarDeadband;

impl BasisAvatarDeadband {
    /// Bone-rotation deadband. ~2.5× the 12-bit High quantization step (0.04°).
    pub const BONE_ANGLE_DEGREES: f32 = 0.10;
    /// Body/hips orientation deadband — long lever arm, kept tighter.
    pub const ROOT_ANGLE_DEGREES: f32 = 0.05;
    /// Hips world-position deadband per axis (metres).
    pub const POSITION_METERS: f32 = 0.002;
    /// Hips local-delta deadband per axis (metres); wire step is ~30 µm.
    pub const HIPS_DELTA_METERS: f32 = 0.0015;
    /// End-effector target position deadband per axis (metres).
    pub const EFFECTOR_POSITION_METERS: f32 = 0.002;
    /// Avatar scale deadband (posit16 wire; scale is normally bit-stable).
    pub const SCALE_UNITS: f32 = 1e-4;
    /// Finger curl/splay deadband in [-1, 1] units.
    pub const FINGER_PERCENT_UNITS: f32 = 0.008;

    /// |dot| threshold equivalent to an angle deadband: cos(angle/2).
    pub fn min_abs_dot_for_angle_degrees(degrees: f32) -> f64 {
        (f64::from(degrees) * (std::f64::consts::PI / 180.0) * 0.5).cos()
    }

    /// True when every quaternion pair (flattened xyzw, length = multiple of 4) is within the
    /// angle expressed by `min_abs_dot`. Sign-insensitive (double cover).
    pub fn quats_within(current: &[f32], last_sent: &[f32], min_abs_dot: f64) -> bool {
        if current.len() != last_sent.len() || (current.len() & 3) != 0 {
            return false;
        }
        let mut i = 0;
        while i < current.len() {
            let dot = f64::from(current[i]) * f64::from(last_sent[i])
                + f64::from(current[i + 1]) * f64::from(last_sent[i + 1])
                + f64::from(current[i + 2]) * f64::from(last_sent[i + 2])
                + f64::from(current[i + 3]) * f64::from(last_sent[i + 3]);
            // !(>=) rather than (<) so a NaN dot returns false (forces send), matching values_within.
            if !(dot.abs() >= min_abs_dot) {
                return false;
            }
            i += 4;
        }
        true
    }

    /// True when every scalar delta |current − last_sent| is at most `max_abs_delta`.
    /// Non-finite values never pass (a NaN must not wedge the suppressor).
    pub fn values_within(current: &[f32], last_sent: &[f32], max_abs_delta: f32) -> bool {
        if current.len() != last_sent.len() {
            return false;
        }
        for i in 0..current.len() {
            let d = current[i] - last_sent[i];
            if !(d <= max_abs_delta && d >= -max_abs_delta) {
                return false;
            }
        }
        true
    }
}
