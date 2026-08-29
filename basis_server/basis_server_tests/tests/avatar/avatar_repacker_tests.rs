//! Direct coverage of `AvatarQualityRepacker::build_all_lower_from_high_into`: position is int24-mm
//! at 0..9 in every tier, so it copies across untouched and the rotation bitstream starts at the
//! same base on both sides. The layout is asserted bit-exactly.

use basis_network_core::SerializableBasis::LocalAvatarSyncMessage;
use basis_network_core::compression::{BasisAvatarBitPacking, BasisBoneRotationCompression, BitQuality};
use basis_network_server::reduction::AvatarQualityRepacker;
use basis_server_tests::support::DeltaTestSupport as S;
use basis_server_tests::support::delta_test_support::TestRng;

fn rescale(q_src: u32, b_src: u32, b_dst: u32) -> u32 {
    if b_src == b_dst {
        return q_src;
    }
    let max_src = (1u64 << b_src) - 1;
    let max_dst = (1u64 << b_dst) - 1;
    ((q_src as u64 * max_dst + (max_src >> 1)) / max_src) as u32
}

/// Start bit of every rotation field relative to the rotation region — the WIRE geometry.
fn rotation_field_offsets(q: BitQuality) -> Vec<usize> {
    S::bone_bit_offsets(q)
}

/// A High payload laid out the way the wire actually carries one.
fn make_wire_high_payload(rng: &mut TestRng) -> Vec<u8> {
    let q = BitQuality::High;
    let mut arr = vec![0u8; BasisAvatarBitPacking::convert_to_size(q)];

    BasisAvatarBitPacking::encode_position((rng.next_f64() * 2000.0 - 1000.0) as f32, (rng.next_f64() * 2000.0 - 1000.0) as f32, (rng.next_f64() * 2000.0 - 1000.0) as f32, &mut arr, 0);
    let tail = S::tail_start(q);
    rng.next_bytes(&mut arr[tail..tail + BasisBoneRotationCompression::TAIL_BYTES]);
    let ee = S::end_effector_offset(q);
    let ee_len = S::end_effector_bytes(q);
    rng.next_bytes(&mut arr[ee..ee + ee_len]);

    let bpc = &BasisBoneRotationCompression::BPC_HIGH;
    let offs = rotation_field_offsets(q);
    let base_bit = S::bone_base_bit(q);
    for slot in 0..BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT {
        let (x, y, z, w) = S::random_quat(rng);
        let packed = if BasisBoneRotationCompression::BONE_DOF[slot] == 3 {
            BasisBoneRotationCompression::encode_smallest_three(x, y, z, w, bpc[slot] as u32, BasisBoneRotationCompression::MAX_COMPONENT[slot])
        } else {
            BasisBoneRotationCompression::encode_restricted(x, y, z, w, slot, q)
        };
        BasisBoneRotationCompression::write_bits(&mut arr, base_bit + offs[slot], packed, BasisBoneRotationCompression::bone_field_width(q, slot));
    }

    let finger_width = BasisBoneRotationCompression::finger_field_width(q);
    for finger in 0..BasisBoneRotationCompression::FINGER_CHANNEL_COUNT {
        let field = BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT + finger;
        let maxv = (1u64 << finger_width) - 1;
        BasisBoneRotationCompression::write_bits(&mut arr, base_bit + offs[field], rng.next_u64() & maxv, finger_width);
    }
    arr
}

#[test]
fn repacked_lower_tiers_match_expected_layout_bit_exactly() {
    let mut rng = TestRng::new(777);
    for _ in 0..50 {
        let high_arr = make_wire_high_payload(&mut rng);
        let high = LocalAvatarSyncMessage { array: Some(high_arr.clone()), data_quality_level: BitQuality::High as u8, ..Default::default() };
        let mut med = LocalAvatarSyncMessage::default();
        let mut low = LocalAvatarSyncMessage::default();
        let mut vlow = LocalAvatarSyncMessage::default();
        AvatarQualityRepacker::build_all_lower_from_high_into(&high, &mut med, &mut low, &mut vlow).unwrap();

        let high_bpc = &BasisBoneRotationCompression::BPC_HIGH;
        let high_offs = rotation_field_offsets(BitQuality::High);

        for (q, msg) in [(BitQuality::Medium, &med), (BitQuality::Low, &low), (BitQuality::VeryLow, &vlow)] {
            assert_eq!(msg.data_quality_level, q as u8);
            let array = msg.array.as_deref().expect("repacked array");
            assert!(array.len() >= BasisAvatarBitPacking::convert_to_size(q));

            let pos_bytes = BasisAvatarBitPacking::position_bytes(q);
            assert_eq!(pos_bytes, BasisAvatarBitPacking::WRITE_POSITION);

            // Position is the same encoding in both tiers, so it copies across byte-exactly.
            for i in 0..pos_bytes {
                assert_eq!(high_arr[i], array[i], "{q:?} position byte {i} mismatch");
            }

            // Every explicit bone is the High bone rescaled to the tier's BPC, written at the
            // 9-byte base.
            let bpc = BasisBoneRotationCompression::get_bpc_table(q);
            let offs = rotation_field_offsets(q);
            for slot in 0..BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT {
                let mut src_pos = S::bone_base_bit(BitQuality::High) + high_offs[slot];
                let raw_high = BasisBoneRotationCompression::read_bits(&high_arr, &mut src_pos, BasisBoneRotationCompression::bone_field_width(BitQuality::High, slot));

                let expected_packed: u64 = match BasisBoneRotationCompression::BONE_DOF[slot] {
                    3 => {
                        let idx = (raw_high & 3) as u32;
                        let hb = high_bpc[slot] as u32;
                        let mask_src = (1u32 << hb) - 1;
                        let qa = ((raw_high >> 2) as u32) & mask_src;
                        let qb = ((raw_high >> (2 + hb)) as u32) & mask_src;
                        let qc = ((raw_high >> (2 + 2 * hb)) as u32) & mask_src;
                        let b = bpc[slot] as u32;
                        idx as u64 | ((rescale(qa, hb, b) as u64) << 2) | ((rescale(qb, hb, b) as u64) << (2 + b)) | ((rescale(qc, hb, b) as u64) << (2 + 2 * b))
                    }
                    2 => {
                        let src_hinge = BasisBoneRotationCompression::hinge_bits(BitQuality::High);
                        let src_twist = BasisBoneRotationCompression::twist_bits(BitQuality::High);
                        let dst_hinge = BasisBoneRotationCompression::hinge_bits(q);
                        let dst_twist = BasisBoneRotationCompression::twist_bits(q);
                        let hinge = (raw_high & ((1u64 << src_hinge) - 1)) as u32;
                        let twist = ((raw_high >> src_hinge) & ((1u64 << src_twist) - 1)) as u32;
                        rescale(hinge, src_hinge, dst_hinge) as u64 | ((rescale(twist, src_twist, dst_twist) as u64) << dst_hinge)
                    }
                    _ => {
                        let src_bits = BasisBoneRotationCompression::single_axis_bits(BitQuality::High);
                        let dst_bits = BasisBoneRotationCompression::single_axis_bits(q);
                        rescale((raw_high & ((1u64 << src_bits) - 1)) as u32, src_bits, dst_bits) as u64
                    }
                };

                let mut dst_pos = pos_bytes * 8 + offs[slot];
                let actual_packed = BasisBoneRotationCompression::read_bits(array, &mut dst_pos, BasisBoneRotationCompression::bone_field_width(q, slot));
                assert_eq!(expected_packed, actual_packed, "{q:?} bone slot {slot} mismatch");
            }

            // Finger channels: curl and splay rescale independently on the same integer ladder.
            let src_curl_bits = BasisBoneRotationCompression::curl_bits(BitQuality::High);
            let src_splay_bits = BasisBoneRotationCompression::splay_bits(BitQuality::High);
            let dst_curl_bits = BasisBoneRotationCompression::curl_bits(q);
            let dst_splay_bits = BasisBoneRotationCompression::splay_bits(q);
            for finger in 0..BasisBoneRotationCompression::FINGER_CHANNEL_COUNT {
                let field = BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT + finger;
                let mut src_pos = S::bone_base_bit(BitQuality::High) + high_offs[field];
                let curl = BasisBoneRotationCompression::read_bits(&high_arr, &mut src_pos, src_curl_bits) as u32;
                let splay = BasisBoneRotationCompression::read_bits(&high_arr, &mut src_pos, src_splay_bits) as u32;
                let expected_packed = rescale(curl, src_curl_bits, dst_curl_bits) as u64 | ((rescale(splay, src_splay_bits, dst_splay_bits) as u64) << dst_curl_bits);
                let mut dst_pos = pos_bytes * 8 + offs[field];
                let actual_packed = BasisBoneRotationCompression::read_bits(array, &mut dst_pos, dst_curl_bits + dst_splay_bits);
                assert_eq!(expected_packed, actual_packed, "{q:?} finger channel {finger} mismatch");
            }

            // Tail is copied verbatim from the High source.
            let src_tail = S::tail_start(BitQuality::High);
            let dst_tail = pos_bytes + BasisBoneRotationCompression::rotation_bytes(q);
            for i in 0..BasisBoneRotationCompression::TAIL_BYTES {
                assert_eq!(high_arr[src_tail + i], array[dst_tail + i]);
            }
        }
    }
}

#[test]
fn repacked_payloads_round_trip_through_delta_codec() {
    // The delta codec and the repacker must agree on the lower-tier layout: a delta built from
    // two repacked frames has to reconstruct the second exactly.
    let mut rng = TestRng::new(778);
    let a = S::make_realistic_payload(BitQuality::High, &mut rng);
    let mut b = a.clone();
    b[0] ^= 0xFF; // move position
    S::flip_bone(&mut b, BitQuality::High, 4);

    let high_a = LocalAvatarSyncMessage { array: Some(a), data_quality_level: BitQuality::High as u8, ..Default::default() };
    let high_b = LocalAvatarSyncMessage { array: Some(b), data_quality_level: BitQuality::High as u8, ..Default::default() };
    let (med_a, low_a, vlow_a) = AvatarQualityRepacker::build_all_lower_from_high(&high_a).unwrap();
    let (med_b, low_b, vlow_b) = AvatarQualityRepacker::build_all_lower_from_high(&high_b).unwrap();

    S::assert_round_trip(med_a.array.as_deref().unwrap(), med_b.array.as_deref().unwrap(), BitQuality::Medium);
    S::assert_round_trip(low_a.array.as_deref().unwrap(), low_b.array.as_deref().unwrap(), BitQuality::Low);
    S::assert_round_trip(vlow_a.array.as_deref().unwrap(), vlow_b.array.as_deref().unwrap(), BitQuality::VeryLow);
}

#[test]
fn a_missing_or_short_high_payload_is_an_error_not_a_panic() {
    let mut med = LocalAvatarSyncMessage::default();
    let mut low = LocalAvatarSyncMessage::default();
    let mut vlow = LocalAvatarSyncMessage::default();
    let missing = LocalAvatarSyncMessage::default();
    assert!(AvatarQualityRepacker::build_all_lower_from_high_into(&missing, &mut med, &mut low, &mut vlow).is_err());
    let short = LocalAvatarSyncMessage { array: Some(vec![0u8; 10]), data_quality_level: 3, ..Default::default() };
    let err = AvatarQualityRepacker::build_all_lower_from_high_into(&short, &mut med, &mut low, &mut vlow).expect_err("short payload");
    assert!(err.report().contains("too small"), "{}", err.report());
    assert!(AvatarQualityRepacker::build_all_lower_from_high(&short).is_err());
}
