//! Image/GIF bandwidth control, both directions: an upload token bucket per sender the server
//! enforces on relayed image traffic, and the paced replay of cached images to an arriving player.
//! Mutates the server configuration, a process-wide static, so the fixture saves and restores it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use basis_network_core::configuration::Configuration;
use basis_network_server::NetworkServer;
use basis_network_server::networking::{BasisImageBandwidthGovernor, PendingPayload};
use basis_network_core::transport::basis_network_shell::NetPeer;
use basis_server_tests::support::FakePeer;
use parking_lot::Mutex;
use serial_test::serial;

struct Fixture {
    previous: Option<Arc<Configuration>>,
}

impl Fixture {
    fn new() -> Self {
        let previous = NetworkServer::configuration();
        NetworkServer::set_configuration(Configuration::default());
        BasisImageBandwidthGovernor::reset();
        // Drive the pump by hand: left on, the background thread drains the same queue underneath
        // these tests and a rate assertion becomes a race against a 25 ms timer.
        BasisImageBandwidthGovernor::set_auto_pump(false);
        Self { previous }
    }

    fn configure(&self, edit: impl FnOnce(&mut Configuration)) {
        NetworkServer::update_configuration(edit);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        BasisImageBandwidthGovernor::reset();
        match self.previous.take() {
            Some(previous) => NetworkServer::set_configuration((*previous).clone()),
            None => NetworkServer::clear_configuration(),
        }
    }
}

fn payloads(count: usize, size: usize) -> Vec<PendingPayload> {
    (0..count).map(|_| PendingPayload::new(7, vec![0u8; size])).collect()
}

// ── Upload: the server-side floor under the client's own pacing ──

#[test]
#[serial(network_statics)]
fn sender_inside_its_budget_is_never_dropped() {
    let f = Fixture::new();
    f.configure(|c| c.image_share_egress_megabits_per_second = 200);
    // Well under one burst of budget, which is what an honest client looks like.
    for _ in 0..50 {
        assert!(BasisImageBandwidthGovernor::try_consume_egress(1, 16 * 1024), "a sender inside its budget must never be dropped");
    }
    assert_eq!(BasisImageBandwidthGovernor::dropped_messages(), 0);
}

#[test]
#[serial(network_statics)]
fn sender_that_ignores_the_budget_is_eventually_dropped() {
    let f = Fixture::new();
    f.configure(|c| {
        c.image_share_egress_megabits_per_second = 1; // 125 KB/s
        c.image_share_egress_enforcement_percent = 100;
    });
    // The bucket is allowed to go negative on any single charge, so what is asserted is that
    // sustained overrun stops, not that a particular call fails.
    let mut dropped = false;
    for _ in 0..200 {
        if !BasisImageBandwidthGovernor::try_consume_egress(1, 64 * 1024) {
            dropped = true;
            break;
        }
    }
    assert!(dropped, "a sender well past its budget must be cut off");
    assert!(BasisImageBandwidthGovernor::dropped_messages() > 0);
    assert!(BasisImageBandwidthGovernor::dropped_bytes() > 0);
}

#[test]
#[serial(network_statics)]
fn fan_out_is_what_costs_not_payload_size() {
    let f = Fixture::new();
    f.configure(|c| {
        c.image_share_egress_megabits_per_second = 1;
        c.image_share_egress_enforcement_percent = 100;
    });
    // One chunk to forty peers is forty times the egress of the same chunk to one.
    const CHUNK: i64 = 16 * 1024;
    let mut wide_accepted = 0;
    while wide_accepted < 40 && BasisImageBandwidthGovernor::try_consume_egress(1, CHUNK * 40) {
        wide_accepted += 1;
    }

    BasisImageBandwidthGovernor::reset();
    f.configure(|c| {
        c.image_share_egress_megabits_per_second = 1;
        c.image_share_egress_enforcement_percent = 100;
    });
    let mut narrow_accepted = 0;
    while narrow_accepted < 40 && BasisImageBandwidthGovernor::try_consume_egress(2, CHUNK) {
        narrow_accepted += 1;
    }
    assert!(narrow_accepted > wide_accepted, "a narrow fan-out must get further on the same budget; wide={wide_accepted} narrow={narrow_accepted}");
}

#[test]
#[serial(network_statics)]
fn enforcement_headroom_lets_an_honest_client_overshoot_slightly() {
    let f = Fixture::new();
    f.configure(|c| {
        c.image_share_egress_megabits_per_second = 1;
        c.image_share_egress_enforcement_percent = 300;
    });
    let mut accepted = 0;
    while accepted < 200 && BasisImageBandwidthGovernor::try_consume_egress(1, 16 * 1024) {
        accepted += 1;
    }

    BasisImageBandwidthGovernor::reset();
    f.configure(|c| {
        c.image_share_egress_megabits_per_second = 1;
        c.image_share_egress_enforcement_percent = 100;
    });
    let mut accepted_tight = 0;
    while accepted_tight < 200 && BasisImageBandwidthGovernor::try_consume_egress(2, 16 * 1024) {
        accepted_tight += 1;
    }
    assert!(accepted > accepted_tight, "headroom must buy real slack; 300%={accepted} 100%={accepted_tight}");
}

#[test]
#[serial(network_statics)]
fn zero_upload_budget_disables_enforcement_rather_than_blocking_everything() {
    let f = Fixture::new();
    f.configure(|c| c.image_share_egress_megabits_per_second = 0);
    for _ in 0..500 {
        assert!(BasisImageBandwidthGovernor::try_consume_egress(1, 1024 * 1024));
    }
    assert_eq!(BasisImageBandwidthGovernor::dropped_messages(), 0);
}

#[test]
#[serial(network_statics)]
fn each_sender_gets_its_own_bucket() {
    let f = Fixture::new();
    f.configure(|c| {
        c.image_share_egress_megabits_per_second = 1;
        c.image_share_egress_enforcement_percent = 100;
    });
    while BasisImageBandwidthGovernor::try_consume_egress(1, 64 * 1024) {}
    // One sharer exhausting itself must not stop anybody else sharing.
    assert!(BasisImageBandwidthGovernor::try_consume_egress(2, 64 * 1024));
}

// ── Download: paced cache replay ──

#[test]
#[serial(network_statics)]
fn zero_download_rate_leaves_replay_to_the_caller() {
    let f = Fixture::new();
    f.configure(|c| c.image_share_download_megabits_per_second = 0);
    let peer = FakePeer::new(9);
    assert!(!BasisImageBandwidthGovernor::enqueue_replay(&peer.as_ref(), payloads(4, 1024)), "0 means unpaced, so the caller sends inline as it always did");
}

#[test]
#[serial(network_statics)]
fn paced_replay_delivers_every_payload_in_order() {
    let f = Fixture::new();
    f.configure(|c| c.image_share_download_megabits_per_second = 200);
    let peer = FakePeer::new(9);
    let sent: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = sent.clone();
    BasisImageBandwidthGovernor::set_send_payload(Some(Arc::new(move |_, _, payload| sink.lock().push(payload.to_vec()))));

    let batch = payloads(8, 1024);
    assert!(BasisImageBandwidthGovernor::enqueue_replay(&peer.as_ref(), batch));
    BasisImageBandwidthGovernor::pump_once_for_tests();

    // A 200 Mb/s bucket carries far more than 8 KB in its initial burst, so one pass drains it.
    assert_eq!(sent.lock().len(), 8);
    assert!(!BasisImageBandwidthGovernor::has_pending_replay(peer.id()));
    BasisImageBandwidthGovernor::set_send_payload(None);
}

#[test]
#[serial(network_statics)]
fn paced_replay_stops_at_the_rate_instead_of_sending_everything() {
    let f = Fixture::new();
    f.configure(|c| c.image_share_download_megabits_per_second = 1);
    let peer = FakePeer::new(9);
    let sent = Arc::new(AtomicUsize::new(0));
    let counter = sent.clone();
    BasisImageBandwidthGovernor::set_send_payload(Some(Arc::new(move |_, _, _| {
        counter.fetch_add(1, Ordering::Relaxed);
    })));

    let batch = payloads(400, 64 * 1024); // 25 MB of cache
    assert!(BasisImageBandwidthGovernor::enqueue_replay(&peer.as_ref(), batch));
    BasisImageBandwidthGovernor::pump_once_for_tests();

    let delivered = sent.load(Ordering::Relaxed);
    assert!(delivered < 400, "a 1 Mb/s replay must not deliver 25 MB in one pass; sent {delivered}/400");
    assert!(BasisImageBandwidthGovernor::has_pending_replay(peer.id()), "the remainder must stay queued for later passes rather than being dropped");
    BasisImageBandwidthGovernor::set_send_payload(None);
}

#[test]
#[serial(network_statics)]
fn departed_peer_drops_its_queued_replay() {
    let f = Fixture::new();
    f.configure(|c| c.image_share_download_megabits_per_second = 1);
    let peer = FakePeer::new(9);
    BasisImageBandwidthGovernor::set_send_payload(Some(Arc::new(|_, _, _| {})));
    assert!(BasisImageBandwidthGovernor::enqueue_replay(&peer.as_ref(), payloads(400, 64 * 1024)));
    BasisImageBandwidthGovernor::remove_peer(peer.id());
    assert!(!BasisImageBandwidthGovernor::has_pending_replay(peer.id()));
    BasisImageBandwidthGovernor::set_send_payload(None);
}
