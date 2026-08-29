//! The reciprocal table replaces a hardware divide in the repacker's innermost operation, so
//! "close enough" is not a thing it can be. The width pairs are drawn from the wire layout and
//! every input is a bitfield of a known width, so the entire input domain is enumerated.

use basis_network_core::compression::{BasisBoneRotationCompression, BitQuality};
use basis_network_server::reduction::QuantRescaleTable;

#[test]
fn reciprocal_matches_division_across_every_width_pair_and_every_input() {
    for b_src in 1..=QuantRescaleTable::MAX_BITS {
        for b_dst in 1..=QuantRescaleTable::MAX_BITS {
            if b_src == b_dst {
                continue;
            }
            let max_src = (1u32 << b_src) - 1;
            for q in 0..=max_src {
                let expected = QuantRescaleTable::rescale_exact(q, b_src, b_dst);
                let actual = QuantRescaleTable::rescale(q, b_src, b_dst);
                assert_eq!(expected, actual, "{b_src}->{b_dst} bits, q={q}");
            }
        }
    }
}

/// Every width pair the wire layout can actually ask for must have taken the multiply path.
#[test]
fn every_width_pair_the_layout_uses_has_a_reciprocal() {
    fn assert_pair(b_src: usize, b_dst: usize) {
        if b_src == b_dst {
            return; // identity short-circuits before the table
        }
        assert!(QuantRescaleTable::has_reciprocal(b_src, b_dst), "{b_src}->{b_dst} bits fell back to a divide; the repacker uses this pair every frame");
    }

    let high_bpc = &BasisBoneRotationCompression::BPC_HIGH;
    for quality in [BitQuality::VeryLow, BitQuality::Low, BitQuality::Medium] {
        let dst_bpc = BasisBoneRotationCompression::get_bpc_table(quality);
        for slot in 0..BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT {
            if BasisBoneRotationCompression::BONE_DOF[slot] != 3 {
                continue;
            }
            assert_pair(high_bpc[slot] as usize, dst_bpc[slot] as usize);
        }
        assert_pair(BasisBoneRotationCompression::curl_bits(BitQuality::High) as usize, BasisBoneRotationCompression::curl_bits(quality) as usize);
        assert_pair(BasisBoneRotationCompression::splay_bits(BitQuality::High) as usize, BasisBoneRotationCompression::splay_bits(quality) as usize);
        assert_pair(BasisBoneRotationCompression::hinge_bits(BitQuality::High) as usize, BasisBoneRotationCompression::hinge_bits(quality) as usize);
        assert_pair(BasisBoneRotationCompression::twist_bits(BitQuality::High) as usize, BasisBoneRotationCompression::twist_bits(quality) as usize);
        assert_pair(BasisBoneRotationCompression::single_axis_bits(BitQuality::High) as usize, BasisBoneRotationCompression::single_axis_bits(quality) as usize);
    }
}

/// Inputs wider than their stated width are outside the 32-bit arithmetic's safe range and must
/// route to the exact path rather than wrapping.
#[test]
fn out_of_domain_inputs_still_match_the_exact_result() {
    for q in [0xFFFFu32, 0x1_0000, 0x00FF_FFFF, u32::MAX / 2, u32::MAX] {
        for b_src in 4..=13 {
            for b_dst in 4..=13 {
                if b_src == b_dst {
                    continue;
                }
                assert_eq!(QuantRescaleTable::rescale_exact(q, b_src, b_dst), QuantRescaleTable::rescale(q, b_src, b_dst), "q={q} {b_src}->{b_dst}");
            }
        }
    }
}

/// Boundary values keep their meaning: zero stays zero and full scale stays full scale.
#[test]
fn endpoints_are_preserved() {
    for b_src in 1..=16 {
        for b_dst in 1..=16 {
            if b_src == b_dst {
                continue;
            }
            assert_eq!(QuantRescaleTable::rescale(0, b_src, b_dst), 0);
            assert_eq!(QuantRescaleTable::rescale((1u32 << b_src) - 1, b_src, b_dst), (1u32 << b_dst) - 1);
        }
    }
}
