//! Voice is queued apart from bulk avatar state. `SaturatedBulkQueue_DoesNotShedVoice` and
//! `VoiceOnPriorityQueue_ArrivesIntact` drive the per-peer unreliable queues of the LiteNetLib
//! transport — the one the legacy clients are served by — exactly as the C# drove LiteNetLib's;
//! the rest pins the classification of which channels are voice data and the population-scaled
//! depth of the priority queue.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use basis_network_core::BasisNetworkCommons;
use basis_network_core::configuration::{BasisPopulationScale, LNLTransportConfig};
use basis_network_core::transport::basis_network_shell::{ConnectionRequest, EventBasedNetListener, NetManager, NetPeerRef};
use basis_network_core::transport::lnl_network_impl::{LnlNetManager, LnlSettings};
use basis_network_core::{NetDataReader, NetDataWriter};
use serial_test::serial;

const CONNECT_KEY: &str = "voice-priority";
const BULK_BOUND: i32 = 64;

fn wait_for(condition: impl Fn() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    condition()
}

/// The C# `Manager(listener, bulkBound)`: a LiteNetLib manager with a bounded bulk queue and
/// the Basis priority channel map.
fn manager(listener: Arc<EventBasedNetListener>, bulk_bound: i32) -> LnlNetManager {
    let mut settings = LnlSettings::from_config(&LNLTransportConfig::default(), true);
    settings.update_time_ms = 5;
    settings.mtu_discovery = false;
    settings.max_unreliable_queue_per_peer = bulk_bound;
    settings.max_priority_unreliable_queue_per_peer = 256;
    settings.disconnect_timeout_ms = 60_000.0;
    LnlNetManager::with_settings(listener, settings)
}

fn payload(seed: usize, length: usize) -> Vec<u8> {
    (0..length).map(|i| (seed * 31 + i * 7) as u8).collect()
}

/// A connected sender/receiver pair; the receiver accepts the key and counts voice.
struct Pair {
    sender: LnlNetManager,
    receiver: LnlNetManager,
    peer: NetPeerRef,
    voice_received: Arc<AtomicUsize>,
    corrupt: Arc<AtomicUsize>,
}

fn connect(bulk_bound: i32, expected_voice: Vec<u8>) -> Pair {
    let sender_listener = EventBasedNetListener::new();
    let receiver_listener = EventBasedNetListener::new();
    let voice_received = Arc::new(AtomicUsize::new(0));
    let corrupt = Arc::new(AtomicUsize::new(0));
    receiver_listener.connection_request_event.subscribe(Arc::new(|request: Arc<dyn ConnectionRequest>| {
        if request.data().get_string().unwrap_or_default() == CONNECT_KEY {
            request.accept().expect("accept");
        } else {
            request.reject(&NetDataWriter::new()).expect("reject");
        }
    }));
    {
        let (voice_received, corrupt) = (voice_received.clone(), corrupt.clone());
        receiver_listener.network_receive_event.subscribe(Arc::new(move |_, mut reader: NetDataReader, channel, _| {
            if channel == BasisNetworkCommons::VOICE_CHANNEL {
                if reader.get_remaining_bytes() == expected_voice {
                    voice_received.fetch_add(1, Ordering::Relaxed);
                } else {
                    corrupt.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    let sender = manager(sender_listener, bulk_bound);
    let receiver = manager(receiver_listener, bulk_bound);
    receiver.start(IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST), 0).unwrap_or_else(|e| panic!("{}", e.report()));
    sender.start(IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST), 0).unwrap_or_else(|e| panic!("{}", e.report()));
    let mut key = NetDataWriter::new();
    key.put_string(CONNECT_KEY).unwrap();
    let peer = sender.connect("127.0.0.1", receiver.local_port(), &key).unwrap_or_else(|e| panic!("{}", e.report()));
    assert!(wait_for(|| sender.connected_peers_count() == 1 && peer.is_connected(), Duration::from_secs(10)), "peer never connected");
    Pair { sender, receiver, peer, voice_received, corrupt }
}

/// Voice must survive a saturated bulk queue: the old shared queue shed roughly every second
/// voice packet whenever position updates overflowed it.
#[test]
#[serial(lnl_transport)]
fn saturated_bulk_queue_does_not_shed_voice() {
    const ROUNDS: usize = 10;
    const VOICE_PER_ROUND: usize = 3;
    const VOICE_SENT: usize = ROUNDS * VOICE_PER_ROUND;
    let voice = payload(2, 96);
    let pair = connect(BULK_BOUND, voice.clone());
    let bulk = payload(1, 220);
    let avatar_channel = BasisNetworkCommons::get_player_avatar_channel_for_quality(3, false);

    // Interleaved, and in paced rounds. Interleaved because the old shared queue only destroyed
    // voice that was already sitting in it when an overflow ran, so voice has to be present
    // throughout. Paced because the point is to overrun the SENDER's queue bound (300 per round
    // against a bound of 64 does that many times over) without also overrunning the loopback
    // receive buffer, which would confuse ordinary UDP loss with the shedding under test.
    for _ in 0..ROUNDS {
        for i in 0..300 {
            pair.peer.send_unreliable_raw_merge(&bulk, 0, bulk.len(), avatar_channel, -1, 0).unwrap();
            if i % (300 / VOICE_PER_ROUND) == 0 {
                pair.peer.send_unreliable_raw_merge(&voice, 0, voice.len(), BasisNetworkCommons::VOICE_CHANNEL, -1, 0).unwrap();
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(pair.sender.unreliable_dropped() > 0, "bulk queue never overflowed, so this run proves nothing about what overflow costs voice");
    // Tolerance covers incidental loopback loss only. A broken build lands near 50%.
    assert!(
        wait_for(|| pair.voice_received.load(Ordering::Relaxed) >= VOICE_SENT * 9 / 10, Duration::from_secs(10)),
        "only {}/{VOICE_SENT} voice packets survived a saturated bulk queue ({} bulk drops) — voice is being shed with bulk traffic again",
        pair.voice_received.load(Ordering::Relaxed),
        pair.sender.unreliable_dropped()
    );
    assert_eq!(pair.sender.priority_unreliable_dropped(), 0);
    pair.sender.stop();
    pair.receiver.stop();
}

/// Voice that rides the priority queue arrives whole: the queue is a different path through the
/// merger, and a byte wrong there would be audio nobody could decode.
#[test]
#[serial(lnl_transport)]
fn voice_on_priority_queue_arrives_intact() {
    const VOICE_SENT: usize = 30;
    let voice = payload(7, 110);
    let pair = connect(8192, voice.clone());
    let bulk = payload(9, 200);
    for _ in 0..VOICE_SENT {
        pair.peer.send_unreliable_raw_merge(&voice, 0, voice.len(), BasisNetworkCommons::VOICE_CHANNEL, -1, 0).unwrap();
        for _ in 0..4 {
            pair.peer.send_unreliable_raw_merge(&bulk, 0, bulk.len(), BasisNetworkCommons::get_player_avatar_channel_for_quality(2, false), -1, 0).unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        wait_for(|| pair.voice_received.load(Ordering::Relaxed) == VOICE_SENT, Duration::from_secs(10)),
        "only {}/{VOICE_SENT} voice packets arrived",
        pair.voice_received.load(Ordering::Relaxed)
    );
    assert_eq!(pair.corrupt.load(Ordering::Relaxed), 0);
    assert_eq!(pair.sender.priority_unreliable_dropped(), 0);
    pair.sender.stop();
    pair.receiver.stop();
}

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
