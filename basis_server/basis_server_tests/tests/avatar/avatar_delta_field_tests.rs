//! Per-field dirty-mask coverage: changing exactly one field must send that field and nothing
//! else, and every field / combination must round-trip byte-exactly. Sizes are asserted as
//! UPPER BOUNDS: under residual coding a field's cost depends on how far it moved, and "flip every
//! bit" is bounded by the field's verbatim width plus its mode bit.

use std::collections::HashSet;

use basis_network_core::compression::{BasisAvatarDeltaCompression, BasisBoneRotationCompression, BitQuality};
use basis_server_tests::support::DeltaTestSupport as S;
use basis_server_tests::support::delta_test_support::TestRng;

const MASK: usize = BasisAvatarDeltaCompression::DIRTY_MASK_BYTES; // 5

/// Mask + a whole-field worst case: its channels verbatim plus the one mode bit.
fn max_body_for(q: BitQuality, fields: &[usize]) -> usize {
    let layout = S::layout(q);
    let bits: usize = fields.iter().map(|&f| layout.field_raw_bits(f) + 1).sum();
    MASK + ((bits + 7) >> 3)
}

const FIELD_POSITION: usize = 0;
const FIELD_SCALE: usize = 1 + BasisBoneRotationCompression::ROTATION_FIELD_COUNT;
const FIELD_BODY_ROT: usize = FIELD_SCALE + 1;
const FIELD_HIPS_DELTA: usize = FIELD_SCALE + 2;
const FIELD_HIPS_ROT: usize = FIELD_SCALE + 3;
const FIELD_END_EFFECTOR: usize = FIELD_SCALE + 4;

fn bone_field(slot: usize) -> usize {
    BasisAvatarDeltaCompression::BONE_FIELD_START + slot
}

#[test]
fn no_change_sends_mask_only() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(q as u64 + 1);
        let kf = S::make_realistic_payload(q, &mut rng);
        let cur = kf.clone();
        let (len, recon) = S::build_apply(&kf, &cur, q);
        assert_eq!(len, MASK);
        assert_eq!(cur, recon);
    }
}

fn assert_byte_field_only(q: BitQuality, field_offset: usize, field_index: usize) {
    let mut rng = TestRng::new((field_offset * 31 + q as usize) as u64);
    let kf = S::make_realistic_payload(q, &mut rng);
    let mut cur = kf.clone();
    cur[field_offset] ^= 0xFF; // flip one byte inside the field
    let (len, recon) = S::build_apply(&kf, &cur, q);
    assert!((MASK + 1..=max_body_for(q, &[field_index])).contains(&len), "{q:?}: {len}");
    assert_eq!(cur, recon);
}

#[test]
fn position_only() {
    for q in S::ALL_QUALITIES {
        assert_byte_field_only(q, 0, FIELD_POSITION);
    }
}

#[test]
fn scale_only() {
    for q in S::ALL_QUALITIES {
        assert_byte_field_only(q, S::scale_offset(q), FIELD_SCALE);
    }
}

#[test]
fn body_rotation_only() {
    for q in S::ALL_QUALITIES {
        assert_byte_field_only(q, S::body_rot_offset(q), FIELD_BODY_ROT);
    }
}

#[test]
fn hips_delta_only() {
    for q in S::ALL_QUALITIES {
        assert_byte_field_only(q, S::hips_delta_offset(q), FIELD_HIPS_DELTA);
    }
}

#[test]
fn hips_rotation_only() {
    for q in S::ALL_QUALITIES {
        assert_byte_field_only(q, S::hips_rot_offset(q), FIELD_HIPS_ROT);
    }
}

#[test]
fn every_single_rotation_field_all_qualities_bounded_and_round_trips() {
    let mut rng = TestRng::new(9001);
    for q in S::ALL_QUALITIES {
        for slot in 0..S::BONE_COUNT {
            let kf = S::make_realistic_payload(q, &mut rng);
            let mut cur = kf.clone();
            S::flip_bone(&mut cur, q, slot);
            let (len, recon) = S::build_apply(&kf, &cur, q);
            assert!((MASK + 1..=max_body_for(q, &[bone_field(slot)])).contains(&len), "{q:?} slot {slot}: {len}");
            assert_eq!(cur, recon);
        }
    }
}

/// A single quantization step on one component — the case the old codec charged a whole bone for
/// and the entire reason this codec exists. One step must cost dramatically less than the field.
#[test]
fn single_component_step_costs_far_less_than_the_field() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(1234 + q as u64);
        let layout = S::layout(q);
        let bpc = S::bpc(q);

        for slot in 0..S::WIRE_BONE_SLOTS {
            let kf = S::make_realistic_payload(q, &mut rng);
            let mut cur = kf.clone();

            // Nudge the first component of this bone by exactly one step.
            let field = bone_field(slot);
            let ch = layout.channels[layout.field_channel_start(field) + 1]; // [0] is the 2-bit index
            let v = BasisAvatarDeltaCompression::read_channel(&cur, &ch);
            BasisAvatarDeltaCompression::write_channel(&mut cur, &ch, (v + 1) & ch.mask());
            if BasisAvatarDeltaCompression::read_channel(&cur, &ch) == BasisAvatarDeltaCompression::read_channel(&kf, &ch) {
                continue;
            }

            let (len, recon) = S::build_apply(&kf, &cur, q);
            assert_eq!(cur, recon);

            // Mask + mode bit + 2 index bits + three EG codes, one of which is +-1 (3 bits) and two
            // of which are zero (1 bit each): 8 bits of body, so at most one byte past the mask.
            assert!(len <= MASK + 1, "{q:?} slot {slot}: one-step change cost {} body bytes, expected 1 (field is {} bits verbatim)", len - MASK, bpc[slot] as usize * 3 + 2);
        }
    }
}

#[test]
fn all_rotation_fields_changed_tail_stable() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(4242 + q as u64);
        let kf = S::make_realistic_payload(q, &mut rng);
        let mut cur = kf.clone();
        for s in 0..S::BONE_COUNT {
            S::flip_bone(&mut cur, q, s);
        }
        let (len, recon) = S::build_apply(&kf, &cur, q);
        // Bounded by the whole rotation region verbatim plus one mode bit per rotation field.
        assert!((MASK + 1..=MASK + ((S::BONE_COUNT + S::rot_bytes(q) * 8 + 7) >> 3)).contains(&len));
        assert_eq!(cur, recon);
    }
}

#[test]
fn all_byte_fields_changed_rotation_stable() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(4343 + q as u64);
        let kf = S::make_realistic_payload(q, &mut rng);
        let mut cur = kf.clone();
        cur[0] ^= 0xFF; // position
        cur[S::scale_offset(q)] ^= 0xFF; // scale
        cur[S::body_rot_offset(q)] ^= 0xFF; // body rot
        cur[S::hips_delta_offset(q)] ^= 0xFF; // hips delta
        cur[S::hips_rot_offset(q)] ^= 0xFF; // hips rot
        let has_effector = S::end_effector_bytes(q) > 0;
        if has_effector {
            cur[S::end_effector_offset(q)] ^= 0xFF; // effector block (High only)
        }
        let fields: Vec<usize> = if has_effector {
            vec![FIELD_POSITION, FIELD_SCALE, FIELD_BODY_ROT, FIELD_HIPS_DELTA, FIELD_HIPS_ROT, FIELD_END_EFFECTOR]
        } else {
            vec![FIELD_POSITION, FIELD_SCALE, FIELD_BODY_ROT, FIELD_HIPS_DELTA, FIELD_HIPS_ROT]
        };
        let (len, recon) = S::build_apply(&kf, &cur, q);
        assert!((MASK + 1..=max_body_for(q, &fields)).contains(&len));
        assert_eq!(cur, recon);
    }
}

#[test]
fn everything_changed_stays_under_max_delta_size() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(4444 + q as u64);
        let kf = S::make_realistic_payload(q, &mut rng);
        let mut cur = kf.clone();
        cur[0] ^= 0xFF;
        cur[S::scale_offset(q)] ^= 0xFF;
        cur[S::body_rot_offset(q)] ^= 0xFF;
        cur[S::hips_delta_offset(q)] ^= 0xFF;
        cur[S::hips_rot_offset(q)] ^= 0xFF;
        for s in 0..S::BONE_COUNT {
            S::flip_bone(&mut cur, q, s);
        }
        if S::end_effector_bytes(q) > 0 {
            S::flip_end_effector(&mut cur, q);
        }
        let (len, recon) = S::build_apply(&kf, &cur, q);
        assert!((MASK + 1..=BasisAvatarDeltaCompression::max_delta_size(q)).contains(&len));
        assert_eq!(cur, recon);
    }
}

#[test]
fn k_rotation_fields_changed_contiguous_packing() {
    for (q, k) in [(BitQuality::High, 1usize), (BitQuality::High, 3), (BitQuality::High, 7), (BitQuality::Medium, 5), (BitQuality::Low, 10), (BitQuality::VeryLow, 20)] {
        let mut rng = TestRng::new((k * 101 + q as usize) as u64);
        let kf = S::make_realistic_payload(q, &mut rng);
        let mut cur = kf.clone();
        let mut slots = HashSet::new();
        while slots.len() < k {
            slots.insert(rng.next(S::BONE_COUNT));
        }
        for &s in &slots {
            S::flip_bone(&mut cur, q, s);
        }
        let (len, recon) = S::build_apply(&kf, &cur, q);
        let fields: Vec<usize> = slots.iter().map(|&s| bone_field(s)).collect();
        assert!((MASK + 1..=max_body_for(q, &fields)).contains(&len));
        assert_eq!(cur, recon);
    }
}
