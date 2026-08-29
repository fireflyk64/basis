//! Wire-level tests: replicate the server's delta frame assembly and the client's parse and confirm
//! they agree, including the DeltaAvatarChannel header helpers, the per-receiver interval-patch
//! offset, and the body / trailing-additional-data split.

use basis_network_core::BasisNetworkCommons;
use basis_network_core::compression::{BasisAvatarDeltaCompression, BitQuality};
use basis_server_tests::support::DeltaTestSupport as S;
use basis_server_tests::support::delta_test_support::TestRng;

#[test]
fn delta_header_round_trips_all_combinations() {
    for q in S::ALL_QUALITIES {
        for add in [false, true] {
            for large in [false, true] {
                let h = BasisNetworkCommons::build_delta_header(q as i32, add, large);
                assert_eq!(BasisNetworkCommons::delta_header_quality(h), q as u8);
                assert_eq!(BasisNetworkCommons::delta_header_has_additional_data(h), add);
                assert_eq!(BasisNetworkCommons::delta_header_large_id(h), large);
            }
        }
    }
}

#[test]
fn delta_channel_is_thirty() {
    assert_eq!(BasisNetworkCommons::DELTA_AVATAR_CHANNEL, 30);
}

// Frame layout: [header:1][playerId:1|2][interval:1][sequence:1][baseSeq:1][delta body][additional?]
#[allow(clippy::too_many_arguments)]
fn assemble(kf: &[u8], cur: &[u8], q: BitQuality, player_id: u16, large_id: bool, has_additional: bool, interval: u8, seq: u8, base_seq: u8, additional: Option<&[u8]>) -> (Vec<u8>, usize) {
    let id_size = if large_id { 2 } else { 1 };
    let add_len = if has_additional { additional.map(|a| a.len()).unwrap_or(0) } else { 0 };
    let mut frame = vec![0u8; 1 + id_size + 3 + BasisAvatarDeltaCompression::max_delta_size(q) + add_len];
    let mut o = 0;
    frame[o] = BasisNetworkCommons::build_delta_header(q as i32, has_additional, large_id);
    o += 1;
    if large_id {
        frame[o] = (player_id & 0xFF) as u8;
        frame[o + 1] = ((player_id >> 8) & 0xFF) as u8;
        o += 2;
    } else {
        frame[o] = player_id as u8;
        o += 1;
    }
    frame[o] = interval;
    frame[o + 1] = seq;
    frame[o + 2] = base_seq;
    o += 3;
    let body_len = BasisAvatarDeltaCompression::build_delta(kf, cur, q, &mut frame, o).unwrap();
    assert!(body_len > 0);
    o += body_len;
    if add_len > 0 {
        frame[o..o + add_len].copy_from_slice(additional.unwrap());
        o += add_len;
    }
    (frame, o)
}

struct Parsed {
    q: BitQuality,
    has_additional: bool,
    large_id: bool,
    player_id: u16,
    interval: u8,
    seq: u8,
    base_seq: u8,
    ok: bool,
    recon: Vec<u8>,
    additional_start: usize,
    additional_len: usize,
}

fn parse(frame: &[u8], total_len: usize, baseline: &[u8]) -> Parsed {
    let mut o = 0;
    let header = frame[o];
    o += 1;
    let q = BitQuality::from_byte(BasisNetworkCommons::delta_header_quality(header)).unwrap();
    let has_add = BasisNetworkCommons::delta_header_has_additional_data(header);
    let large = BasisNetworkCommons::delta_header_large_id(header);
    let player_id = if large {
        let id = u16::from_le_bytes([frame[o], frame[o + 1]]);
        o += 2;
        id
    } else {
        let id = frame[o] as u16;
        o += 1;
        id
    };
    let interval = frame[o];
    let seq = frame[o + 1];
    let base_seq = frame[o + 2];
    o += 3;

    let avail = total_len - o;
    let body_len = BasisAvatarDeltaCompression::delta_body_length(frame, o, avail, q).unwrap();
    assert!(body_len <= avail);
    let mut recon = vec![0u8; S::payload_size(q)];
    let ok = BasisAvatarDeltaCompression::try_apply_delta(baseline, frame, o, body_len, q, &mut recon);
    let add_start = o + body_len;
    Parsed { q, has_additional: has_add, large_id: large, player_id, interval, seq, base_seq, ok, recon, additional_start: add_start, additional_len: total_len - add_start }
}

#[test]
fn frame_round_trip_all_combinations() {
    let mut rng = TestRng::new(20240607);
    for q in S::ALL_QUALITIES {
        for large in [false, true] {
            for has_add in [false, true] {
                for _ in 0..40 {
                    let kf = S::make_realistic_payload(q, &mut rng);
                    let cur = S::make_realistic_payload(q, &mut rng);
                    let player_id: u16 = if large { 300 + rng.next(60000) as u16 } else { rng.next(256) as u16 };
                    let interval = rng.next(256) as u8;
                    let seq = rng.next(256) as u8;
                    let base_seq = rng.next(256) as u8;
                    let add = if has_add {
                        let n = 1 + rng.next(40);
                        Some(rng.bytes(n))
                    } else {
                        None
                    };

                    let (frame, len) = assemble(&kf, &cur, q, player_id, large, has_add, interval, seq, base_seq, add.as_deref());
                    let p = parse(&frame, len, &kf);

                    assert!(p.ok);
                    assert_eq!(p.q, q);
                    assert_eq!(p.has_additional, has_add);
                    assert_eq!(p.large_id, large);
                    assert_eq!(p.player_id, player_id);
                    assert_eq!(p.interval, interval);
                    assert_eq!(p.seq, seq);
                    assert_eq!(p.base_seq, base_seq);
                    assert_eq!(cur, p.recon);
                    // Trailing additional-data bytes survive the body/additional split intact.
                    if let Some(add) = add {
                        assert_eq!(p.additional_len, add.len());
                        assert_eq!(&frame[p.additional_start..p.additional_start + p.additional_len], &add[..]);
                    } else {
                        assert_eq!(p.additional_len, 0);
                    }
                }
            }
        }
    }
}

#[test]
fn interval_byte_at_expected_offset_and_patchable() {
    for large in [false, true] {
        let mut rng = TestRng::new(if large { 1 } else { 2 });
        let q = BitQuality::High;
        let kf = S::make_realistic_payload(q, &mut rng);
        let cur = S::make_realistic_payload(q, &mut rng);
        let player_id: u16 = if large { 4000 } else { 42 };
        let (mut frame, len) = assemble(&kf, &cur, q, player_id, large, false, 0, 5, 9, None);

        // The send loop patches the interval per-receiver at offset (1 + idSize): header + playerId.
        let id_size = if large { 2 } else { 1 };
        let interval_offset = 1 + id_size;
        assert_eq!(frame[interval_offset], 0);

        frame[interval_offset] = 123; // simulate the per-receiver patch
        let p = parse(&frame, len, &kf);
        assert!(p.ok);
        assert_eq!(p.interval, 123);
        assert_eq!(cur, p.recon); // patching interval must not disturb the body
    }
}

#[test]
fn delta_before_keyframe_is_droppable_by_baseline_mismatch() {
    // A receiver with a different-size / absent baseline must not silently apply.
    let mut rng = TestRng::new(31);
    let q = BitQuality::Medium;
    let kf = S::make_realistic_payload(q, &mut rng);
    let cur = S::make_realistic_payload(q, &mut rng);
    let (frame, len) = assemble(&kf, &cur, q, 7, false, false, 0, 3, 88, None);
    let wrong_size_baseline = vec![0u8; S::payload_size(q) - 1];
    let o = 1 + 1 + 3; // header + id + interval + seq + baseSeq
    let body_len = BasisAvatarDeltaCompression::delta_body_length(&frame, o, len - o, q).unwrap();
    let mut recon = vec![0u8; S::payload_size(q)];
    assert!(!BasisAvatarDeltaCompression::try_apply_delta(&wrong_size_baseline, &frame, o, body_len, q, &mut recon));
}
