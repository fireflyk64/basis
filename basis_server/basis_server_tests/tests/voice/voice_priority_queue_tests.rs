//! Voice is queued apart from bulk avatar state. The C# `SaturatedBulkQueue_DoesNotShedVoice` and
//! `VoiceOnPriorityQueue_ArrivesIntact` drove LiteNetLib's per-peer unreliable queues directly;
//! the iroh transport carries voice on its own QUIC stream and has no such queue to overrun, so
//! those two have no Rust counterpart. What remains pins the classification of which channels are
//! voice data and the population-scaled depth of the priority queue.

use basis_network_core::BasisNetworkCommons;
use basis_network_core::configuration::BasisPopulationScale;
use serial_test::serial;

/// Guards the classification itself. Voice DATA channels are priority; avatar state and the voice
/// control channels are not — a recipient list is low-rate and its newest message really does
/// supersede the last, so it belongs in the bulk queue.
#[test]
fn priority_channel_map_covers_exactly_the_voice_data_channels() {
    let map = BasisNetworkCommons::build_priority_unreliable_channel_map();
    assert_eq!(map.len(), usize::from(BasisNetworkCommons::TOTAL_CHANNELS));

    assert!(map[usize::from(BasisNetworkCommons::VOICE_CHANNEL)]);
    assert!(map[usize::from(BasisNetworkCommons::SHOUT_VOICE_CHANNEL)]);
    assert!(map[usize::from(BasisNetworkCommons::VOICE_LARGE_CHANNEL)]);

    assert!(!map[usize::from(BasisNetworkCommons::AUDIO_RECIPIENTS_CHANNEL)]);
    assert!(!map[usize::from(BasisNetworkCommons::AUDIO_RECIPIENTS_LARGE_CHANNEL)]);
    assert!(!map[usize::from(BasisNetworkCommons::AUDIO_RECIPIENTS_INVERTED_CHANNEL)]);
    assert!(!map[usize::from(BasisNetworkCommons::AUDIO_RECIPIENTS_BITFIELD_CHANNEL)]);
    assert!(!map[usize::from(BasisNetworkCommons::DELTA_AVATAR_CHANNEL)]);
    assert!(!map[usize::from(BasisNetworkCommons::AUTH_IDENTITY_CHANNEL)]);

    for quality in 0..4 {
        assert!(!map[usize::from(BasisNetworkCommons::get_player_avatar_channel_for_quality(quality, false))]);
        assert!(!map[usize::from(BasisNetworkCommons::get_player_avatar_channel_for_quality(quality, true))]);
    }

    // Exactly three, so an accidental blanket "everything is priority" fails here rather than
    // quietly reinstating one queue under two names.
    assert_eq!(map.iter().filter(|x| **x).count(), 3);
}

/// The sizing lesson, pinned so it cannot be undone by someone reasoning about voice as a single
/// conversation again. This queue shipped once at a flat 256 and measured 32.8% voice delivery
/// at 1000 clients, against 93.6% with a population-scaled bound. It is also asserted DEEPER
/// than the bulk bound: bulk depth buys avatar frames the next frame supersedes, voice depth buys
/// audio with no replacement.
#[test]
#[serial(population_scale)]
fn priority_queue_bound_scales_with_population_and_outranks_the_bulk_bound() {
    const GB: i64 = 1024 * 1024 * 1024;
    BasisPopulationScale::override_available_memory_for_tests(64 * GB);

    let voice_at_1000 = BasisPopulationScale::priority_queue_per_peer(0, 1000);
    let bulk_at_1000 = BasisPopulationScale::unreliable_queue_per_peer(0, 1000);

    assert!(voice_at_1000 >= 4096, "voice bound {voice_at_1000} at 1000 players is in the range that measured 32.8% delivery");
    assert!(voice_at_1000 >= bulk_at_1000, "voice bound {voice_at_1000} must not be shallower than the bulk bound {bulk_at_1000} — bulk is the traffic that should shed first");

    // Deeper per peer as the crowd thins, never unbounded.
    assert!(BasisPopulationScale::priority_queue_per_peer(0, 100) >= voice_at_1000);
    let at_8000 = BasisPopulationScale::priority_queue_per_peer(0, 8000);
    assert!((BasisPopulationScale::MIN_PRIORITY_QUEUE_PER_PEER..=BasisPopulationScale::MAX_PRIORITY_QUEUE_PER_PEER).contains(&at_8000));

    // A pinned value still wins, so an operator can reproduce a measurement.
    assert_eq!(BasisPopulationScale::priority_queue_per_peer(777, 1000), 777);

    // Even a small box keeps enough depth to carry a crowd's fan-in.
    BasisPopulationScale::override_available_memory_for_tests(8 * GB);
    assert!(BasisPopulationScale::priority_queue_per_peer(0, 2000) >= BasisPopulationScale::MIN_PRIORITY_QUEUE_PER_PEER);

    BasisPopulationScale::override_available_memory_for_tests(0);
}
