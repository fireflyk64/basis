//! Round-trips the v50 grouped avatar bundle: real writer (build_raw_for_range) into the real
//! reader (`BasisAvatarBundleCodec::try_flatten`), asserting every entry comes back on its original
//! channel with its original bytes and this receiver's interval byte patched in. Ragged body
//! lengths on purpose: equal lengths would hide an indexing error in the un-transpose.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use basis_network_core::BasisNetworkCommons;
use basis_network_core::compression::BasisAvatarBundleCodec;
use basis_network_server::reduction::{BasisServerReductionSystemEvents, PendingAvatarSend, ReceiverData};

fn make(channel: u8, interval_offset: u8, interval: u8, body: Vec<u8>) -> PendingAvatarSend {
    PendingAvatarSend { length: body.len(), source: Arc::from(body), channel, interval, interval_offset }
}

/// Writer → reader, returning the flat entries the client would dispatch.
fn round_trip(pending: Vec<PendingAvatarSend>) -> Vec<(u8, Vec<u8>)> {
    let count = pending.len();
    let mut recv = ReceiverData { pending_sends: pending, ..Default::default() };
    BasisServerReductionSystemEvents::test_only_sort_pending_by_channel(&mut recv, count, 0);
    let raw_len = BasisServerReductionSystemEvents::test_only_build_raw_for_range(&mut recv, 0, count);

    let mut flat = vec![0u8; BasisAvatarBundleCodec::max_flat_size(raw_len)];
    let flat_len = BasisAvatarBundleCodec::try_flatten(&recv.bundle_raw_scratch[..raw_len], &mut flat).expect("try_flatten rejected output the writer just produced");

    let mut out = Vec::new();
    let mut offset = 0;
    while offset + 3 <= flat_len {
        let channel = flat[offset];
        let len = flat[offset + 1] as usize | ((flat[offset + 2] as usize) << 8);
        offset += 3;
        assert!(len > 0 && offset + len <= flat_len, "flat frame overran the buffer");
        out.push((channel, flat[offset..offset + len].to_vec()));
        offset += len;
    }
    assert_eq!(flat_len, offset);
    out
}

/// What the receiver must see: the source bytes with its own interval byte patched in.
fn patched(p: &PendingAvatarSend) -> Vec<u8> {
    let mut b = p.source.to_vec();
    b[p.interval_offset as usize] = p.interval;
    b
}

fn assert_round_trips(pending: Vec<PendingAvatarSend>) {
    // Snapshot before the sort reorders the list under us.
    let expected: Vec<(u8, Vec<u8>)> = pending.iter().map(|p| (p.channel, patched(p))).collect();
    let got = round_trip(pending);

    // Grouping reorders entries, so compare as multisets keyed by channel.
    assert_eq!(expected.len(), got.len());
    let mut by_channel: HashMap<u8, Vec<Vec<u8>>> = HashMap::new();
    for (ch, body) in &expected {
        by_channel.entry(*ch).or_default().push(body.clone());
    }
    for (ch, wants) in by_channel {
        let mine: Vec<&Vec<u8>> = got.iter().filter(|(c, _)| *c == ch).map(|(_, b)| b).collect();
        assert_eq!(wants.len(), mine.len(), "channel {ch}");
        for want in wants {
            assert!(mine.iter().any(|m| **m == want), "channel {ch}: no returned body matched {want:?}");
        }
    }
}

fn ramp(len: usize, seed: usize) -> Vec<u8> {
    (0..len).map(|i| (seed * 31 + i * 7) as u8).collect()
}

#[test]
fn fixed_size_quality_group_round_trips() {
    assert_round_trips(vec![
        make(BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL, 1, 40, ramp(12, 1)),
        make(BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL, 1, 41, ramp(12, 2)),
        make(BasisNetworkCommons::PLAYER_AVATAR_LOW_CHANNEL, 1, 42, ramp(8, 3)),
    ]);
}

#[test]
fn transposed_delta_group_with_ragged_lengths_round_trips() {
    assert_round_trips(vec![
        make(BasisNetworkCommons::DELTA_AVATAR_CHANNEL, 2, 50, ramp(5, 1)),
        make(BasisNetworkCommons::DELTA_AVATAR_CHANNEL, 2, 51, ramp(17, 2)),
        make(BasisNetworkCommons::DELTA_AVATAR_CHANNEL, 2, 52, ramp(3, 3)),
        make(BasisNetworkCommons::DELTA_AVATAR_CHANNEL, 3, 53, ramp(31, 4)),
    ]);
}

#[test]
fn mixed_channels_interleaved_on_arrival_round_trip() {
    assert_round_trips(vec![
        make(BasisNetworkCommons::DELTA_AVATAR_CHANNEL, 2, 60, ramp(9, 1)),
        make(BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL, 1, 61, ramp(14, 2)),
        make(BasisNetworkCommons::DELTA_AVATAR_CHANNEL, 2, 62, ramp(4, 3)),
        make(BasisNetworkCommons::PLAYER_AVATAR_VERY_LOW_LARGE_CHANNEL, 2, 63, ramp(11, 4)),
        make(BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL, 1, 64, ramp(14, 5)),
        make(BasisNetworkCommons::DELTA_AVATAR_CHANNEL, 3, 65, ramp(22, 6)),
        make(BasisNetworkCommons::PLAYER_AVATAR_HIGH_ADDITIONAL_CHANNEL, 1, 66, ramp(19, 7)),
    ]);
}

#[test]
fn entries_shorter_than_their_interval_offset_are_dropped() {
    // Length <= interval_offset means there is no room for the interval byte; the writer skips
    // these, and the group count must reflect that rather than counting them and desyncing.
    let got = round_trip(vec![
        make(BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL, 1, 70, ramp(10, 1)),
        make(BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL, 5, 71, ramp(3, 2)), // dropped
        make(BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL, 1, 72, ramp(10, 3)),
    ]);
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|(c, _)| *c == BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL));
}

#[test]
fn sort_is_stable_enough_that_every_channel_forms_one_run() {
    let mut recv = ReceiverData { pending_sends: vec![ make(BasisNetworkCommons::DELTA_AVATAR_CHANNEL, 2, 80, ramp(6, 1)), make(BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL, 1, 81, ramp(6, 2)), make(BasisNetworkCommons::DELTA_AVATAR_CHANNEL, 2, 82, ramp(6, 3)), make(BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL, 1, 83, ramp(6, 4)), ], ..Default::default() };
    BasisServerReductionSystemEvents::test_only_sort_pending_by_channel(&mut recv, 4, 0);

    // What bundling needs is that each channel forms ONE run, so a chunk charges the group
    // header once per channel rather than per entry.
    let mut seen = HashSet::new();
    for i in 0..recv.pending_sends.len() {
        if i > 0 && recv.pending_sends[i].channel == recv.pending_sends[i - 1].channel {
            continue;
        }
        assert!(seen.insert(recv.pending_sends[i].channel), "channel {} appears in more than one run", recv.pending_sends[i].channel);
    }
}

#[test]
fn truncated_group_is_rejected_rather_than_misparsed() {
    let mut recv = ReceiverData { pending_sends: vec![make(BasisNetworkCommons::DELTA_AVATAR_CHANNEL, 2, 90, ramp(12, 1)), make(BasisNetworkCommons::DELTA_AVATAR_CHANNEL, 2, 91, ramp(12, 2))], ..Default::default() };
    let raw_len = BasisServerReductionSystemEvents::test_only_build_raw_for_range(&mut recv, 0, 2);

    let mut flat = vec![0u8; BasisAvatarBundleCodec::max_flat_size(raw_len)];
    for cut in 1..raw_len {
        assert!(BasisAvatarBundleCodec::try_flatten(&recv.bundle_raw_scratch[..raw_len - cut], &mut flat).is_none(), "a body truncated by {cut} byte(s) was accepted");
    }
}
