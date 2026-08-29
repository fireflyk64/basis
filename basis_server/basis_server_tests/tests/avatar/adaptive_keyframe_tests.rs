//! Adaptive keyframe stretch (v42): a streak of small High deltas doubles the periodic keyframe
//! interval up to AvatarDeltaKeyframeMaxIntervalMs; any large delta snaps it back to the base.

use basis_network_server::reduction::{BasisServerReductionSystemEvents, SenderWork};
use serial_test::serial;

struct Guard {
    base: i32,
    max: i32,
}

impl Guard {
    fn new() -> Self {
        let g = Self { base: BasisServerReductionSystemEvents::avatar_delta_keyframe_interval_ms(), max: BasisServerReductionSystemEvents::avatar_delta_keyframe_max_interval_ms() };
        BasisServerReductionSystemEvents::set_avatar_delta_keyframe_interval_ms(500);
        BasisServerReductionSystemEvents::set_avatar_delta_keyframe_max_interval_ms(2000);
        g
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        BasisServerReductionSystemEvents::set_avatar_delta_keyframe_interval_ms(self.base);
        BasisServerReductionSystemEvents::set_avatar_delta_keyframe_max_interval_ms(self.max);
    }
}

#[test]
#[serial(reduction_statics)]
fn effective_interval_doubles_per_shift_and_clamps_to_max() {
    let _g = Guard::new();
    assert_eq!(BasisServerReductionSystemEvents::effective_keyframe_interval_ms(0), 500);
    assert_eq!(BasisServerReductionSystemEvents::effective_keyframe_interval_ms(1), 1000);
    assert_eq!(BasisServerReductionSystemEvents::effective_keyframe_interval_ms(2), 2000);
    assert_eq!(BasisServerReductionSystemEvents::effective_keyframe_interval_ms(3), 2000);
    assert_eq!(BasisServerReductionSystemEvents::effective_keyframe_interval_ms(60), 2000);
}

#[test]
#[serial(reduction_statics)]
fn effective_interval_max_at_or_below_base_disables_stretch() {
    let _g = Guard::new();
    BasisServerReductionSystemEvents::set_avatar_delta_keyframe_max_interval_ms(0);
    assert_eq!(BasisServerReductionSystemEvents::effective_keyframe_interval_ms(5), 500);
    BasisServerReductionSystemEvents::set_avatar_delta_keyframe_max_interval_ms(500);
    assert_eq!(BasisServerReductionSystemEvents::effective_keyframe_interval_ms(5), 500);
}

#[test]
#[serial(reduction_statics)]
fn small_delta_streak_stretches_after_four_big_delta_resets() {
    let _g = Guard::new();
    let mut sender = SenderWork::default();

    // Three small deltas: not yet stretched.
    for _ in 0..3 {
        BasisServerReductionSystemEvents::test_only_update_keyframe_stretch(&mut sender, 20);
    }
    assert_eq!(sender.keyframe_stretch_shift, 0);

    // Fourth completes the streak.
    BasisServerReductionSystemEvents::test_only_update_keyframe_stretch(&mut sender, 20);
    assert_eq!(sender.keyframe_stretch_shift, 1);

    // Four more: next stretch step.
    for _ in 0..4 {
        BasisServerReductionSystemEvents::test_only_update_keyframe_stretch(&mut sender, 20);
    }
    assert_eq!(sender.keyframe_stretch_shift, 2);

    // At the cap (2000ms at shift 2), further small deltas stop accumulating.
    for _ in 0..16 {
        BasisServerReductionSystemEvents::test_only_update_keyframe_stretch(&mut sender, 20);
    }
    assert_eq!(sender.keyframe_stretch_shift, 2);
    assert_eq!(BasisServerReductionSystemEvents::effective_keyframe_interval_ms(sender.keyframe_stretch_shift), 2000);

    // A single big delta (motion) snaps everything back.
    BasisServerReductionSystemEvents::test_only_update_keyframe_stretch(&mut sender, 200);
    assert_eq!(sender.keyframe_stretch_shift, 0);
    assert_eq!(sender.small_delta_streak, 0);
    assert_eq!(BasisServerReductionSystemEvents::effective_keyframe_interval_ms(sender.keyframe_stretch_shift), 500);
}
