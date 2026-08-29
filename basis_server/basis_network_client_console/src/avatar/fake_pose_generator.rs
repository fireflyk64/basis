//! Port of `FakePoseGenerator.cs`: realistic human-like avatar pose data for fake clients — a
//! natural standing pose with subtle idle animation, encoded with the same smallest-three /
//! restricted-axis compression as the real client.

use std::sync::LazyLock;

use basis_network_core::compression::{BasisBoneRotationCompression, BitQuality};

const DEG2RAD: f32 = std::f32::consts::PI / 180.0;
const TWO_PI: f32 = std::f32::consts::PI * 2.0;
const INV_SQRT2: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Slots the wire still carries as explicit rotations. Since v47 the thirty finger joints are
/// ten curl/splay channels, written by `write_finger_channels`.
const WIRE_BONE_COUNT: usize = BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT; // 21
const FINGER_COUNT: usize = BasisBoneRotationCompression::FINGER_CHANNEL_COUNT; // 10

// Base natural standing pose: one quaternion per wire bone slot, flat float array
// [slot * 4 + 0..4] = x, y, z, w. T-pose-relative deltas: identity means T-pose.
static BASE_POSE: LazyLock<[f32; WIRE_BONE_COUNT * 4]> = LazyLock::new(build_natural_standing_pose);

// Rotation-field bit offsets per quality, built once — a load-test sender runs this per client
// per send, so it must not allocate.
static FIELD_OFFSETS_BY_QUALITY: LazyLock<[Vec<usize>; 4]> = LazyLock::new(|| {
    std::array::from_fn(|q| {
        let mut offsets = vec![0usize; BasisBoneRotationCompression::ROTATION_FIELD_COUNT];
        let quality = BitQuality::from_byte(q as u8).unwrap_or(BitQuality::High);
        let _ = BasisBoneRotationCompression::build_rotation_field_offsets(quality, &mut offsets);
        offsets
    })
});

fn field_offsets(quality: BitQuality) -> &'static [usize] {
    &FIELD_OFFSETS_BY_QUALITY[quality as usize]
}

// BONE_WRITE_ORDER slot assignments:
//  0:Spine 1:Chest 2:UpperChest 3:Neck 4:Head 5:LUpperArm 6:RUpperArm 7:LUpperLeg 8:RUpperLeg
//  9:LLowerArm 10:RLowerArm 11:LLowerLeg 12:RLowerLeg 13:LShoulder 14:RShoulder 15:LHand
//  16:RHand 17:LFoot 18:RFoot 19:LToes 20:RToes
fn build_natural_standing_pose() -> [f32; WIRE_BONE_COUNT * 4] {
    let mut pose = [0f32; WIRE_BONE_COUNT * 4];
    // Initialize every wire bone slot to identity (T-pose)
    for slot in 0..WIRE_BONE_COUNT {
        set_quat(&mut pose, slot, 0.0, 0.0, 0.0, 1.0);
    }
    // Spine chain: natural S-curve
    set_axis_angle(&mut pose, 0, 1.0, 0.0, 0.0, 5.0); // Spine: slight forward lean
    set_axis_angle(&mut pose, 1, 1.0, 0.0, 0.0, -3.0); // Chest: slight extension (compensate)
    set_axis_angle(&mut pose, 2, 1.0, 0.0, 0.0, 2.0); // UpperChest: slight forward
    set_axis_angle(&mut pose, 3, 1.0, 0.0, 0.0, 8.0); // Neck: forward tilt
    set_axis_angle(&mut pose, 4, 1.0, 0.0, 0.0, -3.0); // Head: slight back (eyes level)
    // Upper arms: down from T-pose. -Z rotation swings the arm downward in the local T-pose frame.
    set_axis_angle(&mut pose, 5, 0.0, 0.0, 1.0, -72.0);
    set_axis_angle(&mut pose, 6, 0.0, 0.0, 1.0, -72.0);
    // Upper legs: standing straight with tiny forward tilt
    set_axis_angle(&mut pose, 7, 1.0, 0.0, 0.0, 2.0);
    set_axis_angle(&mut pose, 8, 1.0, 0.0, 0.0, 2.0);
    // Lower arms: slight elbow bend
    set_axis_angle(&mut pose, 9, 0.0, 1.0, 0.0, 20.0);
    set_axis_angle(&mut pose, 10, 0.0, 1.0, 0.0, -20.0);
    // Lower legs: very slight knee bend
    set_axis_angle(&mut pose, 11, 1.0, 0.0, 0.0, 5.0);
    set_axis_angle(&mut pose, 12, 1.0, 0.0, 0.0, 5.0);
    // Shoulders: slight depression
    set_axis_angle(&mut pose, 13, 0.0, 0.0, 1.0, -3.0);
    set_axis_angle(&mut pose, 14, 0.0, 0.0, 1.0, 3.0);
    // Hands: slight natural wrist angle
    set_axis_angle(&mut pose, 15, 0.0, 0.0, 1.0, 5.0);
    set_axis_angle(&mut pose, 16, 0.0, 0.0, 1.0, -5.0);
    // Feet: slight dorsiflexion for standing
    set_axis_angle(&mut pose, 17, 1.0, 0.0, 0.0, -8.0);
    set_axis_angle(&mut pose, 18, 1.0, 0.0, 0.0, -8.0);
    // Toes: flat on ground (identity). Fingers: see write_finger_channels.
    pose
}

fn set_quat(pose: &mut [f32], slot: usize, x: f32, y: f32, z: f32, w: f32) {
    let idx = slot * 4;
    pose[idx] = x;
    pose[idx + 1] = y;
    pose[idx + 2] = z;
    pose[idx + 3] = w;
}

fn set_axis_angle(pose: &mut [f32], slot: usize, ax: f32, ay: f32, az: f32, degrees: f32) {
    let (qx, qy, qz, qw) = axis_angle_to_quat(ax, ay, az, degrees);
    set_quat(pose, slot, qx, qy, qz, qw);
}

fn axis_angle_to_quat(mut ax: f32, mut ay: f32, mut az: f32, degrees: f32) -> (f32, f32, f32, f32) {
    let half = degrees * DEG2RAD * 0.5;
    let s = half.sin();
    let c = half.cos();
    let len = (ax * ax + ay * ay + az * az).sqrt();
    if len > 0.0001 {
        let inv = 1.0 / len;
        ax *= inv;
        ay *= inv;
        az *= inv;
    }
    (ax * s, ay * s, az * s, c)
}

/// Hamilton product: result = a * b
#[allow(clippy::too_many_arguments)]
fn quat_mul(ax: f32, ay: f32, az: f32, aw: f32, bx: f32, by: f32, bz: f32, bw: f32) -> (f32, f32, f32, f32) {
    let rw = aw * bw - ax * bx - ay * by - az * bz;
    let rx = aw * bx + ax * bw + ay * bz - az * by;
    let ry = aw * by - ax * bz + ay * bw + az * bx;
    let rz = aw * bz + ax * by - ay * bx + az * bw;
    (rx, ry, rz, rw)
}

fn normalize(x: f32, y: f32, z: f32, w: f32) -> (f32, f32, f32, f32) {
    let len = (x * x + y * y + z * z + w * w).sqrt();
    if len > 1e-8 {
        let inv = 1.0 / len;
        (x * inv, y * inv, z * inv, w * inv)
    } else {
        (0.0, 0.0, 0.0, 1.0)
    }
}

fn quantize_small(mut v: f32) -> u16 {
    v = v.clamp(-INV_SQRT2, INV_SQRT2);
    let t = (v + INV_SQRT2) / (2.0 * INV_SQRT2);
    ((t * 65535.0).round() as i32).clamp(0, 65535) as u16
}

pub struct FakePoseGenerator;

impl FakePoseGenerator {
    /// Writes the whole rotation region — the explicit bone rotations (base pose + idle
    /// animation, smallest-three) followed by the ten finger curl/splay channels. Clears the
    /// region before writing, since write_bits ORs into bytes.
    pub fn write_bone_rotations(dst: &mut [u8], byte_offset: usize, quality: BitQuality, time_sec: f64, phase: f32) {
        let bpc = BasisBoneRotationCompression::get_bpc_table(quality);
        let ranges = &BasisBoneRotationCompression::MAX_COMPONENT;

        // Clear the rotation region (write_bits ORs into bytes, so must start clean)
        let rot_bytes = BasisBoneRotationCompression::rotation_bytes(quality);
        let Some(region) = dst.get_mut(byte_offset..byte_offset + rot_bytes) else {
            return;
        };
        region.fill(0);

        // Field starts come from the codec rather than a running counter, so this generator
        // cannot drift out of step with the wire layout.
        let offsets = field_offsets(quality);
        let base_bit = byte_offset << 3;

        for slot in 0..WIRE_BONE_COUNT {
            // Every slot animates every frame — a load-test sender must produce fresh rotation
            // bits per send like a real tracked human, not a frozen statue.
            let idx = slot * 4;
            let (bx, by, bz, bw) = (BASE_POSE[idx], BASE_POSE[idx + 1], BASE_POSE[idx + 2], BASE_POSE[idx + 3]);
            let (dx, dy, dz, dw) = Self::get_idle_delta(slot, time_sec, phase);
            let (rx, ry, rz, rw) = quat_mul(bx, by, bz, bw, dx, dy, dz, dw);
            let (rx, ry, rz, rw) = normalize(rx, ry, rz, rw);

            let total_bits = BasisBoneRotationCompression::bone_field_width(quality, slot);
            let packed = if BasisBoneRotationCompression::BONE_DOF[slot] == 3 {
                BasisBoneRotationCompression::encode_smallest_three(rx, ry, rz, rw, bpc[slot] as u32, ranges[slot])
            } else {
                BasisBoneRotationCompression::encode_restricted(rx, ry, rz, rw, slot, quality)
            };
            BasisBoneRotationCompression::write_bits(dst, base_bit + offsets[slot], packed, total_bits);
        }

        Self::write_finger_channels(dst, base_bit, offsets, quality, time_sec, phase);
    }

    /// Writes the ten finger channels: one curl and one splay scalar per finger in [-1, 1],
    /// ordered L thumb→little then R thumb→little, packed [curl][splay] exactly as the real
    /// client does. Amplitudes and rates are sized so both scalars cross their quantization step
    /// every send at the ~11 Hz load-test cadence.
    fn write_finger_channels(dst: &mut [u8], base_bit: usize, offsets: &[usize], quality: BitQuality, time_sec: f64, phase: f32) {
        let curl_bits = BasisBoneRotationCompression::curl_bits(quality);
        let splay_bits = BasisBoneRotationCompression::splay_bits(quality);
        for finger in 0..FINGER_COUNT {
            // Per-finger phase spread so a hand ripples rather than clenching as one block.
            let fp = phase * 1.1 + finger as f32 * 0.73;
            // Relaxed hand sits partly curled; grip slowly tightens and releases.
            let curl = 0.30 + 0.35 * ((time_sec * 0.50 * TWO_PI as f64) as f32 + fp).sin();
            let splay = 0.25 * ((time_sec * 0.37 * TWO_PI as f64) as f32 + fp * 1.4).sin();
            let q_curl = BasisBoneRotationCompression::encode_signed_unit(curl, curl_bits) as u64;
            let q_splay = BasisBoneRotationCompression::encode_signed_unit(splay, splay_bits) as u64;
            let field = WIRE_BONE_COUNT + finger;
            BasisBoneRotationCompression::write_bits(dst, base_bit + offsets[field], q_curl | (q_splay << curl_bits), curl_bits + splay_bits);
        }
    }

    /// Writes an animated hips rotation into the 7-byte tail of the packet:
    /// [largest index:1][a:2][b:2][c:2], each component quantized from ±InvSqrt2 to 0..65535.
    pub fn write_compressed_hips_rotation(dst: &mut [u8], offset: usize, time_sec: f64, phase: f32) {
        // Subtle body yaw sway + slight lateral tilt
        let yaw = 3.0 * ((time_sec * 0.06 * TWO_PI as f64) as f32 + phase * 1.7).sin();
        let tilt = 1.0 * ((time_sec * 0.04 * TWO_PI as f64) as f32 + phase * 2.3).sin();
        let (yx, yy, yz, yw) = axis_angle_to_quat(0.0, 1.0, 0.0, yaw);
        let (tx, ty, tz, tw) = axis_angle_to_quat(0.0, 0.0, 1.0, tilt);
        let (qx, qy, qz, qw) = quat_mul(yx, yy, yz, yw, tx, ty, tz, tw);
        let (qx, qy, qz, qw) = normalize(qx, qy, qz, qw);
        Self::write_compressed_quat(dst, offset, qx, qy, qz, qw);
    }

    fn write_compressed_quat(dst: &mut [u8], offset: usize, mut qx: f32, mut qy: f32, mut qz: f32, mut qw: f32) {
        if dst.len() < offset + 7 {
            return;
        }
        // Find largest absolute component
        let (ax, ay, az, aw) = (qx.abs(), qy.abs(), qz.abs(), qw.abs());
        let mut largest = 0;
        let mut max = ax;
        if ay > max {
            largest = 1;
            max = ay;
        }
        if az > max {
            largest = 2;
            max = az;
        }
        if aw > max {
            largest = 3;
        }
        // Ensure largest component is positive (double-cover equivalence)
        let sign = match largest {
            0 => qx,
            1 => qy,
            2 => qz,
            _ => qw,
        };
        if sign < 0.0 {
            qx = -qx;
            qy = -qy;
            qz = -qz;
            qw = -qw;
        }
        // Extract three smallest components
        let (a, b, c) = match largest {
            0 => (qy, qz, qw),
            1 => (qx, qz, qw),
            2 => (qx, qy, qw),
            _ => (qx, qy, qz),
        };
        let (qa, qb, qc) = (quantize_small(a), quantize_small(b), quantize_small(c));
        dst[offset] = largest as u8;
        dst[offset + 1] = qa as u8;
        dst[offset + 2] = (qa >> 8) as u8;
        dst[offset + 3] = qb as u8;
        dst[offset + 4] = (qb >> 8) as u8;
        dst[offset + 5] = qc as u8;
        dst[offset + 6] = (qc >> 8) as u8;
    }

    /// Each animated bone gets a small time-varying delta quaternion layered on top of the base
    /// pose. Frequencies are sub-1 Hz to produce slow, natural-looking motion at the 11 Hz send
    /// rate: breathing on spine/chest, slow gaze drift, arm sway, weight shift.
    fn get_idle_delta(slot: usize, t: f64, phase: f32) -> (f32, f32, f32, f32) {
        let p = phase;
        let s = |freq: f64, phase_mul: f32| -> f32 { ((t * freq * TWO_PI as f64) as f32 + p * phase_mul).sin() };
        match slot {
            0 => axis_angle_to_quat(1.0, 0.0, 0.0, 1.5 * s(0.25, 1.0)), // Spine — breathing
            1 => axis_angle_to_quat(1.0, 0.0, 0.0, 1.0 * s(0.25, 1.0)), // Chest — breathing
            3 => {
                // Neck — slow gaze drift (yaw + pitch combined)
                let yaw = 3.0 * s(0.08, 1.3);
                let pitch = 1.5 * s(0.12, 0.7);
                let (yx, yy, yz, yw) = axis_angle_to_quat(0.0, 1.0, 0.0, yaw);
                let (px, py, pz, pw) = axis_angle_to_quat(1.0, 0.0, 0.0, pitch);
                quat_mul(yx, yy, yz, yw, px, py, pz, pw)
            }
            4 => axis_angle_to_quat(1.0, 0.0, 0.0, 1.0 * s(0.15, 2.1)), // Head — micro-nod
            5 => axis_angle_to_quat(1.0, 0.0, 0.0, 2.0 * s(0.1, 1.0)),  // Left upper arm — sway
            6 => axis_angle_to_quat(1.0, 0.0, 0.0, 2.0 * ((t * 0.1 * TWO_PI as f64) as f32 + p + std::f32::consts::PI).sin()), // Right upper arm — out of phase
            7 => axis_angle_to_quat(0.0, 0.0, 1.0, 1.0 * s(0.05, 1.0)), // Left upper leg — weight shift
            8 => axis_angle_to_quat(0.0, 0.0, 1.0, -s(0.05, 1.0)), // Right upper leg — opposite
            _ => {
                // Every remaining slot oscillates continuously, with amplitude × frequency sized
                // so the per-send angular step crosses that bone group's quantization step at the
                // ~11 Hz send rate (coarser BPC ⇒ bigger, faster motion).
                let (amplitude, mut frequency) = if slot >= 19 { (16.0f32, 0.50f32) } else { (2.0f32, 0.30 + 0.05 * (slot % 5) as f32) };
                // Slot-seeded frequency jitter + phase spread so bones (and players) desync.
                frequency *= 1.0 + 0.07 * (slot % 3) as f32;
                let angle = amplitude * ((t * frequency as f64 * TWO_PI as f64) as f32 + p * 1.1 + slot as f32 * 0.61).sin();
                // Restricted slots only carry their anatomical axes on the wire, so animate the
                // hinge axis — motion on a dropped axis would encode to silence.
                let axis_code = if BasisBoneRotationCompression::BONE_DOF[slot] == 3 { (slot % 3) as u8 } else { BasisBoneRotationCompression::BONE_AXIS_A[slot] };
                match axis_code {
                    0 => axis_angle_to_quat(1.0, 0.0, 0.0, angle),
                    1 => axis_angle_to_quat(0.0, 1.0, 0.0, angle),
                    _ => axis_angle_to_quat(0.0, 0.0, 1.0, angle),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use basis_network_core::compression::BasisAvatarBitPacking;

    #[test]
    fn rotation_region_changes_between_sends_and_stays_in_bounds() {
        let size = BasisAvatarBitPacking::convert_to_size(BitQuality::High);
        let mut a = vec![0u8; size];
        let mut b = vec![0u8; size];
        FakePoseGenerator::write_bone_rotations(&mut a, BasisAvatarBitPacking::WRITE_POSITION, BitQuality::High, 0.0, 0.3);
        FakePoseGenerator::write_bone_rotations(&mut b, BasisAvatarBitPacking::WRITE_POSITION, BitQuality::High, 0.09, 0.3);
        assert_ne!(a, b, "idle animation must produce fresh bits per send");
        let rot_end = BasisAvatarBitPacking::WRITE_POSITION + BasisBoneRotationCompression::rotation_bytes(BitQuality::High);
        assert!(a[rot_end..].iter().all(|&x| x == 0), "nothing may be written past the rotation region");
        assert!(a[..BasisAvatarBitPacking::WRITE_POSITION].iter().all(|&x| x == 0));
    }

    #[test]
    fn hips_rotation_is_a_valid_smallest_three() {
        let mut dst = vec![0u8; 7];
        FakePoseGenerator::write_compressed_hips_rotation(&mut dst, 0, 1.5, 0.7);
        assert!(dst[0] <= 3);
        let mut short = vec![0u8; 3];
        FakePoseGenerator::write_compressed_hips_rotation(&mut short, 0, 1.5, 0.7); // must not panic
        assert_eq!(short, vec![0u8; 3]);
    }
}
