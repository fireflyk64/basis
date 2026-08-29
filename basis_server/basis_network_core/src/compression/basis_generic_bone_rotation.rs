use super::basis_bone_rotation_compression::BasisBoneRotationCompression;

/// Minimal unit-quaternion POD in (x, y, z, w) order — the same component order as
/// UnityEngine.Quaternion and Unity.Mathematics.quaternion.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quat {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Quat {
    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }

    pub const IDENTITY: Quat = Quat::new(0.0, 0.0, 0.0, 1.0);

    pub fn identity() -> Quat {
        Self::IDENTITY
    }
}

impl std::fmt::Display for Quat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({:.6}, {:.6}, {:.6}, {:.6})", self.x, self.y, self.z, self.w)
    }
}

/// Rig-neutral ("generic") bone rotation space — the wire representation for humanoid pose.
///
/// A bone's T-pose-relative local delta `d = conj(T) * C` is expressed in the bone's own rest
/// frame and so is only meaningful to the rig that produced it. Conjugating by `F` (the bone's
/// rest orientation relative to the avatar root) re-expresses it in root axes:
///
///     g = F * d * conj(F)      [ENCODE]
///     d = conj(F) * g * F      [DECODE]
///
/// so two avatars with different local bone axes produce the same `g` for the same visible bend.
/// Conjugation is an isometry of SO(3), so every quantisation table sized to a joint's own range
/// of motion stays valid. `F == identity` collapses to the legacy scheme exactly.
pub struct BasisGenericBoneRotation;

impl BasisGenericBoneRotation {
    /// Hamilton product, matching Unity's a*b convention (apply b, then a).
    #[inline]
    pub fn mul(a: &Quat, b: &Quat) -> Quat {
        Quat::new(
            a.w * b.x + a.x * b.w + a.y * b.z - a.z * b.y,
            a.w * b.y - a.x * b.z + a.y * b.w + a.z * b.x,
            a.w * b.z + a.x * b.y - a.y * b.x + a.z * b.w,
            a.w * b.w - a.x * b.x - a.y * b.y - a.z * b.z,
        )
    }

    /// Inverse of a UNIT quaternion. Every rotation here is unit-length by construction.
    #[inline]
    pub fn conjugate(q: &Quat) -> Quat {
        Quat::new(-q.x, -q.y, -q.z, q.w)
    }

    /// Renormalises, falling back to identity on a degenerate input.
    #[inline]
    pub fn normalize(q: &Quat) -> Quat {
        let len_sq = q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w;
        if len_sq < 1e-12 {
            return Quat::IDENTITY;
        }
        let inv = 1.0 / (f64::from(len_sq).sqrt() as f32);
        Quat::new(q.x * inv, q.y * inv, q.z * inv, q.w * inv)
    }

    /// Rig-local delta → generic. `rest_frame` is F, the bone's rest rotation relative to the avatar root.
    #[inline]
    pub fn local_delta_to_generic(rest_frame: &Quat, local_delta: &Quat) -> Quat {
        Self::mul(&Self::mul(rest_frame, local_delta), &Self::conjugate(rest_frame))
    }

    /// Generic → rig-local delta, using THIS rig's rest frame.
    #[inline]
    pub fn generic_to_local_delta(rest_frame: &Quat, generic: &Quat) -> Quat {
        Self::mul(&Self::mul(&Self::conjugate(rest_frame), generic), rest_frame)
    }

    /// Current local rotation → generic, in one step.
    pub fn to_generic(rest_frame: &Quat, tpose_local: &Quat, current_local: &Quat) -> Quat {
        Self::local_delta_to_generic(rest_frame, &Self::mul(&Self::conjugate(tpose_local), current_local))
    }

    /// Generic → current local rotation for a rig described by (rest_frame, tpose_local).
    pub fn from_generic(rest_frame: &Quat, tpose_local: &Quat, generic: &Quat) -> Quat {
        Self::mul(tpose_local, &Self::generic_to_local_delta(rest_frame, generic))
    }

    /// Precomputes the pair that turns a live local rotation into the generic wire value:
    /// `g = pre * current_local * post`. Returns `(pre, post)`.
    pub fn build_encode_operators(rest_frame: &Quat, tpose_local: &Quat) -> (Quat, Quat) {
        let f = Self::normalize(rest_frame);
        let t = Self::normalize(tpose_local);
        (Self::mul(&f, &Self::conjugate(&t)), Self::conjugate(&f))
    }

    /// Precomputes the pair that turns a generic wire value into THIS rig's local rotation:
    /// `current_local = pre * g * post`. Returns `(pre, post)`.
    pub fn build_decode_operators(rest_frame: &Quat, tpose_local: &Quat) -> (Quat, Quat) {
        let f = Self::normalize(rest_frame);
        let t = Self::normalize(tpose_local);
        (Self::mul(&t, &Self::conjugate(&f)), f)
    }

    /// Applies a folded operator pair. Same expression on both ends.
    #[inline]
    pub fn apply(pre: &Quat, middle: &Quat, post: &Quat) -> Quat {
        Self::mul(&Self::mul(pre, middle), post)
    }

    /// Builds per-slot encode operators in `BONE_WRITE_ORDER` order. Inputs are indexed by
    /// HumanBodyBones enum value (length >= 55); the outputs by wire slot (length SYNC_BONE_COUNT).
    pub fn build_encode_operator_table(rest_frame_by_bone: &[Quat], tpose_local_by_bone: &[Quat], out_pre: &mut [Quat], out_post: &mut [Quat]) {
        let order = &BasisBoneRotationCompression::BONE_WRITE_ORDER;
        for slot in 0..BasisBoneRotationCompression::SYNC_BONE_COUNT {
            let bone = order[slot] as usize;
            let (pre, post) = Self::build_encode_operators(&rest_frame_by_bone[bone], &tpose_local_by_bone[bone]);
            out_pre[slot] = pre;
            out_post[slot] = post;
        }
    }

    /// Slot-order decode operators; mirror of [`Self::build_encode_operator_table`].
    pub fn build_decode_operator_table(rest_frame_by_bone: &[Quat], tpose_local_by_bone: &[Quat], out_pre: &mut [Quat], out_post: &mut [Quat]) {
        let order = &BasisBoneRotationCompression::BONE_WRITE_ORDER;
        for slot in 0..BasisBoneRotationCompression::SYNC_BONE_COUNT {
            let bone = order[slot] as usize;
            let (pre, post) = Self::build_decode_operators(&rest_frame_by_bone[bone], &tpose_local_by_bone[bone]);
            out_pre[slot] = pre;
            out_post[slot] = post;
        }
    }
}
