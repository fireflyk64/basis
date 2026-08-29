//! End-to-end protocol simulation. Mirrors the server's keyframe/delta decision and
//! per-(sender,receiver) baseline selection and the client's baseline gate, driving the REAL codec
//! through packet loss, rate throttling, quality switches, and keyframe promotion. The invariant:
//! whenever the client accepts a frame, its reconstruction EXACTLY equals the sender's true pose at
//! that frame — it never applies a delta against the wrong baseline — and it re-synchronizes after
//! loss. baseSeq is a single byte, so lossy runs are capped below 256 generations.

use std::collections::HashSet;

use basis_network_core::compression::{BasisAvatarDeltaCompression, BitQuality};
use basis_server_tests::support::DeltaTestSupport as S;
use basis_server_tests::support::delta_test_support::TestRng;

enum Frame {
    Key { q: usize, seq: u8, payload: Vec<u8>, true_gen: i64 },
    Delta { q: usize, #[allow(dead_code)] seq: u8, base_seq: u8, body: Vec<u8>, true_gen: i64 },
}

#[derive(Debug)]
struct Stats {
    applied: i32,
    keyframes: i32,
    deltas: i32,
    dropped: i32,
    last_applied_gen: i64,
}

fn mutate(prev: &[u8], q: BitQuality, rng: &mut TestRng, big_jump: bool) -> Vec<u8> {
    let mut next = prev.to_vec();
    if rng.next_f64() < 0.6 {
        next[0] = rng.next(256) as u8; // root position drifts often
    }
    let n_bones = if big_jump { S::BONE_COUNT } else { rng.next(4) };
    let mut used = HashSet::new();
    for i in 0..n_bones {
        let slot = if big_jump { i } else { rng.next(S::BONE_COUNT) };
        if !used.insert(slot) {
            continue;
        }
        let maxv = (1u64 << S::bone_width(q, slot)) - 1;
        S::set_bone(&mut next, q, slot, rng.next_u64() & maxv);
    }
    if big_jump {
        next[S::scale_offset(q)] ^= 0xFF;
        next[S::hips_rot_offset(q)] ^= 0xFF;
    }
    next
}

fn q_of(i: usize) -> BitQuality {
    BitQuality::from_byte(i as u8).unwrap()
}

fn run_scenario(gens: usize, loss: f64, serve_period: usize, switch_quality: bool, kf_interval: i64, seed: u64, receiver_quality_schedule: Option<&[usize]>) -> Stats {
    let mut rng = TestRng::new(seed);

    // Build four evolving quality streams; big jumps happen on the same gens across qualities.
    let mut poses: Vec<Vec<Vec<u8>>> = (0..4).map(|qi| vec![S::make_realistic_payload(q_of(qi), &mut rng)]).collect();
    for _ in 1..gens {
        let big = rng.next_f64() < 0.02;
        for qi in 0..4 {
            let prev = poses[qi].last().unwrap().clone();
            let next = mutate(&prev, q_of(qi), &mut rng, big);
            poses[qi].push(next);
        }
    }

    // Server global state.
    let mut keyframe_gen: i64 = 0;
    let mut keyframe_seq: u8 = 0;
    let mut last_kf_gen: i64 = 0;
    let mut keyframe_payload: Vec<Vec<u8>> = (0..4).map(|qi| poses[qi][0].clone()).collect();
    let mut current_is_keyframe;
    let mut probe = vec![0u8; BasisAvatarDeltaCompression::max_delta_size(BitQuality::High)];

    // Server per-receiver baseline view.
    let mut srv_baseline_gen: i64 = 0;
    let mut srv_baseline_q: i64 = -1;

    // Client state.
    let mut cli_baseline: Option<Vec<u8>> = None;
    let mut cli_baseline_seq: u8 = 0;
    let mut cli_baseline_q: i64 = -1;
    let mut last_applied: i64 = -1;
    let (mut applied, mut kf_a, mut d_a, mut dropped) = (0, 0, 0, 0);

    let mut rq = 3usize; // receiver starts at High

    for g in 0..gens {
        let seq = g as u8;
        let is_kf = if g == 0 {
            true
        } else {
            let periodic = (g as i64 - last_kf_gen) >= kf_interval;
            let mut promote = false;
            if !periodic {
                let probe_len = BasisAvatarDeltaCompression::build_delta(&keyframe_payload[3], &poses[3][g], BitQuality::High, &mut probe, 0);
                promote = match probe_len {
                    None => true,
                    Some(len) => len >= BasisAvatarDeltaCompression::payload_size(BitQuality::High),
                };
            }
            periodic || promote
        };
        if is_kf {
            keyframe_gen = g as i64;
            keyframe_seq = seq;
            last_kf_gen = g as i64;
            for qi in 0..4 {
                keyframe_payload[qi] = poses[qi][g].clone();
            }
            current_is_keyframe = true;
        } else {
            current_is_keyframe = false;
        }

        if let Some(schedule) = receiver_quality_schedule {
            rq = schedule[g];
        } else if switch_quality && g > 0 && rng.next_f64() < 0.03 {
            rq = rng.next(4);
        }

        if g % serve_period != 0 {
            continue;
        }

        // Server send decision (mirrors the hot send loop).
        let send_delta = !current_is_keyframe && srv_baseline_gen == keyframe_gen && srv_baseline_q == rq as i64;
        let frame = if send_delta {
            let mut dst = vec![0u8; BasisAvatarDeltaCompression::max_delta_size(q_of(rq))];
            let body = BasisAvatarDeltaCompression::build_delta(&keyframe_payload[rq], &poses[rq][g], q_of(rq), &mut dst, 0).unwrap();
            Frame::Delta { q: rq, seq, base_seq: keyframe_seq, body: dst[..body].to_vec(), true_gen: g as i64 }
        } else {
            srv_baseline_gen = keyframe_gen; // server records the (possibly-lost) send unconditionally
            srv_baseline_q = rq as i64;
            Frame::Key { q: rq, seq: keyframe_seq, payload: keyframe_payload[rq].clone(), true_gen: keyframe_gen }
        };

        if rng.next_f64() < loss {
            continue; // lost in transit
        }

        // Client processing.
        match frame {
            Frame::Key { q, seq, payload, true_gen } => {
                assert_eq!(poses[q][true_gen as usize], payload);
                cli_baseline = Some(payload);
                cli_baseline_seq = seq;
                cli_baseline_q = q as i64;
                last_applied = true_gen;
                applied += 1;
                kf_a += 1;
            }
            Frame::Delta { q, base_seq, body, true_gen, .. } => {
                if let Some(baseline) = cli_baseline.as_ref().filter(|_| cli_baseline_q == q as i64 && cli_baseline_seq == base_seq) {
                    let mut recon = vec![0u8; BasisAvatarDeltaCompression::payload_size(q_of(q))];
                    let ok = BasisAvatarDeltaCompression::try_apply_delta(baseline, &body, 0, body.len(), q_of(q), &mut recon);
                    assert!(ok, "a delta that matched the held baseline failed to apply");
                    assert_eq!(poses[q][true_gen as usize], recon); // THE invariant: exact reconstruction
                    last_applied = true_gen;
                    applied += 1;
                    d_a += 1;
                } else {
                    dropped += 1;
                }
            }
        }
    }
    Stats { applied, keyframes: kf_a, deltas: d_a, dropped, last_applied_gen: last_applied }
}

#[test]
fn protocol_reconstructs_exactly_and_resyncs() {
    // loss, servePeriod, switchQuality
    for (loss, serve_period, switch_quality) in [(0.0, 1, false), (0.0, 1, true), (0.0, 3, true), (0.1, 1, false), (0.1, 2, true), (0.3, 1, true), (0.3, 4, false), (0.5, 1, true), (0.5, 6, true)] {
        const GENS: usize = 240; // < 256: single non-wrapping seq window for lossy runs
        let seed = ((loss * 1000.0) as u64) * 100 + serve_period as u64 * 10 + u64::from(switch_quality);
        let st = run_scenario(GENS, loss, serve_period, switch_quality, 8, seed, None);

        assert!(st.applied > 0, "client never applied a frame");
        // Correctness is asserted inline; here we assert liveness (no permanent desync).
        if loss <= 0.5 {
            let served_last = ((GENS - 1) / serve_period * serve_period) as i64;
            // Re-sync within a few keyframe intervals of the end.
            assert!(st.last_applied_gen >= served_last - 8 * 8, "stale: lastApplied={}, served up to {served_last}", st.last_applied_gen);
        }
    }
}

#[test]
fn protocol_long_run_no_loss_sustained_exact_reconstruction() {
    // 1000 gens exercises seq wrap safely (loss-free => baseline always current), plus many
    // promotions from the injected big jumps. Correctness asserted inline throughout.
    let st = run_scenario(1000, 0.0, 1, true, 10, 777, None);
    assert!(st.deltas > 0 && st.keyframes > 0);
    assert_eq!(st.last_applied_gen, 999);
    assert_eq!(st.dropped, 0);
}

#[test]
fn protocol_quality_switch_forces_rebaseline_before_delta() {
    // Force a quality change every 5 gens; with no loss the client must always hold a matching
    // (quality, baseSeq) baseline before any delta, so reconstruction stays exact.
    let gens = 200;
    let mut schedule = vec![0usize; gens];
    let mut rng = TestRng::new(4);
    let mut cur = 3;
    for (g, slot) in schedule.iter_mut().enumerate() {
        if g % 5 == 0 {
            cur = rng.next(4);
        }
        *slot = cur;
    }
    let st = run_scenario(gens, 0.0, 1, false, 8, 4, Some(&schedule));
    assert!(st.applied > gens as i32 / 2);
    assert_eq!(st.last_applied_gen, gens as i64 - 1);
}

#[test]
fn protocol_moderate_loss_recovers_repeatedly() {
    let st = run_scenario(240, 0.35, 1, true, 6, 12345, None);
    assert!(st.keyframes >= 3, "expected repeated keyframe re-syncs under loss");
    assert!(st.deltas > 0, "expected some deltas to apply between keyframes");
}
