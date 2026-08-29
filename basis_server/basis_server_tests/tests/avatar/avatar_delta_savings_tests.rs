//! Bandwidth-savings characterization at different levels of data similarity. Wire sizes model
//! the common byte-id case: keyframe wire = 3 + payload, delta wire = 5 + deltaBody.
//! `print_savings_table` also prints what the previous fixed-width delta codec would have spent on
//! the same poses, the measurement that justifies the change.

use std::collections::HashSet;

use basis_network_core::compression::{BasisAvatarDeltaCompression, BasisBoneRotationCompression, BasisChannelKind, BitQuality};
use basis_server_tests::support::DeltaTestSupport as S;
use basis_server_tests::support::delta_test_support::TestRng;

const MASK: usize = BasisAvatarDeltaCompression::DIRTY_MASK_BYTES;

fn keyframe_wire(q: BitQuality) -> usize {
    3 + S::payload_size(q)
}

fn delta_wire(body: usize) -> usize {
    5 + body
}

fn savings(q: BitQuality, body: usize) -> f64 {
    1.0 - delta_wire(body) as f64 / keyframe_wire(q) as f64
}

fn body_for(kf: &[u8], cur: &[u8], q: BitQuality) -> usize {
    let mut dst = vec![0u8; BasisAvatarDeltaCompression::max_delta_size(q)];
    BasisAvatarDeltaCompression::build_delta(kf, cur, q, &mut dst, 0).unwrap()
}

/// What the superseded codec would have produced: the dirty mask, then every changed field
/// verbatim (byte fields whole, rotation fields bit-packed contiguously).
fn legacy_body_for(kf: &[u8], cur: &[u8], q: BitQuality) -> usize {
    let layout = S::layout(q);
    let (mut byte_field_bits, mut rot_field_bits) = (0usize, 0usize);
    for f in 0..BasisAvatarDeltaCompression::FIELD_COUNT {
        let mut dirty = false;
        for c in layout.field_channel_start(f)..layout.field_channel_end(f) {
            let ch = layout.channels[c];
            if BasisAvatarDeltaCompression::read_channel(cur, &ch) != BasisAvatarDeltaCompression::read_channel(kf, &ch) {
                dirty = true;
                break;
            }
        }
        if !dirty {
            continue;
        }
        let is_rotation = (BasisAvatarDeltaCompression::BONE_FIELD_START..BasisAvatarDeltaCompression::BONE_FIELD_START + BasisBoneRotationCompression::ROTATION_FIELD_COUNT).contains(&f);
        if is_rotation {
            rot_field_bits += layout.field_raw_bits(f);
        } else {
            byte_field_bits += layout.field_raw_bits(f);
        }
    }
    MASK + (byte_field_bits >> 3) + ((rot_field_bits + 7) >> 3)
}

#[test]
fn idle_saves_over_87_percent() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(q as u64);
        let kf = S::make_realistic_payload(q, &mut rng);
        let body = body_for(&kf, &kf.clone(), q);
        assert_eq!(body, MASK);
        // 87%, not 88: the v52 restricted-DOF encoding shrank the VeryLow keyframe to 74 bytes.
        assert!(savings(q, body) >= 0.87, "idle savings {:.1}% < 87% at {q:?}", savings(q, body) * 100.0);
    }
}

#[test]
fn root_position_only_saves_over_78_percent() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(q as u64 + 10);
        let kf = S::make_realistic_payload(q, &mut rng);
        let mut cur = kf.clone();
        cur[0] ^= 0xFF;
        let body = body_for(&kf, &cur, q);
        assert!(body <= MASK + S::pos_bytes(q) + 1);
        assert!(savings(q, body) >= 0.78, "position-only savings {:.1}% < 78% at {q:?}", savings(q, body) * 100.0);
    }
}

/// The case this codec exists for: every joint moving slightly, which is what a person standing
/// and talking produces.
#[test]
fn small_motion_everywhere_beats_legacy_substantially() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(q as u64 + 55);
        let (mut mine, mut legacy) = (0i64, 0i64);
        for _ in 0..200 {
            let kf = S::make_realistic_payload(q, &mut rng);
            let cur = nudge_all_components(&kf, q, &mut rng, 2);
            mine += body_for(&kf, &cur, q) as i64;
            legacy += legacy_body_for(&kf, &cur, q) as i64;
        }
        let ratio = mine as f64 / legacy as f64;
        // The narrow tiers gain least; High is where the bits actually are.
        let bound = if q == BitQuality::High { 0.45 } else { 0.75 };
        assert!(ratio < bound, "{q:?}: small-motion body is {:.1}% of the legacy scheme, expected under {:.0}% ({:.1} B vs {:.1} B)", ratio * 100.0, bound * 100.0, mine as f64 / 200.0, legacy as f64 / 200.0);
    }
}

#[test]
fn all_rotation_fields_still_saves_something() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(q as u64 + 20);
        let kf = S::make_realistic_payload(q, &mut rng);
        let mut cur = kf.clone();
        for s in 0..S::BONE_COUNT {
            S::flip_bone(&mut cur, q, s);
        }
        let body = body_for(&kf, &cur, q);
        assert!(body <= MASK + ((S::BONE_COUNT + S::rot_bytes(q) * 8 + 7) >> 3));
        assert!(savings(q, body) > 0.05, "all-rotation savings {:.1}% not positive enough at {q:?}", savings(q, body) * 100.0);
    }
}

#[test]
fn uncorrelated_poses_trigger_keyframe_promotion() {
    // Two independent random poses: every field falls back to raw. The server's
    // `body >= payload` guard has to fire on it or the delta is pure overhead.
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(q as u64 + 30);
        for _ in 0..50 {
            let kf = S::make_payload(q, &mut rng);
            let cur = S::make_payload(q, &mut rng);
            let body = body_for(&kf, &cur, q);
            assert!(body >= S::payload_size(q), "expected promotion at {q:?}: body {body} < payload {}", S::payload_size(q));
            assert!(body <= BasisAvatarDeltaCompression::max_delta_size(q));
        }
    }
}

/// Flipping one byte of each byte-field and every bit of each rotation field used to force
/// promotion. It no longer does: a one-byte flip of a 24-bit position axis is a bounded residual.
#[test]
fn wholesale_pose_change_stays_under_the_keyframe() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(q as u64 + 31);
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
        let body = body_for(&kf, &cur, q);
        let legacy = legacy_body_for(&kf, &cur, q);
        assert!(body < legacy, "{q:?}: {body} B should undercut the legacy {legacy} B");
        S::assert_round_trip(&kf, &cur, q);
    }
}

/// Perturbs every Delta channel by up to ±max_steps quantization steps.
fn nudge_all_components(kf: &[u8], q: BitQuality, rng: &mut TestRng, max_steps: i32) -> Vec<u8> {
    let mut cur = kf.to_vec();
    let layout = S::layout(q);
    for ch in &layout.channels {
        if ch.kind != BasisChannelKind::Delta {
            continue;
        }
        let step = rng.next_range(-max_steps, max_steps + 1);
        let v = BasisAvatarDeltaCompression::read_channel(&cur, ch);
        BasisAvatarDeltaCompression::write_channel(&mut cur, ch, ((v as i32).wrapping_add(step) as u32) & ch.mask());
    }
    cur
}

#[test]
fn print_savings_table() {
    let mut rng = TestRng::new(2024);
    const TRIALS: usize = 400;

    println!("Avatar delta bandwidth vs full keyframe (byte-id wire, averaged over realistic poses)");
    println!("'legacy' = the fixed-width delta codec this replaced, on the same poses.");
    println!();

    for q in S::ALL_QUALITIES {
        println!("== {q:?}  (keyframe wire = {} B, payload = {} B) ==", keyframe_wire(q), S::payload_size(q));
        println!("  scenario                | body B | legacy B | wire B | savings | vs legacy");

        type Mutate<'a> = &'a dyn Fn(&[u8], BitQuality, &mut TestRng) -> Vec<u8>;
        let mut row = |name: &str, mutate: Mutate<'_>| {
            let (mut mine_sum, mut legacy_sum) = (0i64, 0i64);
            for _ in 0..TRIALS {
                let kf = S::make_realistic_payload(q, &mut rng);
                let cur = mutate(&kf, q, &mut rng);
                mine_sum += body_for(&kf, &cur, q) as i64;
                legacy_sum += legacy_body_for(&kf, &cur, q) as i64;
            }
            let body = mine_sum as f64 / TRIALS as f64;
            let legacy = legacy_sum as f64 / TRIALS as f64;
            let wire = 5.0 + body;
            println!("  {name:<23} | {body:6.1} | {legacy:8.1} | {wire:6.1} | {:6.1}%  | {:6.1}%", (1.0 - wire / keyframe_wire(q) as f64) * 100.0, (1.0 - body / legacy) * 100.0);
        };

        row("idle", &|kf, _, _| kf.to_vec());
        row("position only", &|kf, _, _| {
            let mut c = kf.to_vec();
            c[0] ^= 0xFF;
            c
        });
        row("micro motion (+-1)", &|kf, qq, r| nudge_all_components(kf, qq, r, 1));
        row("small motion (+-2)", &|kf, qq, r| nudge_all_components(kf, qq, r, 2));
        row("moderate motion (+-8)", &|kf, qq, r| nudge_all_components(kf, qq, r, 8));
        row("large motion (+-64)", &|kf, qq, r| nudge_all_components(kf, qq, r, 64));
        row("k=5 fields re-posed", &|kf, qq, r| {
            let mut c = kf.to_vec();
            let mut slots = HashSet::new();
            while slots.len() < 5 {
                slots.insert(r.next(S::BONE_COUNT));
            }
            for s in slots {
                S::flip_bone(&mut c, qq, s);
            }
            c
        });
        row("everything re-randomized", &|kf, qq, _| {
            let mut c = kf.to_vec();
            for s in 0..S::BONE_COUNT {
                S::flip_bone(&mut c, qq, s);
            }
            c[0] ^= 0xFF;
            c[S::scale_offset(qq)] ^= 0xFF;
            c[S::body_rot_offset(qq)] ^= 0xFF;
            c[S::hips_delta_offset(qq)] ^= 0xFF;
            c[S::hips_rot_offset(qq)] ^= 0xFF;
            if S::end_effector_bytes(qq) > 0 {
                S::flip_end_effector(&mut c, qq);
            }
            c
        });
        println!();
    }
}
