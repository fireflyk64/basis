//! StripAdditionalDataAtLowQuality: Low/VeryLow tiers drop AdditionalAvatarData (face/behaviour
//! params are unreadable at those view distances); High/Medium keep it. Off = legacy copy-to-all.

use basis_network_core::SerializableBasis::{AdditionalAvatarData, LocalAvatarSyncMessage};
use basis_network_server::reduction::{BasisServerReductionSystemEvents, SenderWork};
use serial_test::serial;

struct StripGuard(bool);

impl StripGuard {
    fn new(value: bool) -> Self {
        let saved = BasisServerReductionSystemEvents::strip_additional_data_at_low_quality();
        BasisServerReductionSystemEvents::set_strip_additional_data_at_low_quality(value);
        Self(saved)
    }
}

impl Drop for StripGuard {
    fn drop(&mut self) {
        BasisServerReductionSystemEvents::set_strip_additional_data_at_low_quality(self.0);
    }
}

fn sender_with_high_additional() -> SenderWork {
    let mut sender = SenderWork::default();
    sender.avatar_high = LocalAvatarSyncMessage {
        additional_avatar_datas: Some(vec![AdditionalAvatarData { message_index: 2, payload_size: 3, array: Some(vec![1, 2, 3]) }]),
        additional_avatar_data_size: 1,
        linked_avatar_index: 7,
        ..Default::default()
    };
    sender
}

#[test]
#[serial(reduction_statics)]
fn strip_on_low_tiers_drop_additional_medium_keeps_it() {
    let _g = StripGuard::new(true);
    let mut sender = sender_with_high_additional();
    BasisServerReductionSystemEvents::test_only_propagate_additional_data(&mut sender);

    let high = &sender.avatar_high;
    assert_eq!(sender.avatar_medium.additional_avatar_datas, high.additional_avatar_datas);
    assert_eq!(sender.avatar_medium.additional_avatar_data_size, high.additional_avatar_data_size);
    assert!(sender.avatar_low.additional_avatar_datas.is_none());
    assert_eq!(sender.avatar_low.additional_avatar_data_size, 0);
    assert!(sender.avatar_very_low.additional_avatar_datas.is_none());
    assert_eq!(sender.avatar_very_low.additional_avatar_data_size, 0);
    assert_eq!(sender.avatar_medium.linked_avatar_index, high.linked_avatar_index);
    assert_eq!(sender.avatar_low.linked_avatar_index, high.linked_avatar_index);
    assert_eq!(sender.avatar_very_low.linked_avatar_index, high.linked_avatar_index);
}

#[test]
#[serial(reduction_statics)]
fn strip_off_all_tiers_keep_additional() {
    let _g = StripGuard::new(false);
    let mut sender = sender_with_high_additional();
    BasisServerReductionSystemEvents::test_only_propagate_additional_data(&mut sender);

    let high = &sender.avatar_high;
    assert_eq!(sender.avatar_medium.additional_avatar_datas, high.additional_avatar_datas);
    assert_eq!(sender.avatar_low.additional_avatar_datas, high.additional_avatar_datas);
    assert_eq!(sender.avatar_very_low.additional_avatar_datas, high.additional_avatar_datas);
    assert_eq!(sender.avatar_low.additional_avatar_data_size, high.additional_avatar_data_size);
    assert_eq!(sender.avatar_very_low.additional_avatar_data_size, high.additional_avatar_data_size);
}
