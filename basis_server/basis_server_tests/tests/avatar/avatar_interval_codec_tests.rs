//! Piecewise avatar-interval byte codec (v42): bytes 0..199 keep the legacy base+b meaning,
//! bytes 200..255 step 12 ms so distant receivers can pace below the old 3.3 Hz floor.

use basis_network_core::BasisNetworkCommons;

const BASE: i32 = 50;

#[test]
fn legacy_region_is_exact_and_unchanged() {
    for ms in BASE..BASE + i32::from(BasisNetworkCommons::AVATAR_INTERVAL_EXTENDED_START) {
        let b = BasisNetworkCommons::encode_avatar_interval_byte(ms, BASE);
        assert_eq!(i32::from(b), ms - BASE);
        assert_eq!(BasisNetworkCommons::decode_avatar_interval_ms(b, BASE), ms);
    }
}

#[test]
fn every_byte_round_trips_through_decode_then_encode() {
    for b in 0..=u8::MAX {
        let ms = BasisNetworkCommons::decode_avatar_interval_ms(b, BASE);
        assert_eq!(BasisNetworkCommons::encode_avatar_interval_byte(ms, BASE), b);
    }
}

#[test]
fn decode_is_strictly_monotonic_and_caps_under_receiver_window_clamp() {
    let mut prev = -1;
    for b in 0..=u8::MAX {
        let ms = BasisNetworkCommons::decode_avatar_interval_ms(b, BASE);
        assert!(ms > prev, "byte {b} not monotonic: {ms} <= {prev}");
        prev = ms;
    }
    // Max encodable interval stays under the receiver's 1 s interpolation-window clamp.
    assert_eq!(prev, BASE + 200 + 55 * 12);
    assert!(prev < 1000);
}

#[test]
fn extended_region_quantizes_within_half_step() {
    let mut ms = BASE + 200;
    while ms <= BASE + 200 + 55 * 12 {
        let b = BasisNetworkCommons::encode_avatar_interval_byte(ms, BASE);
        let back = BasisNetworkCommons::decode_avatar_interval_ms(b, BASE);
        assert!((back - ms).abs() <= BasisNetworkCommons::AVATAR_INTERVAL_EXTENDED_STEP_MS / 2 + 1, "{ms} -> byte {b} -> {back}");
        ms += 7;
    }
}

#[test]
fn out_of_range_clamps() {
    assert_eq!(BasisNetworkCommons::encode_avatar_interval_byte(0, BASE), 0);
    assert_eq!(BasisNetworkCommons::encode_avatar_interval_byte(BASE, BASE), 0);
    assert_eq!(BasisNetworkCommons::encode_avatar_interval_byte(BASE + 5000, BASE), u8::MAX);
    assert_eq!(BasisNetworkCommons::encode_avatar_interval_byte(i32::MAX - BASE, BASE), u8::MAX);
}
