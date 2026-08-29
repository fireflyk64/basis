//! Port of `HelloWorldPeerStressTests.cs`.
//!
//! Sustained traffic over BOTH paths a Basis client has to a peer at once: the server relay, and
//! a direct peer-to-peer link the server only introduces. The direct path has the most moving
//! parts — signalling on the P2P channel, an X25519 key exchange relayed by the server, the
//! endpoint introduction, the dial/accept, and finally the offload handshake after which the
//! server stops relaying between the pair.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use basis_hello_world_client::{HelloPeerClient, HelloTransport};
use basis_server_tests::support::{HelloWorldServerFixture, read_scale, wait_until};
use serial_test::serial;

/// Default population. Eight is enough for four independent pairs plus non-partners to exercise
/// the fallback against.
const DEFAULT_CLIENT_COUNT: usize = 8;
/// Messages per client per path. The run sends 2 x ClientCount x Rounds in total.
const DEFAULT_ROUNDS: usize = 25;
/// Pacing between rounds, so an unpaced loop does not just measure the outgoing queues.
const ROUND_PAUSE: Duration = Duration::from_millis(4);

const JOIN_TIMEOUT: Duration = Duration::from_secs(45);
const LINK_TIMEOUT: Duration = Duration::from_secs(60);
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(90);

/// Counters rather than a log of messages, so the scale can be turned up without the collector
/// becoming the thing that runs out of memory first.
#[derive(Default)]
struct Tally {
    direct_count: AtomicI64,
    direct_sum: AtomicI64,
    relay_count: AtomicI64,
    relay_sum: AtomicI64,
    fallback_count: AtomicI64,
    misrouted_fallbacks: AtomicI64, // a fallback that somehow took a direct link
    wrong_sender: AtomicI64,
}

#[test]
#[serial(network_statics)]
fn peer_clients_sustain_traffic_over_direct_links_and_the_server_at_once() {
    let client_count = read_scale("BASIS_HELLO_STRESS_CLIENTS", DEFAULT_CLIENT_COUNT, 4);
    let rounds = read_scale("BASIS_HELLO_STRESS_ROUNDS", DEFAULT_ROUNDS, 1);
    assert!(client_count.is_multiple_of(2), "the pairing below needs an even population");

    let server = HelloWorldServerFixture::new();
    let mut clients: Vec<Arc<HelloPeerClient>> = Vec::with_capacity(client_count);
    for i in 0..client_count {
        let client = HelloPeerClient::new(&format!("Peer{i:02X}")).unwrap_or_else(|e| panic!("{}", e.report()));
        let joined = client.connect(server.connection_string(), 0, HelloWorldServerFixture::PASSWORD, JOIN_TIMEOUT).unwrap_or_else(|e| panic!("{}", e.report()));
        assert!(joined, "peer client {i} did not join the server");
        clients.push(client);
    }

    let ids: Vec<u16> = clients.iter().map(|c| c.player_id()).collect();
    let mut distinct = ids.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), client_count);

    // Partner = the other half of an adjacent pair; across = a peer we deliberately never link
    // to, so its traffic has to go through the server; fallback = a third peer, also unlinked,
    // used to prove a "direct" send still lands when there is no link.
    let partner = move |i: usize| if i.is_multiple_of(2) { i + 1 } else { i - 1 };
    let across = move |i: usize| (i + client_count / 2) % client_count;
    let fallback_target = move |i: usize| (i + 3) % client_count;

    let tally: Vec<Arc<Tally>> = (0..client_count).map(|_| Arc::new(Tally::default())).collect();
    for i in 0..client_count {
        let t = tally[i].clone();
        let ids = ids.clone();
        clients[i].base().on_number_received(Arc::new(move |sender, value, transport| {
            if transport == HelloTransport::DirectLink {
                if sender != ids[partner(i)] {
                    t.wrong_sender.fetch_add(1, Ordering::Relaxed);
                }
                t.direct_count.fetch_add(1, Ordering::Relaxed);
                t.direct_sum.fetch_add(value as i64, Ordering::Relaxed);
            } else {
                if sender != ids[across(i)] {
                    t.wrong_sender.fetch_add(1, Ordering::Relaxed);
                }
                t.relay_count.fetch_add(1, Ordering::Relaxed);
                t.relay_sum.fetch_add(value as i64, Ordering::Relaxed);
            }
        }));
        let t = tally[i].clone();
        clients[i].base().on_text_received(Arc::new(move |_, _, transport| {
            t.fallback_count.fetch_add(1, Ordering::Relaxed);
            if transport != HelloTransport::ServerRelay {
                t.misrouted_fallbacks.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    // Only one side of each pair dials; the other accepts. The request/accept exchange has an
    // initiator, and having both sides initiate would just race two sessions for the same pair.
    for i in (0..client_count).step_by(2) {
        let up = clients[i].open_direct_link(ids[i + 1], LINK_TIMEOUT).unwrap_or_else(|e| panic!("{}", e.report()));
        assert!(up, "no direct link between peer {i} and peer {} within {}s", i + 1, LINK_TIMEOUT.as_secs());
    }

    // The initiator returns as soon as its own confirmation lands; the acceptor's arrives on its
    // own connection a moment later, so both halves are given the link timeout to agree.
    wait_until(
        || (0..client_count).all(|i| clients[i].has_direct_link(ids[partner(i)])),
        LINK_TIMEOUT,
        || {
            let missing: Vec<String> = (0..client_count).filter(|&i| !clients[i].has_direct_link(ids[partner(i)])).map(|i| format!("peer {i}")).collect();
            format!("no confirmed link to the partner for {}", missing.join(", "))
        },
    );
    for i in 0..client_count {
        assert!(!clients[i].has_direct_link(ids[across(i)]), "peer {i} unexpectedly linked to a non-partner");
    }
    println!("{client_count} peers joined; {} direct links up.", client_count / 2);

    // One unlinked "direct" send per client, before the bulk traffic, so the fallback is
    // measured while the direct links are live rather than in a quiet moment.
    for i in 0..client_count {
        clients[i].send_text_direct(ids[fallback_target(i)], &format!("fallback-from-{i:02X}")).unwrap_or_else(|e| panic!("{}", e.report()));
    }

    for round in 1..=rounds as i32 {
        for i in 0..client_count {
            clients[i].send_number_direct(ids[partner(i)], round).unwrap_or_else(|e| panic!("{}", e.report())); // over the direct link
            clients[i].base().send_number(ids[across(i)], round).unwrap_or_else(|e| panic!("{}", e.report())); // through the server
        }
        std::thread::sleep(ROUND_PAUSE);
    }

    let expected_sum = (rounds as i64) * (rounds as i64 + 1) / 2;
    wait_until(
        || tally.iter().all(|t| t.direct_count.load(Ordering::Relaxed) >= rounds as i64 && t.relay_count.load(Ordering::Relaxed) >= rounds as i64 && t.fallback_count.load(Ordering::Relaxed) >= 1),
        DELIVERY_TIMEOUT,
        || {
            let parts: Vec<String> = tally
                .iter()
                .enumerate()
                .map(|(i, t)| format!("p{i} direct={}/{rounds} relay={}/{rounds} fallback={}/1", t.direct_count.load(Ordering::Relaxed), t.relay_count.load(Ordering::Relaxed), t.fallback_count.load(Ordering::Relaxed)))
                .collect();
            format!("delivery stalled: {}", parts.join(", "))
        },
    );

    for (i, t) in tally.iter().enumerate() {
        assert_eq!(t.wrong_sender.load(Ordering::Relaxed), 0, "peer {i} heard from the wrong sender");
        assert_eq!(t.direct_count.load(Ordering::Relaxed), rounds as i64);
        assert_eq!(t.relay_count.load(Ordering::Relaxed), rounds as i64);
        assert_eq!(t.direct_sum.load(Ordering::Relaxed), expected_sum);
        assert_eq!(t.relay_sum.load(Ordering::Relaxed), expected_sum);
        // A "direct" send with no link must still arrive, and must arrive relayed.
        assert_eq!(t.fallback_count.load(Ordering::Relaxed), 1);
        assert_eq!(t.misrouted_fallbacks.load(Ordering::Relaxed), 0);
    }

    let messages = client_count * (2 * rounds + 1);
    println!("{messages} messages delivered ({} direct, {} relayed, {client_count} fallback).", client_count * rounds, client_count * rounds);

    for client in &clients {
        client.disconnect();
    }
}

/// A direct link to oneself is refused up front, and a link request before joining is an error.
#[test]
#[serial(network_statics)]
fn direct_link_misuse_is_reported_not_panicked() {
    let server = HelloWorldServerFixture::new();
    let lonely = HelloPeerClient::new("Lonely").unwrap();
    assert!(lonely.open_direct_link(1, Duration::from_millis(10)).is_err(), "not joined yet");
    assert!(lonely.connect(server.connection_string(), 0, HelloWorldServerFixture::PASSWORD, Duration::from_secs(20)).unwrap());
    let me = lonely.player_id();
    let err = lonely.open_direct_link(me, Duration::from_millis(10)).expect_err("a client cannot link to itself");
    assert!(err.report().contains("itself"), "{}", err.report());
    // A link to a player that does not exist never comes up, and says so without failing.
    assert!(!lonely.open_direct_link(me.wrapping_add(1000), Duration::from_millis(300)).unwrap());
    assert_eq!(lonely.direct_link_count(), 0);
    lonely.disconnect();
}
