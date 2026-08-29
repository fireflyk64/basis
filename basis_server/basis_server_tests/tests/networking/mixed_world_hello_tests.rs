//! The mixed world: legacy clients on the LiteNetLib protocol and new clients on iroh in one
//! room, on one server, with the server standing between them.
//!
//! Every client here is a real Basis client doing the full join — version check, password, DID
//! challenge, metadata — over its own transport; only the transport differs. The legacy ones
//! speak the wire protocol of the C# `LiteNetLib` byte for byte.

// The mesh and ring loops index two parallel arrays by position, as the C# tests do.
#![allow(clippy::needless_range_loop)]

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use basis_hello_world_client::{BasisHelloClient, HelloPeerClient, HelloTransport};
use basis_network_core::transport::LnlNetManager;
use basis_network_core::transport::basis_network_stack_registry::BasisNetworkStackRegistry;
use basis_network_core::transport::connection_target::ConnectionTarget;
use basis_network_core::transport::iroh_network_impl::IrohRuntime;
use basis_network_server::NetworkServer;
use basis_network_server::rest_api::BasisServerInfoQuery;
use basis_server_tests::support::{HelloWorldServerFixture, wait_until};
use parking_lot::Mutex;
use serial_test::serial;

const JOIN_TIMEOUT: Duration = Duration::from_secs(30);
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);
const LINK_TIMEOUT: Duration = Duration::from_secs(60);

fn legacy_client(server: &HelloWorldServerFixture, name: &str) -> Arc<BasisHelloClient> {
    let client = BasisHelloClient::with_stack(name, BasisNetworkStackRegistry::LITE_NET_LIB_ID).unwrap_or_else(|e| panic!("{}", e.report()));
    let joined = client.connect(&server.legacy_address(), 0, HelloWorldServerFixture::PASSWORD, JOIN_TIMEOUT).unwrap_or_else(|e| panic!("{}", e.report()));
    assert!(joined, "legacy client {name} did not join on {}", server.legacy_address());
    client
}

fn iroh_client(server: &HelloWorldServerFixture, name: &str) -> Arc<BasisHelloClient> {
    let client = BasisHelloClient::new(name).unwrap_or_else(|e| panic!("{}", e.report()));
    let joined = client.connect(server.connection_string(), 0, HelloWorldServerFixture::PASSWORD, JOIN_TIMEOUT).unwrap_or_else(|e| panic!("{}", e.report()));
    assert!(joined, "iroh client {name} did not join");
    client
}

#[test]
#[serial(network_statics)]
fn legacy_and_iroh_clients_share_one_world() {
    let server = HelloWorldServerFixture::new();
    // Alternating transports around the ring, so every hop crosses from one world to the other.
    let clients: Vec<Arc<BasisHelloClient>> = vec![
        legacy_client(&server, "Legacy0"),
        iroh_client(&server, "Iroh1"),
        legacy_client(&server, "Legacy2"),
        iroh_client(&server, "Iroh3"),
    ];
    let ids: Vec<u16> = clients.iter().map(|c| c.player_id()).collect();
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 4, "player ids must be unique across transports: {ids:?}");
    assert_eq!(NetworkServer::authenticated_peers().len(), 4);

    // Full mesh of directed text: every client hears from every other, whichever side it is on.
    type Inbox = Arc<Mutex<Vec<(u16, String)>>>;
    let inbox: Vec<Inbox> = (0..4).map(|_| Arc::new(Mutex::new(Vec::new()))).collect();
    for (i, client) in clients.iter().enumerate() {
        let box_i = inbox[i].clone();
        client.on_text_received(Arc::new(move |sender, text, transport| {
            assert_eq!(transport, HelloTransport::ServerRelay);
            box_i.lock().push((sender, text));
        }));
    }
    for from in 0..4 {
        for to in 0..4 {
            if from != to {
                clients[from].send_text(ids[to], &format!("hello{from}_{to}")).unwrap_or_else(|e| panic!("{}", e.report()));
            }
        }
    }
    wait_until(
        || inbox.iter().all(|b| b.lock().len() >= 3),
        DELIVERY_TIMEOUT,
        || format!("per client: {:?}", inbox.iter().map(|b| b.lock().len()).collect::<Vec<_>>()),
    );
    for to in 0..4 {
        let got = inbox[to].lock().clone();
        assert_eq!(got.len(), 3, "client {to} got a message meant for someone else");
        for from in 0..4 {
            if from != to {
                assert!(got.contains(&(ids[from], format!("hello{from}_{to}"))), "client {to} missing the text from {from}");
            }
        }
    }

    // The hello-world volley itself, round the ring twice.
    let hops: Arc<Mutex<Vec<(usize, u16, i32)>>> = Arc::new(Mutex::new(Vec::new()));
    let finished = Arc::new(Mutex::new(false));
    const FINAL: i32 = 8;
    for i in 0..4 {
        let me = clients[i].clone();
        let next_id = ids[(i + 1) % 4];
        let hops = hops.clone();
        let finished = finished.clone();
        clients[i].on_number_received(Arc::new(move |sender, value, _| {
            hops.lock().push((i, sender, value));
            if value >= FINAL {
                *finished.lock() = true;
            } else {
                me.send_number(next_id, value + 1).unwrap_or_else(|e| panic!("{}", e.report()));
            }
        }));
    }
    clients[0].send_number(ids[1], 1).unwrap();
    wait_until(|| *finished.lock(), DELIVERY_TIMEOUT, || format!("the volley stopped after {:?}", hops.lock()));
    let mut ordered = hops.lock().clone();
    ordered.sort_by_key(|h| h.2);
    assert_eq!(ordered.len(), FINAL as usize);
    for (hop, (receiver, sender, value)) in ordered.iter().enumerate() {
        assert_eq!(*value, hop as i32 + 1);
        assert_eq!(*receiver, (hop + 1) % 4);
        assert_eq!(*sender, ids[hop % 4]);
    }
    for client in &clients {
        client.disconnect();
    }
    wait_until(|| NetworkServer::authenticated_peers().is_empty(), DELIVERY_TIMEOUT, || format!("{} peers still authenticated", NetworkServer::authenticated_peers().len()));
}

#[test]
#[serial(network_statics)]
fn legacy_clients_are_never_offloaded_to_direct_links() {
    let server = HelloWorldServerFixture::new();
    let legacy = HelloPeerClient::with_stack("LegacyPeer", BasisNetworkStackRegistry::LITE_NET_LIB_ID).unwrap_or_else(|e| panic!("{}", e.report()));
    assert!(legacy.connect(&server.legacy_address(), 0, HelloWorldServerFixture::PASSWORD, JOIN_TIMEOUT).unwrap());
    let modern_a = HelloPeerClient::new("IrohA").unwrap_or_else(|e| panic!("{}", e.report()));
    assert!(modern_a.connect(server.connection_string(), 0, HelloWorldServerFixture::PASSWORD, JOIN_TIMEOUT).unwrap());
    let modern_b = HelloPeerClient::new("IrohB").unwrap_or_else(|e| panic!("{}", e.report()));
    assert!(modern_b.connect(server.connection_string(), 0, HelloWorldServerFixture::PASSWORD, JOIN_TIMEOUT).unwrap());

    // The server declines at once, so a legacy request comes back false long before any timeout
    // — and so does an iroh client's request for a legacy partner.
    let started = std::time::Instant::now();
    assert!(!legacy.open_direct_link(modern_a.player_id(), LINK_TIMEOUT).unwrap(), "a legacy client was offered a direct link");
    assert!(started.elapsed() < Duration::from_secs(20), "the decline took {:?}; it should be immediate", started.elapsed());
    assert!(!modern_a.open_direct_link(legacy.player_id(), LINK_TIMEOUT).unwrap(), "an iroh client was offered a direct link to a legacy one");
    assert!(!legacy.has_direct_link(modern_a.player_id()) && !modern_a.has_direct_link(legacy.player_id()));

    // "Direct" sends between them still land — relayed by the server, and reported as such.
    let received: Arc<Mutex<Vec<(u16, i32, HelloTransport)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = received.clone();
    modern_a.base().on_number_received(Arc::new(move |sender, value, transport| sink.lock().push((sender, value, transport))));
    let sink = received.clone();
    legacy.base().on_number_received(Arc::new(move |sender, value, transport| sink.lock().push((sender, value, transport))));
    legacy.send_number_direct(modern_a.player_id(), 41).unwrap();
    modern_a.send_number_direct(legacy.player_id(), 42).unwrap();
    wait_until(|| received.lock().len() == 2, DELIVERY_TIMEOUT, || format!("{:?}", received.lock()));
    let got = received.lock().clone();
    assert!(got.contains(&(legacy.player_id(), 41, HelloTransport::ServerRelay)));
    assert!(got.contains(&(modern_a.player_id(), 42, HelloTransport::ServerRelay)));

    // The control: two iroh clients in the same room still get their direct link.
    assert!(modern_a.open_direct_link(modern_b.player_id(), LINK_TIMEOUT).unwrap(), "the iroh pair should offload as before");
    wait_until(|| modern_b.has_direct_link(modern_a.player_id()), LINK_TIMEOUT, || "the acceptor never confirmed the link".to_string());
    legacy.disconnect();
    modern_a.disconnect();
    modern_b.disconnect();
}

#[test]
#[serial(network_statics)]
fn a_legacy_client_that_vanishes_is_timed_out_by_the_server() {
    let server = HelloWorldServerFixture::new();
    let staying = iroh_client(&server, "Staying");
    let vanishing = legacy_client(&server, "Vanishing");
    assert_eq!(NetworkServer::authenticated_peers().len(), 2);
    // Pull the plug: no disconnect packet, the socket just stops answering.
    let manager = vanishing.network_client().and_then(|c| c.client()).expect("a transport");
    manager.as_any().downcast_ref::<LnlNetManager>().expect("a LiteNetLib transport").stop_silently();
    let timeout = Duration::from_millis(u64::try_from(HelloWorldServerFixture::LEGACY_DISCONNECT_TIMEOUT_MS).unwrap() + 6000);
    wait_until(
        || NetworkServer::authenticated_peers().len() == 1,
        timeout,
        || format!("{} peers still authenticated", NetworkServer::authenticated_peers().len()),
    );
    assert!(NetworkServer::authenticated_peers().contains_key(&i32::from(staying.player_id())));
    staying.disconnect();
}

#[test]
#[serial(network_statics)]
fn a_legacy_client_with_the_wrong_password_is_refused() {
    let server = HelloWorldServerFixture::new();
    let client = BasisHelloClient::with_stack("WrongPassword", BasisNetworkStackRegistry::LITE_NET_LIB_ID).unwrap();
    let joined = client.connect(&server.legacy_address(), 0, "not-the-password", Duration::from_secs(8)).unwrap();
    assert!(!joined);
    assert!(!client.is_joined());
    assert!(NetworkServer::authenticated_peers().is_empty());
    // The refusal reached the transport: the peer the client holds is no longer connected.
    assert!(client.server_peer().is_some_and(|p| !p.is_connected()));
}

#[test]
#[serial(network_statics)]
fn the_legacy_port_answers_the_server_info_probe() {
    let server = HelloWorldServerFixture::new();
    let _one = legacy_client(&server, "Counted");
    let target = ConnectionTarget::new(BasisNetworkStackRegistry::LITE_NET_LIB_ID, &server.legacy_address());
    let result = IrohRuntime::block_on(BasisNetworkStackRegistry::probe_async(Some(target), 5000)).unwrap();
    assert!(result.reachable, "probe failed: {}", result.error);
    assert_eq!(result.name, "Basis Server");
    assert_eq!(result.online, 1);
    assert_eq!(result.max, 64);
    // The mixed parser routes the same string the same way — once the server's per-IP probe
    // cooldown (an anti-amplification measure it shares with the C# server) has passed.
    std::thread::sleep(Duration::from_millis(u64::try_from(BasisServerInfoQuery::MIN_INTERVAL_MS).unwrap_or(1000) + 250));
    let target = ConnectionTarget::new(BasisNetworkStackRegistry::MIXED_ID, &server.legacy_address());
    let result = IrohRuntime::block_on(BasisNetworkStackRegistry::probe_async(Some(target), 5000)).unwrap();
    assert!(result.reachable, "mixed probe failed: {}", result.error);
}
