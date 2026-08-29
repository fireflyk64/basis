//! Tests for the core wire primitives: smallest-three quaternion compression and its bitstream,
//! the ranged ushort float codec, MathExtensions, packet sequence validation, and the raw
//! position read/write extensions. Avatar bit-packing/delta codecs are covered elsewhere.

use basis_network_core::compression::{BasisAvatarBitPacking, BasisBoneRotationCompression, BasisNetworkCompressionExtensions, BasisRangedUshortFloatData, BitQuality};
use basis_network_core::mathematics::math_extensions::{MathExtensions, Quaternion, Vector3};
use basis_network_core::protocol::basis_packet_util::BasisPacketUtil;
use basis_server_tests::support::DeltaTestSupport;
use basis_server_tests::support::delta_test_support::TestRng;

type Q = (f32, f32, f32, f32);

// ── Smallest-three quaternion compression ──

const S2: f32 = 0.70710678; // sin/cos 45°
const S225: f32 = 0.38268343; // sin 22.5°
const C225: f32 = 0.92387953; // cos 22.5°
const S60: f32 = 0.8660254; // sin 60°

fn normalize(x: f32, y: f32, z: f32, w: f32) -> Q {
    let len = (x * x + y * y + z * z + w * w).sqrt();
    (x / len, y / len, z / len, w / len)
}

fn canonical_quats() -> Vec<Q> {
    vec![
        (0.0, 0.0, 0.0, 1.0),
        (0.0, 0.0, 0.0, -1.0),
        (1.0, 0.0, 0.0, 0.0),
        (0.0, 1.0, 0.0, 0.0),
        (0.0, 0.0, 1.0, 0.0),
        (-1.0, 0.0, 0.0, 0.0),
        (0.0, -1.0, 0.0, 0.0),
        (0.0, 0.0, -1.0, 0.0),
        (S2, 0.0, 0.0, S2),
        (0.0, S2, 0.0, S2),
        (0.0, 0.0, S2, S2),
        (-S2, 0.0, 0.0, S2),
        (S2, 0.0, 0.0, -S2),
        (S2, -S2, 0.0, 0.0),
        (S225, 0.0, 0.0, C225),
        (0.0, -S225, 0.0, C225),
        (S60, 0.0, 0.0, 0.5),
        (0.5, 0.5, 0.5, 0.5),
        (-0.5, -0.5, -0.5, -0.5),
        (0.5, -0.5, 0.5, -0.5),
        normalize(0.7072, 0.7070, 0.0, 0.0),
        normalize(0.01, -0.01, 0.01, 0.9999),
        normalize(0.577, 0.577, 0.577, 0.05),
    ]
}

fn one_minus_abs_dot(a: Q, bx: f32, by: f32, bz: f32, bw: f32) -> f64 {
    let dot = a.0 as f64 * bx as f64 + a.1 as f64 * by as f64 + a.2 as f64 * bz as f64 + a.3 as f64 * bw as f64;
    1.0 - dot.abs()
}

/// Worst-case 1-|dot| bound for quantization at half-step h, with float slop.
fn tolerance(bpc: u32, max_range: f32) -> f64 {
    let h = max_range as f64 / ((1u32 << bpc) - 1) as f64;
    12.0 * h * h + 2e-6
}

fn assert_encode_decode_close(q: Q, bpc: u32, max_range: f32) {
    let packed = BasisBoneRotationCompression::encode_smallest_three(q.0, q.1, q.2, q.3, bpc, max_range);
    let (dx, dy, dz, dw) = BasisBoneRotationCompression::decode_smallest_three(packed, bpc, max_range);
    let norm = (dx as f64 * dx as f64 + dy as f64 * dy as f64 + dz as f64 * dz as f64 + dw as f64 * dw as f64).sqrt();
    assert!((norm - 1.0).abs() < 1e-3, "decoded quaternion not unit length: {norm}");
    let err = one_minus_abs_dot(q, dx, dy, dz, dw);
    let tol = tolerance(bpc, max_range);
    assert!(err <= tol, "bpc={bpc} q={q:?} err={err} tol={tol}");
}

#[test]
fn smallest_three_round_trip_canonical_quaternions() {
    for bpc in [4, 5, 6, 8, 10, 12] {
        for q in canonical_quats() {
            assert_encode_decode_close(q, bpc, BasisBoneRotationCompression::INV_SQRT2);
        }
    }
}

#[test]
fn smallest_three_round_trip_random_sweep() {
    for bpc in [4u32, 5, 6, 8, 10, 12] {
        let mut rng = TestRng::new(9000 + bpc as u64);
        for _ in 0..400 {
            let q = DeltaTestSupport::random_quat(&mut rng);
            assert_encode_decode_close(q, bpc, BasisBoneRotationCompression::INV_SQRT2);
        }
    }
}

#[test]
fn smallest_three_hemisphere_equivalence_packed_bits_identical() {
    let mut rng = TestRng::new(777);
    for bpc in [5u32, 8, 12] {
        for q in canonical_quats() {
            let a = BasisBoneRotationCompression::encode_smallest_three(q.0, q.1, q.2, q.3, bpc, BasisBoneRotationCompression::INV_SQRT2);
            let b = BasisBoneRotationCompression::encode_smallest_three(-q.0, -q.1, -q.2, -q.3, bpc, BasisBoneRotationCompression::INV_SQRT2);
            assert_eq!(a, b);
        }
        for _ in 0..200 {
            let (x, y, z, w) = DeltaTestSupport::random_quat(&mut rng);
            let a = BasisBoneRotationCompression::encode_smallest_three(x, y, z, w, bpc, BasisBoneRotationCompression::INV_SQRT2);
            let b = BasisBoneRotationCompression::encode_smallest_three(-x, -y, -z, -w, bpc, BasisBoneRotationCompression::INV_SQRT2);
            assert_eq!(a, b);
        }
    }
}

#[test]
fn smallest_three_restricted_range_round_trips_small_rotations() {
    const BPC: u32 = 8;
    const MAX_RANGE: f32 = 0.5;
    let axes = [(1.0f32, 0.0f32, 0.0f32), (0.0, 1.0, 0.0), (0.0, 0.0, 1.0), (0.57735, 0.57735, 0.57735), (S2, -S2, 0.0), (0.0, 0.6, -0.8)];
    let mut deg = 5;
    while deg <= 50 {
        let half = deg as f32 * std::f32::consts::PI / 360.0;
        let (s, c) = (half.sin(), half.cos());
        for (ax, ay, az) in axes {
            assert_encode_decode_close((ax * s, ay * s, az * s, c), BPC, MAX_RANGE);
        }
        deg += 5;
    }
}

#[test]
fn smallest_three_out_of_range_components_clamp_to_max_range() {
    // 90° about X has non-dropped magnitude 0.7071, beyond the 0.5 range: the encoder clamps, so
    // the decode lands on the nearest representable pose (60° about X).
    const BPC: u32 = 10;
    const MAX_RANGE: f32 = 0.5;
    let packed = BasisBoneRotationCompression::encode_smallest_three(S2, 0.0, 0.0, S2, BPC, MAX_RANGE);
    let (x, y, z, w) = BasisBoneRotationCompression::decode_smallest_three(packed, BPC, MAX_RANGE);
    assert!((x - S60).abs() <= 1e-3, "x={x}");
    assert!((w - 0.5).abs() <= 1e-3, "w={w}");
    assert!(y.abs() <= 5e-3, "y={y}");
    assert!(z.abs() <= 5e-3, "z={z}");
    let norm = (x as f64 * x as f64 + y as f64 * y as f64 + z as f64 * z as f64 + w as f64 * w as f64).sqrt();
    assert!((norm - 1.0).abs() < 1e-3);
}

// ── Bitstream ──

fn width_mask(width: u32) -> u64 {
    if width == 64 { u64::MAX } else { (1u64 << width) - 1 }
}

#[test]
fn write_bits_read_bits_random_variable_width_fields_round_trip() {
    let mut rng = TestRng::new(4242);
    const FIELD_COUNT: usize = 300;
    let mut widths = vec![0u32; FIELD_COUNT];
    let mut values = vec![0u64; FIELD_COUNT];
    let mut buffer = vec![0u8; 3000];

    let mut bit_pos = 0usize;
    for i in 0..FIELD_COUNT {
        widths[i] = 1 + rng.next(64) as u32;
        values[i] = rng.next_u64() & width_mask(widths[i]);
        BasisBoneRotationCompression::write_bits(&mut buffer, bit_pos, values[i], widths[i]);
        bit_pos += widths[i] as usize;
    }
    assert!(bit_pos <= buffer.len() * 8);

    let mut read_pos = 0usize;
    for i in 0..FIELD_COUNT {
        let got = BasisBoneRotationCompression::read_bits(&buffer, &mut read_pos, widths[i]);
        assert_eq!(values[i], got, "field {i}");
    }
    assert_eq!(bit_pos, read_pos);
}

#[test]
fn write_bits_is_lsb_first_and_leaves_neighbors_untouched() {
    let mut buf = [0u8; 4];
    BasisBoneRotationCompression::write_bits(&mut buf, 6, 0b101, 3);
    assert_eq!(buf, [0x40, 0x01, 0x00, 0x00]);
    let mut pos = 6;
    assert_eq!(BasisBoneRotationCompression::read_bits(&buf, &mut pos, 3), 0b101);
    assert_eq!(pos, 9);

    let mut wide = [0u8; 16];
    BasisBoneRotationCompression::write_bits(&mut wide, 5, u64::MAX, 64);
    assert_eq!(wide[0], 0xE0);
    for &b in &wide[1..=7] {
        assert_eq!(b, 0xFF);
    }
    assert_eq!(wide[8], 0x1F);
    for &b in &wide[9..] {
        assert_eq!(b, 0x00);
    }
    let mut wide_pos = 5;
    assert_eq!(BasisBoneRotationCompression::read_bits(&wide, &mut wide_pos, 64), u64::MAX);
    assert_eq!(wide_pos, 69);
}

// ── Bone tables and packet sizing ──

#[test]
fn rotation_field_offsets_are_contiguous_and_match_rotation_bytes_for_all_qualities() {
    for q in BitQuality::ALL {
        let widths = BasisBoneRotationCompression::build_rotation_field_widths(q);
        assert_eq!(widths.len(), BasisBoneRotationCompression::ROTATION_FIELD_COUNT);

        let mut offsets = vec![0usize; BasisBoneRotationCompression::ROTATION_FIELD_COUNT];
        let total_bits = BasisBoneRotationCompression::build_rotation_field_offsets(q, &mut offsets);

        // Offsets must tile the region exactly: no gaps, no overlap.
        let mut expected = 0usize;
        for i in 0..widths.len() {
            assert_eq!(expected, offsets[i]);
            expected += widths[i] as usize;
        }
        assert_eq!(expected, total_bits);
        assert_eq!(total_bits, BasisBoneRotationCompression::rotation_bits(q));
        assert_eq!((total_bits + 7) >> 3, BasisBoneRotationCompression::rotation_bytes(q));

        // The explicit bone slots come first, then one field per finger channel.
        for slot in 0..BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT {
            let expected_width = match BasisBoneRotationCompression::BONE_DOF[slot] {
                3 => 2 + 3 * BasisBoneRotationCompression::get_bpc_table(q)[slot] as u32,
                2 => BasisBoneRotationCompression::hinge_bits(q) + BasisBoneRotationCompression::twist_bits(q),
                _ => BasisBoneRotationCompression::single_axis_bits(q),
            };
            assert_eq!(expected_width, widths[slot]);
        }
        for f in 0..BasisBoneRotationCompression::FINGER_CHANNEL_COUNT {
            assert_eq!(BasisBoneRotationCompression::finger_field_width(q), widths[BasisBoneRotationCompression::WIRE_BONE_SLOT_COUNT + f]);
        }
    }
}

#[test]
fn bone_order_tables_are_consistent_inverses() {
    let order = &BasisBoneRotationCompression::BONE_WRITE_ORDER;
    let to_slot = BasisBoneRotationCompression::bone_to_slot();

    assert_eq!(order.len(), BasisBoneRotationCompression::SYNC_BONE_COUNT);
    assert_eq!(order.len(), 51);
    assert_eq!(to_slot.len(), 55);

    let expected_bones: Vec<i32> = (1..=54).filter(|v| *v != 21 && *v != 22 && *v != 23).collect();
    let mut sorted = order.to_vec();
    sorted.sort_unstable();
    assert_eq!(expected_bones, sorted);

    for (slot, bone) in order.iter().enumerate() {
        assert_eq!(slot as i32, to_slot[*bone as usize]);
    }
    assert_eq!(to_slot[0], -1);
    assert_eq!(to_slot[21], -1);
    assert_eq!(to_slot[22], -1);
    assert_eq!(to_slot[23], -1);
}

#[test]
fn quality_tables_lengths_and_ranges_are_valid() {
    for table in [&BasisBoneRotationCompression::BPC_HIGH, &BasisBoneRotationCompression::BPC_MEDIUM, &BasisBoneRotationCompression::BPC_LOW, &BasisBoneRotationCompression::BPC_VERY_LOW] {
        assert_eq!(table.len(), BasisBoneRotationCompression::SYNC_BONE_COUNT);
        assert!(table.iter().all(|b| (2..=12).contains(b)));
    }
    let max_comp = &BasisBoneRotationCompression::MAX_COMPONENT;
    assert_eq!(max_comp.len(), BasisBoneRotationCompression::SYNC_BONE_COUNT);
    assert!(max_comp.iter().all(|m| (1e-3..=BasisBoneRotationCompression::INV_SQRT2 + 1e-6).contains(m)));

    // The C# asserted reference identity; the tables are consts here, so the lookup is pinned by value.
    assert_eq!(BasisBoneRotationCompression::get_bpc_table(BitQuality::High), &BasisBoneRotationCompression::BPC_HIGH);
    assert_eq!(BasisBoneRotationCompression::get_bpc_table(BitQuality::Medium), &BasisBoneRotationCompression::BPC_MEDIUM);
    assert_eq!(BasisBoneRotationCompression::get_bpc_table(BitQuality::Low), &BasisBoneRotationCompression::BPC_LOW);
    assert_eq!(BasisBoneRotationCompression::get_bpc_table(BitQuality::VeryLow), &BasisBoneRotationCompression::BPC_VERY_LOW);
}

#[test]
fn packet_sizes_are_pinned_wire_compatibility() {
    // Current v52 wire sizes (restricted-DOF bone encoding); a change here is a protocol break and
    // must be deliberate.
    assert_eq!(BasisBoneRotationCompression::rotation_bytes(BitQuality::VeryLow), 44);
    assert_eq!(BasisBoneRotationCompression::rotation_bytes(BitQuality::Low), 53);
    assert_eq!(BasisBoneRotationCompression::rotation_bytes(BitQuality::Medium), 67);
    assert_eq!(BasisBoneRotationCompression::rotation_bytes(BitQuality::High), 94);

    assert_eq!(BasisAvatarBitPacking::WRITE_POSITION, 9);
    assert_eq!(BasisAvatarBitPacking::TAIL_BYTES, 21);
    assert_eq!(BasisAvatarBitPacking::WRITE_HIPS_DELTA, 5);
    for q in BitQuality::ALL {
        assert_eq!(BasisAvatarBitPacking::position_bytes(q), BasisAvatarBitPacking::WRITE_POSITION);
    }

    assert_eq!(BasisBoneRotationCompression::end_effector_bytes(BitQuality::VeryLow), 0);
    assert_eq!(BasisBoneRotationCompression::end_effector_bytes(BitQuality::Low), 0);
    assert_eq!(BasisBoneRotationCompression::end_effector_bytes(BitQuality::Medium), 0);
    assert_eq!(BasisBoneRotationCompression::end_effector_bytes(BitQuality::High), BasisBoneRotationCompression::END_EFFECTOR_BLOCK_BYTES);

    for q in BitQuality::ALL {
        let expected = BasisAvatarBitPacking::position_bytes(q) + BasisBoneRotationCompression::rotation_bytes(q) + BasisBoneRotationCompression::TAIL_BYTES + BasisBoneRotationCompression::end_effector_bytes(q);
        assert_eq!(expected, BasisBoneRotationCompression::convert_to_size(q));
    }
    assert_eq!(BasisBoneRotationCompression::convert_to_size(BitQuality::VeryLow), 74);
    assert_eq!(BasisBoneRotationCompression::convert_to_size(BitQuality::Low), 83);
    assert_eq!(BasisBoneRotationCompression::convert_to_size(BitQuality::Medium), 97);
    assert_eq!(BasisBoneRotationCompression::convert_to_size(BitQuality::High), 159);
}

// ── BasisRangedUshortFloatData ──

#[test]
fn ranged_float_round_trips_within_half_precision() {
    for (min, max, precision) in [(-1.0f32, 1.0f32, 0.001f32), (0.0, 1.0, 0.01), (-3.1415927, 3.1415927, 0.001), (0.0, 10.0, 1.0), (-50.0, 50.0, 0.1)] {
        let codec = BasisRangedUshortFloatData::new(min, max, precision);
        let tol = 0.5 * precision + 0.011 * precision;
        for i in 0..=1000 {
            let v = min + (max - min) * (i as f32 / 1000.0);
            let compressed = codec.compress(v);
            assert!(compressed <= codec.mask);
            let back = codec.decompress(compressed);
            assert!(back >= min && back <= max, "decompressed {back} escaped [{min},{max}]");
            assert!((back - v).abs() <= tol, "v={v} back={back} tol={tol}");
        }
        assert_eq!(codec.decompress(codec.compress(min)), min);
    }
}

#[test]
fn ranged_float_out_of_range_inputs_clamp_to_bounds() {
    let codec = BasisRangedUshortFloatData::new(-1.0, 1.0, 0.001);
    assert_eq!(codec.compress(-1.0), codec.compress(-100.0));
    assert_eq!(codec.compress(1.0), codec.compress(100.0));
    assert_eq!(codec.compress(f32::NEG_INFINITY), 0);
    assert_eq!(codec.compress(1.0), codec.compress(f32::INFINITY));

    assert_eq!(codec.decompress(0), -1.0);
    assert!(codec.decompress(codec.mask) <= 1.0);
    assert!(codec.decompress(u16::MAX) <= 1.0);
    let mut prev = codec.decompress(0);
    for code in 1..100u16 {
        let cur = codec.decompress(code);
        assert!(cur > prev, "decompress should be monotonic over in-range codes");
        prev = cur;
    }
}

#[test]
fn ranged_float_required_bits_and_mask_pinned() {
    for (min, max, precision, bits, mask) in [(-1.0f32, 1.0f32, 0.001f32, 11, 2047u16), (0.0, 1.0, 0.01, 7, 127), (0.0, 10.0, 1.0, 4, 15), (0.0, 16.0, 1.0, 5, 31)] {
        let codec = BasisRangedUshortFloatData::new(min, max, precision);
        assert_eq!(codec.required_bits, bits);
        assert_eq!(codec.mask, mask);
    }
}

#[test]
fn fast_log2_pins() {
    for (value, expected) in [(0u32, 0), (1, 0), (2, 1), (3, 1), (4, 2), (7, 2), (8, 3), (255, 7), (256, 8), (1023, 9), (1024, 10), (65535, 15), (65536, 16), (2147483648, 31), (4294967295, 31)] {
        assert_eq!(BasisRangedUshortFloatData::fast_log2(value), expected, "value {value}");
    }
}

// ── MathExtensions and support structs ──

#[test]
fn clamp_float_edges() {
    assert_eq!(MathExtensions::clamp_f32(5.0, 0.0, 10.0), 5.0);
    assert_eq!(MathExtensions::clamp_f32(-5.0, 0.0, 10.0), 0.0);
    assert_eq!(MathExtensions::clamp_f32(15.0, 0.0, 10.0), 10.0);
    assert_eq!(MathExtensions::clamp_f32(0.0, 0.0, 10.0), 0.0);
    assert_eq!(MathExtensions::clamp_f32(10.0, 0.0, 10.0), 10.0);
    assert_eq!(MathExtensions::clamp_f32(3.0, 7.0, 7.0), 7.0);
    assert_eq!(MathExtensions::clamp_f32(f32::INFINITY, 0.0, 10.0), 10.0);
    assert_eq!(MathExtensions::clamp_f32(f32::NEG_INFINITY, 0.0, 10.0), 0.0);
    assert!(MathExtensions::clamp_f32(f32::NAN, 0.0, 10.0).is_nan());
    assert_eq!(MathExtensions::clamp_f32(-2.0, -3.0, -1.0), -2.0);
}

#[test]
fn clamp_int_edges() {
    assert_eq!(MathExtensions::clamp_i32(5, 0, 10), 5);
    assert_eq!(MathExtensions::clamp_i32(-5, 0, 10), 0);
    assert_eq!(MathExtensions::clamp_i32(15, 0, 10), 10);
    assert_eq!(MathExtensions::clamp_i32(0, 0, 10), 0);
    assert_eq!(MathExtensions::clamp_i32(10, 0, 10), 10);
    assert_eq!(MathExtensions::clamp_i32(i32::MIN, 7, 7), 7);
    assert_eq!(MathExtensions::clamp_i32(i32::MAX, i32::MIN, i32::MAX), i32::MAX);
    assert_eq!(MathExtensions::clamp_i32(i32::MAX, -3, -1), -1);
}

#[test]
fn clamp_double_edges() {
    assert_eq!(MathExtensions::clamp_f64(5.0, 0.0, 10.0), 5.0);
    assert_eq!(MathExtensions::clamp_f64(-5.0, 0.0, 10.0), 0.0);
    assert_eq!(MathExtensions::clamp_f64(15.0, 0.0, 10.0), 10.0);
    assert_eq!(MathExtensions::clamp_f64(f64::INFINITY, 0.0, 10.0), 10.0);
    assert_eq!(MathExtensions::clamp_f64(f64::NEG_INFINITY, 0.0, 10.0), 0.0);
    assert!(MathExtensions::clamp_f64(f64::NAN, 0.0, 10.0).is_nan());
}

#[test]
fn vector3_operators_and_squared_magnitude() {
    let a = Vector3::new(1.0, 2.0, 3.0);
    let b = Vector3::new(-4.0, 0.5, 10.0);
    let sum = a + b;
    assert_eq!((sum.x, sum.y, sum.z), (-3.0, 2.5, 13.0));
    let diff = a - b;
    assert_eq!((diff.x, diff.y, diff.z), (5.0, 1.5, -7.0));
    assert_eq!(a.squared_magnitude(), 14.0);
    assert_eq!(Vector3::new(0.0, 0.0, 0.0).squared_magnitude(), 0.0);
}

#[test]
fn quaternion_constructor_sets_components() {
    let q = Quaternion::new(0.1, -0.2, 0.3, -0.4);
    assert_eq!(q.value.x, 0.1);
    assert_eq!(q.value.y, -0.2);
    assert_eq!(q.value.z, 0.3);
    assert_eq!(q.value.w, -0.4);
}

// ── BasisPacketUtil sequence validation ──

#[test]
fn is_newer_half_window_semantics_with_wraparound() {
    assert!(BasisPacketUtil::is_newer(5, 4));
    assert!(!BasisPacketUtil::is_newer(4, 5));
    assert!(BasisPacketUtil::is_newer(0, 255));
    assert!(!BasisPacketUtil::is_newer(255, 0));
    assert!(BasisPacketUtil::is_newer(127, 0));
    assert!(!BasisPacketUtil::is_newer(128, 0));
    assert!(BasisPacketUtil::is_newer(1, 200));
    assert!(!BasisPacketUtil::is_newer(200, 1));
    // Exactly opposite sequence numbers are mutually "not newer".
    assert!(!BasisPacketUtil::is_newer(0, 128));
    assert!(!BasisPacketUtil::is_newer(129, 1));
    assert!(!BasisPacketUtil::is_newer(1, 129));
    // Equal sequences count as "newer" here; validate_packet adds the inequality check.
    assert!(BasisPacketUtil::is_newer(42, 42));
    assert!(!BasisPacketUtil::validate_packet(42, 42));
}

#[test]
fn validate_packet_exhaustive_matches_half_window_model() {
    for old_seq in 0..=255u8 {
        for new_seq in 0..=255u8 {
            let delta = new_seq.wrapping_sub(old_seq);
            let expected = (1..=127).contains(&delta);
            assert_eq!(expected, BasisPacketUtil::validate_packet(new_seq, old_seq), "new {new_seq} old {old_seq}");
        }
    }
}

// ── BasisNetworkCompressionExtensions ──

fn assert_close(expected: f32, actual: f32, tolerance: f32) {
    assert!((expected - actual).abs() <= tolerance, "expected {expected} ± {tolerance}, got {actual}");
}

#[test]
fn write_position_read_position_round_trip_at_offset_zero() {
    let pos_bytes = BasisAvatarBitPacking::WRITE_POSITION;
    let mut buffer = vec![0u8; pos_bytes];
    let mut offset = 0usize;
    let pos = Vector3::new(1.5, -2.25, 3.75);
    assert!(BasisNetworkCompressionExtensions::write_position(pos, &mut buffer, &mut offset));
    assert_eq!(offset, pos_bytes);

    // int24 millimetres: exact for these values, and never worse than half a millimetre.
    let back = BasisNetworkCompressionExtensions::read_position(&buffer).unwrap();
    assert_close(pos.x, back.x, 0.0006);
    assert_close(pos.y, back.y, 0.0006);
    assert_close(pos.z, back.z, 0.0006);
}

#[test]
fn write_position_advances_offset_read_position_always_reads_from_start() {
    let pos_bytes = BasisAvatarBitPacking::WRITE_POSITION;
    let mut buffer = vec![0u8; pos_bytes * 2];
    let mut offset = 0usize;
    let first = Vector3::new(10.0, 20.0, 30.0);
    let second = Vector3::new(-1.0, -2.0, -3.0);
    assert!(BasisNetworkCompressionExtensions::write_position(first, &mut buffer, &mut offset));
    assert_eq!(offset, pos_bytes);
    assert!(BasisNetworkCompressionExtensions::write_position(second, &mut buffer, &mut offset));
    assert_eq!(offset, pos_bytes * 2);

    assert_close(second.x, BasisAvatarBitPacking::decode_axis_mm(&buffer, pos_bytes).unwrap(), 0.0006);
    assert_close(second.y, BasisAvatarBitPacking::decode_axis_mm(&buffer, pos_bytes + 3).unwrap(), 0.0006);
    assert_close(second.z, BasisAvatarBitPacking::decode_axis_mm(&buffer, pos_bytes + 6).unwrap(), 0.0006);

    // read_position has no offset parameter: it always decodes the vector at buffer start.
    let back = BasisNetworkCompressionExtensions::read_position(&buffer).unwrap();
    assert_close(first.x, back.x, 0.0006);
    assert_close(first.y, back.y, 0.0006);
    assert_close(first.z, back.z, 0.0006);
}

#[test]
fn write_position_clamps_non_finite_instead_of_wrapping() {
    let mut buffer = vec![0u8; BasisAvatarBitPacking::WRITE_POSITION];
    let mut offset = 0usize;
    let pos = Vector3::new(-0.0, f32::NAN, f32::INFINITY);
    assert!(BasisNetworkCompressionExtensions::write_position(pos, &mut buffer, &mut offset));
    let back = BasisNetworkCompressionExtensions::read_position(&buffer).unwrap();
    assert_eq!(back.x, 0.0);
    assert_eq!(back.y, 0.0);
    assert_close(8388.607, back.z, 0.001);

    // A buffer that cannot hold a position is refused rather than written past.
    let mut short = vec![0u8; 4];
    let mut off = 0usize;
    assert!(!BasisNetworkCompressionExtensions::write_position(pos, &mut short, &mut off));
    assert_eq!(off, 0);
    assert!(BasisNetworkCompressionExtensions::read_position(&short).is_none());
}
