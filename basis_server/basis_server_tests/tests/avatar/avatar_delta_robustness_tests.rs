//! Defensive behavior: bad inputs return sentinel values (never panic), corrupt/truncated deltas
//! are rejected rather than misapplied, and the baseline is never mutated. Includes fuzzing.

use basis_network_core::compression::{BasisAvatarDeltaCompression, BitQuality};
use basis_server_tests::support::DeltaTestSupport as S;
use basis_server_tests::support::delta_test_support::TestRng;

fn dst(q: BitQuality) -> Vec<u8> {
    vec![0u8; BasisAvatarDeltaCompression::max_delta_size(q)]
}

#[test]
fn build_delta_undersized_returns_none() {
    let q = BitQuality::High;
    let size = S::payload_size(q);
    let good = vec![0u8; size];
    assert_eq!(BasisAvatarDeltaCompression::build_delta(&[], &good, q, &mut dst(q), 0), None);
    assert_eq!(BasisAvatarDeltaCompression::build_delta(&good, &[], q, &mut dst(q), 0), None);
    assert_eq!(BasisAvatarDeltaCompression::build_delta(&good, &good, q, &mut [], 0), None);
    assert_eq!(BasisAvatarDeltaCompression::build_delta(&vec![0u8; size - 1], &good, q, &mut dst(q), 0), None);
    assert_eq!(BasisAvatarDeltaCompression::build_delta(&good, &vec![0u8; size - 1], q, &mut dst(q), 0), None);
    // dst too small for the worst case.
    assert_eq!(BasisAvatarDeltaCompression::build_delta(&good, &good, q, &mut [0u8; 10], 0), None);
    // dst start past the end.
    assert_eq!(BasisAvatarDeltaCompression::build_delta(&good, &good, q, &mut dst(q), usize::MAX), None);
}

#[test]
fn try_apply_delta_undersized_returns_false() {
    let q = BitQuality::Medium;
    let size = S::payload_size(q);
    let baseline = vec![0u8; size];
    let mut d = dst(q);
    let len = BasisAvatarDeltaCompression::build_delta(&baseline, &baseline, q, &mut d, 0).unwrap(); // mask-only delta
    let mut out_full = vec![0u8; size];

    assert!(!BasisAvatarDeltaCompression::try_apply_delta(&[], &d, 0, len, q, &mut out_full));
    assert!(!BasisAvatarDeltaCompression::try_apply_delta(&baseline, &[], 0, len, q, &mut out_full));
    assert!(!BasisAvatarDeltaCompression::try_apply_delta(&baseline, &d, 0, len, q, &mut []));
    assert!(!BasisAvatarDeltaCompression::try_apply_delta(&vec![0u8; size - 1], &d, 0, len, q, &mut out_full));
    assert!(!BasisAvatarDeltaCompression::try_apply_delta(&baseline, &d, 0, len, q, &mut vec![0u8; size - 1]));
    // Fewer bytes than even the dirty mask.
    assert!(!BasisAvatarDeltaCompression::try_apply_delta(&baseline, &d, 0, BasisAvatarDeltaCompression::DIRTY_MASK_BYTES - 1, q, &mut out_full));
}

#[test]
fn try_apply_delta_wrong_length_is_rejected() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(q as u64 + 77);
        let kf = S::make_payload(q, &mut rng);
        let cur = S::make_payload(q, &mut rng);
        let mut d = dst(q);
        let len = BasisAvatarDeltaCompression::build_delta(&kf, &cur, q, &mut d, 0).unwrap();
        let mut out_full = vec![0u8; S::payload_size(q)];
        // One byte short and one byte long must both fail (mask says exactly `len`).
        assert!(!BasisAvatarDeltaCompression::try_apply_delta(&kf, &d, 0, len - 1, q, &mut out_full));
        assert!(!BasisAvatarDeltaCompression::try_apply_delta(&kf, &d, 0, len + 1, q, &mut out_full));
    }
}

#[test]
fn try_apply_delta_out_of_range_window_is_rejected() {
    let q = BitQuality::Low;
    let mut rng = TestRng::new(5);
    let kf = S::make_payload(q, &mut rng);
    let cur = S::make_payload(q, &mut rng);
    let mut d = dst(q);
    let len = BasisAvatarDeltaCompression::build_delta(&kf, &cur, q, &mut d, 0).unwrap();
    let mut out_full = vec![0u8; S::payload_size(q)];
    assert!(!BasisAvatarDeltaCompression::try_apply_delta(&kf, &d, usize::MAX, len, q, &mut out_full));
    assert!(!BasisAvatarDeltaCompression::try_apply_delta(&kf, &d, d.len() - 2, len, q, &mut out_full));
}

#[test]
fn try_apply_delta_does_not_mutate_baseline() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(q as u64 + 321);
        for _ in 0..50 {
            let kf = S::make_payload(q, &mut rng);
            let baseline_copy = kf.clone();
            let cur = S::make_payload(q, &mut rng);
            let mut d = dst(q);
            let len = BasisAvatarDeltaCompression::build_delta(&kf, &cur, q, &mut d, 0).unwrap();
            let mut out_full = vec![0u8; S::payload_size(q)];
            assert!(BasisAvatarDeltaCompression::try_apply_delta(&kf, &d, 0, len, q, &mut out_full));
            assert_eq!(baseline_copy, kf); // baseline untouched
        }
    }
}

#[test]
fn delta_body_length_insufficient_data_returns_none() {
    let q = BitQuality::High;
    let buf = vec![0u8; BasisAvatarDeltaCompression::DIRTY_MASK_BYTES];
    assert_eq!(BasisAvatarDeltaCompression::delta_body_length(&buf, 0, BasisAvatarDeltaCompression::DIRTY_MASK_BYTES - 1, q), None);
    assert_eq!(BasisAvatarDeltaCompression::delta_body_length(&[], 0, 100, q), None);
}

#[test]
fn fuzz_garbage_delta_never_panics() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(q as u64 + 8888);
        let baseline = S::make_payload(q, &mut rng);
        let mut out_full = vec![0u8; S::payload_size(q)];
        for _ in 0..20000 {
            let n = rng.next(S::payload_size(q) + 20);
            let garbage = rng.bytes(n);
            // Neither call may panic regardless of content; correctness is that they reject bad data.
            let probe = BasisAvatarDeltaCompression::delta_body_length(&garbage, 0, garbage.len(), q);
            let applied = BasisAvatarDeltaCompression::try_apply_delta(&baseline, &garbage, 0, garbage.len(), q, &mut out_full);
            if applied {
                // If it accepted the bytes, the claimed length must have matched the mask.
                assert_eq!(probe, Some(garbage.len()));
            }
        }
    }
}

#[test]
fn fuzz_truncated_valid_delta_never_panics() {
    for q in S::ALL_QUALITIES {
        let mut rng = TestRng::new(q as u64 + 9999);
        let baseline = S::make_payload(q, &mut rng);
        let mut out_full = vec![0u8; S::payload_size(q)];
        let mut d = dst(q);
        for _ in 0..2000 {
            let len = BasisAvatarDeltaCompression::build_delta(&baseline, &S::make_payload(q, &mut rng), q, &mut d, 0).unwrap();
            let truncated = rng.next(len + 1);
            // Truncation should be rejected (length mismatch) but must never panic.
            let _ = BasisAvatarDeltaCompression::try_apply_delta(&baseline, &d, 0, truncated, q, &mut out_full);
        }
    }
}
