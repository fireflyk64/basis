//! Idle-suppression decision (`BasisAvatarIdleSuppression::should_send`) and a faithful
//! simulation of the client send gate. Suppression only drops byte-identical, additional-free
//! frames, so it is lossless; these pin that contract and characterize the packet reduction.

use basis_network_core::compression::{BasisAvatarIdleSuppression, BitQuality};
use basis_server_tests::support::DeltaTestSupport as S;
use basis_server_tests::support::delta_test_support::TestRng;

const HB: f64 = BasisAvatarIdleSuppression::DEFAULT_HEARTBEAT_SECONDS;
const DT: f64 = 0.05; // 20 Hz

fn send(cur: &[u8], last: &[u8], has_last: bool, add: bool, linked: bool, now: f64, last_t: f64) -> bool {
    BasisAvatarIdleSuppression::should_send(cur, last, has_last, add, linked, now, last_t, HB)
}

fn realistic(seed: u64) -> Vec<u8> {
    S::make_realistic_payload(BitQuality::High, &mut TestRng::new(seed))
}

#[test]
fn first_frame_always_sends() {
    let p = realistic(1);
    assert!(send(&p, &[], false, false, false, 0.0, 0.0));
}

#[test]
fn identical_within_heartbeat_suppressed() {
    let p = realistic(2);
    let last = p.clone();
    assert!(!send(&p, &last, true, false, false, 0.5, 0.0));
}

#[test]
fn identical_heartbeat_elapsed_sends() {
    let p = realistic(3);
    let last = p.clone();
    assert!(send(&p, &last, true, false, false, HB, 0.0));
}

#[test]
fn one_bit_changed_sends() {
    let mut p = realistic(4);
    let last = p.clone();
    p[10] ^= 0x01;
    assert!(send(&p, &last, true, false, false, 0.1, 0.0));
}

#[test]
fn additional_data_forces_send() {
    let p = realistic(5);
    let last = p.clone();
    assert!(send(&p, &last, true, true, false, 0.1, 0.0));
}

#[test]
fn linked_avatar_change_forces_send() {
    let p = realistic(6);
    let last = p.clone();
    assert!(send(&p, &last, true, false, true, 0.1, 0.0));
}

#[test]
fn length_change_forces_send() {
    let p = realistic(7);
    let last = vec![0u8; p.len() - 1];
    assert!(send(&p, &last, true, false, false, 0.1, 0.0));
}

/// Faithful model of the client gate (Compress + RecordLastSent): a frame is emitted only when
/// should_send is true, and that becomes the new baseline.
fn simulate_sends(frames: &[Vec<u8>], dt: f64, heartbeat: f64) -> usize {
    let mut last: Vec<u8> = Vec::new();
    let mut last_t = 0.0;
    let mut has_last = false;
    let mut sends = 0;
    for (i, frame) in frames.iter().enumerate() {
        let now = i as f64 * dt;
        if BasisAvatarIdleSuppression::should_send(frame, &last, has_last, false, false, now, last_t, heartbeat) {
            sends += 1;
            last = frame.clone();
            last_t = now;
            has_last = true;
        }
    }
    sends
}

#[test]
fn idle_player_sends_only_heartbeats() {
    let pose = realistic(100);
    let n = (10.0 / DT) as usize; // 200 frames, 10 s
    let frames = vec![pose; n]; // motionless: identical every frame
    let sends = simulate_sends(&frames, DT, HB);
    let reduction = 1.0 - sends as f64 / n as f64;
    println!("Idle 10 s @20 Hz: {sends}/{n} frames sent ({:.1}% fewer packets)", reduction * 100.0);
    assert!(sends <= 12, "idle sends {sends} exceeded heartbeat budget");
    assert!(reduction >= 0.90, "idle packet reduction {:.1}% < 90%", reduction * 100.0);
}

#[test]
fn mixed_timeline_reduces_packets() {
    let mut rng = TestRng::new(200);
    let mut frames = Vec::new();
    // 4 s idle
    let rest_a = S::make_realistic_payload(BitQuality::High, &mut rng);
    for _ in 0..80 {
        frames.push(rest_a.clone());
    }
    // 2 s moving: a distinct quantized pose every frame
    let mut last_move = rest_a;
    for _ in 0..40 {
        last_move = S::make_realistic_payload(BitQuality::High, &mut rng);
        frames.push(last_move.clone());
    }
    // 4 s idle where the player came to rest (holds the last moving pose)
    for _ in 0..80 {
        frames.push(last_move.clone());
    }

    let n = frames.len(); // 200
    let sends = simulate_sends(&frames, DT, HB);
    let reduction = 1.0 - sends as f64 / n as f64;
    println!("Mixed (idle/move/idle) 10 s @20 Hz: {sends}/{n} frames sent ({:.1}% fewer packets)", reduction * 100.0);
    assert!(reduction >= 0.50, "mixed packet reduction {:.1}% < 50%", reduction * 100.0);
}

#[test]
fn continuous_motion_sends_every_frame() {
    let mut rng = TestRng::new(300);
    let n = 100;
    let frames: Vec<Vec<u8>> = (0..n).map(|_| S::make_realistic_payload(BitQuality::High, &mut rng)).collect();
    let sends = simulate_sends(&frames, DT, HB);
    assert_eq!(sends, n); // no suppression under real motion — never drops a moving frame
}

#[test]
fn print_packet_and_byte_table() {
    let payload = S::payload_size(BitQuality::High);
    const WIRE_OVERHEAD: usize = 1; // app sequence byte (transport header excluded)
    let per_packet = payload + WIRE_OVERHEAD;
    println!("High payload = {payload} B, per-packet wire ≈ {per_packet} B (excl. UDP/transport header)");
    println!();
    println!("scenario (10 s @20 Hz, 200 frames) | packets before→after | uplink B/s before→after | reduction");

    let row = |name: &str, frames: &[Vec<u8>]| {
        let n = frames.len();
        let sends = simulate_sends(frames, DT, HB);
        let before = (n * per_packet) as f64 / 10.0;
        let after = (sends * per_packet) as f64 / 10.0;
        println!("  {name:<24} | {n:>4} → {sends:>3}          | {before:>7.0} → {after:>6.0}        | {:>6.1}%", (1.0 - sends as f64 / n as f64) * 100.0);
    };

    let mut rng = TestRng::new(2025);
    let idle_pose = S::make_realistic_payload(BitQuality::High, &mut rng);
    let idle = vec![idle_pose; 200];

    let mut mixed = Vec::new();
    let rest = S::make_realistic_payload(BitQuality::High, &mut rng);
    for _ in 0..120 {
        mixed.push(rest.clone());
    }
    let mut lm = rest;
    for _ in 0..20 {
        lm = S::make_realistic_payload(BitQuality::High, &mut rng);
        mixed.push(lm.clone());
    }
    for _ in 0..60 {
        mixed.push(lm.clone());
    }

    let moving: Vec<Vec<u8>> = (0..200).map(|_| S::make_realistic_payload(BitQuality::High, &mut rng)).collect();

    row("fully idle", &idle);
    row("mostly idle (10% move)", &mixed);
    row("continuous motion", &moving);
}
