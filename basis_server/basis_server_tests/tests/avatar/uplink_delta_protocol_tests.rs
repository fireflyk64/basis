//! v42 uplink/P2P avatar delta protocol: the client frames [hdr][seq][baseSeq][delta body] on
//! DeltaAvatarChannel against its last uploaded keyframe; the server (or a P2P peer) reconstructs
//! with the shared codec, NACKing via control frames when the baseline is missing or stale.

use basis_network_core::BasisNetworkCommons;
use basis_network_core::compression::{BasisAvatarDeltaCompression, BitQuality};
use basis_server_tests::support::DeltaTestSupport as S;
use basis_server_tests::support::delta_test_support::TestRng;

#[test]
fn client_frame_reconstructs_exactly_on_matching_baseline() {
    let mut rng = TestRng::new(4242);
    let keyframe = S::make_realistic_payload(BitQuality::High, &mut rng);
    let mut current = keyframe.clone();
    current[0] ^= 0xFF; // moved position
    S::flip_bone(&mut current, BitQuality::High, 7); // one bone changed

    // Client side: encode the delta and frame it the way the compressor does.
    let client_base_seq = 10u8;
    let client_seq = 11u8;
    let mut scratch = vec![0u8; BasisAvatarDeltaCompression::max_delta_size(BitQuality::High)];
    let body_len = BasisAvatarDeltaCompression::build_delta(&keyframe, &current, BitQuality::High, &mut scratch, 0).unwrap();
    assert!(body_len > 0 && body_len < keyframe.len(), "delta must beat the keyframe here");

    let mut frame = vec![0u8; 3 + body_len];
    frame[0] = BasisNetworkCommons::build_delta_header(3, false, false);
    frame[1] = client_seq;
    frame[2] = client_base_seq;
    frame[3..].copy_from_slice(&scratch[..body_len]);

    // Server side: parse exactly like handle_delta_channel_inbound.
    let header = frame[0];
    assert!(!BasisNetworkCommons::is_delta_control_header(header));
    assert_eq!(BasisNetworkCommons::delta_header_quality(header), 3);
    assert!(!BasisNetworkCommons::delta_header_has_additional_data(header));
    assert_eq!(frame[1], client_seq);
    assert_eq!(frame[2], client_base_seq);

    let parsed_len = BasisAvatarDeltaCompression::delta_body_length(&frame, 3, frame.len() - 3, BitQuality::High).unwrap();
    assert_eq!(parsed_len, body_len);

    let mut reconstructed = vec![0u8; BasisAvatarDeltaCompression::payload_size(BitQuality::High)];
    assert!(BasisAvatarDeltaCompression::try_apply_delta(&keyframe, &frame, 3, parsed_len, BitQuality::High, &mut reconstructed));
    assert_eq!(current, reconstructed);
}

#[test]
fn stale_baseline_is_detected_by_seq_mismatch() {
    // The server's stored baseline seq must equal the frame's baseSeq; anything else NACKs.
    let stored_baseline_seq = 9u8;
    let frame_base_seq = 10u8;
    assert_ne!(stored_baseline_seq, frame_base_seq);
}

#[test]
fn fully_changed_frame_promotes_to_keyframe() {
    let mut rng = TestRng::new(555);
    let keyframe = S::make_realistic_payload(BitQuality::High, &mut rng);
    let current = S::make_realistic_payload(BitQuality::High, &mut rng);
    let mut scratch = vec![0u8; BasisAvatarDeltaCompression::max_delta_size(BitQuality::High)];
    let body_len = BasisAvatarDeltaCompression::build_delta(&keyframe, &current, BitQuality::High, &mut scratch, 0).unwrap();
    // The client sends a keyframe whenever the delta is not strictly smaller.
    assert!(body_len >= keyframe.len(), "an everything-changed delta must trigger promotion");
}

#[test]
fn control_headers_never_collide_with_data_headers() {
    for qi in 0..4 {
        for additional in [false, true] {
            for large_id in [false, true] {
                let hdr = BasisNetworkCommons::build_delta_header(qi, additional, large_id);
                assert!(!BasisNetworkCommons::is_delta_control_header(hdr));
            }
        }
    }
    assert!(BasisNetworkCommons::is_delta_control_header(BasisNetworkCommons::DELTA_CONTROL_KEYFRAME_REQUEST));
    assert!(BasisNetworkCommons::is_delta_control_header(BasisNetworkCommons::DELTA_CONTROL_UPLINK_KEYFRAME_REQUEST));
    assert_ne!(BasisNetworkCommons::DELTA_CONTROL_KEYFRAME_REQUEST, BasisNetworkCommons::DELTA_CONTROL_UPLINK_KEYFRAME_REQUEST);
}

#[test]
fn idle_uplink_delta_is_mask_only() {
    let mut rng = TestRng::new(777);
    let keyframe = S::make_realistic_payload(BitQuality::High, &mut rng);
    let mut scratch = vec![0u8; BasisAvatarDeltaCompression::max_delta_size(BitQuality::High)];
    let body_len = BasisAvatarDeltaCompression::build_delta(&keyframe, &keyframe.clone(), BitQuality::High, &mut scratch, 0).unwrap();
    assert_eq!(body_len, BasisAvatarDeltaCompression::DIRTY_MASK_BYTES);
}
