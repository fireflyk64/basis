//! The order a receiver's pending avatar sends are written to its peer.
//!
//! The unreliable queue discards from the front when it is over budget, so this order decides who
//! keeps moving on an overloaded server. These pin the two properties that guarantee it: channels
//! stay grouped (bundling needs the runs) and the rarest-updated tier is written last.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use basis_network_core::BasisNetworkCommons;
use basis_network_server::reduction::{BasisServerReductionSystemEvents, PendingAvatarSend, ReceiverData};

const SMALL_ID_CHANNELS: [u8; 4] = [
    BasisNetworkCommons::PLAYER_AVATAR_VERY_LOW_CHANNEL,
    BasisNetworkCommons::PLAYER_AVATAR_LOW_CHANNEL,
    BasisNetworkCommons::PLAYER_AVATAR_MEDIUM_CHANNEL,
    BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL,
];

const LARGE_ID_CHANNELS: [u8; 4] = [
    BasisNetworkCommons::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL,
    BasisNetworkCommons::PLAYER_AVATAR_LOW_LARGE_CHANNEL,
    BasisNetworkCommons::PLAYER_AVATAR_MEDIUM_LARGE_CHANNEL,
    BasisNetworkCommons::PLAYER_AVATAR_HIGH_LARGE_CHANNEL,
];

fn build_mixed_channels() -> Vec<u8> {
    let mut channels = Vec::new();
    for _ in 0..3 {
        for quality in (0..4).rev() {
            channels.push(SMALL_ID_CHANNELS[quality]);
            channels.push(LARGE_ID_CHANNELS[quality]);
            channels.push(SMALL_ID_CHANNELS[quality] + 1);
            channels.push(LARGE_ID_CHANNELS[quality] + 1);
        }
    }
    channels
}

fn sort(channels: &[u8]) -> Vec<PendingAvatarSend> {
    let count = channels.len();
    let pending: Vec<PendingAvatarSend> = channels.iter().enumerate().map(|(i, &channel)| PendingAvatarSend { source: Arc::from(vec![0u8; 4]), length: i + 1, channel, interval: 0, interval_offset: 1 }).collect();
    let mut recv = ReceiverData { pending_sends: pending, ..Default::default() };
    BasisServerReductionSystemEvents::test_only_sort_pending_by_channel(&mut recv, count, 0);
    recv.pending_sends.truncate(count);
    recv.pending_sends
}

fn is_tier(channel: u8, quality: usize) -> bool {
    channel == SMALL_ID_CHANNELS[quality] || channel == SMALL_ID_CHANNELS[quality] + 1 || channel == LARGE_ID_CHANNELS[quality] || channel == LARGE_ID_CHANNELS[quality] + 1
}

fn first_index_of_tier(sorted: &[PendingAvatarSend], quality: usize) -> Option<usize> {
    sorted.iter().position(|s| is_tier(s.channel, quality))
}

fn last_index_of_tier(sorted: &[PendingAvatarSend], quality: usize) -> Option<usize> {
    sorted.iter().rposition(|s| is_tier(s.channel, quality))
}

fn first_index_of(sorted: &[PendingAvatarSend], channel: u8) -> Option<usize> {
    sorted.iter().position(|s| s.channel == channel)
}

#[test]
fn rarest_tier_is_written_last() {
    let sorted = sort(&build_mixed_channels());
    for quality in 0..4 {
        for higher in quality + 1..4 {
            assert!(last_index_of_tier(&sorted, higher) < first_index_of_tier(&sorted, quality), "every tier-{higher} entry must be written before every tier-{quality} entry");
        }
    }
}

#[test]
fn id_width_does_not_decide_who_survives() {
    let sorted = sort(&build_mixed_channels());
    let count = sorted.len();
    for quality in 0..4 {
        let small = first_index_of(&sorted, SMALL_ID_CHANNELS[quality]).expect("small");
        let large = first_index_of(&sorted, LARGE_ID_CHANNELS[quality]).expect("large");
        let next_tier_start = if quality == 0 { count } else { first_index_of_tier(&sorted, quality - 1).expect("next tier") };
        assert!(small < next_tier_start && large < next_tier_start, "both id widths of a tier must stay inside that tier's run");
    }
}

#[test]
fn delta_frames_outlive_near_keyframes_but_not_distant_ones() {
    let mut channels = build_mixed_channels();
    for _ in 0..3 {
        channels.push(BasisNetworkCommons::DELTA_AVATAR_CHANNEL);
    }
    let sorted = sort(&channels);

    let first_delta = first_index_of(&sorted, BasisNetworkCommons::DELTA_AVATAR_CHANNEL).expect("delta");
    let last_delta = first_delta + 2;
    assert_eq!(sorted[last_delta].channel, BasisNetworkCommons::DELTA_AVATAR_CHANNEL);

    assert!(last_index_of_tier(&sorted, 3).expect("high") < first_delta, "High keyframes are cheaper to lose than a delta");
    assert!(last_delta < first_index_of_tier(&sorted, 2).expect("medium"), "a delta is cheaper to lose than a Medium keyframe");
}

#[test]
fn channels_stay_grouped_and_nothing_is_lost() {
    let channels = build_mixed_channels();
    let sorted = sort(&channels);

    let mut expected: HashMap<u8, usize> = HashMap::new();
    for &channel in &channels {
        *expected.entry(channel).or_default() += 1;
    }

    let mut actual: HashMap<u8, usize> = HashMap::new();
    let mut closed = HashSet::new();
    let mut current = sorted[0].channel;
    for entry in &sorted {
        *actual.entry(entry.channel).or_default() += 1;
        if entry.channel != current {
            assert!(closed.insert(current), "channel {current} appears in more than one run");
            current = entry.channel;
        }
    }
    assert_eq!(expected, actual);
}

#[test]
fn order_within_a_channel_is_preserved() {
    let sorted = sort(&build_mixed_channels());
    let mut last_seen: HashMap<u8, usize> = HashMap::new();
    for entry in &sorted {
        if let Some(previous) = last_seen.get(&entry.channel) {
            assert!(*previous < entry.length, "the sort must stay stable so the sender rotation still decides order within a channel");
        }
        last_seen.insert(entry.channel, entry.length);
    }
}
