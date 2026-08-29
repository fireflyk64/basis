//! The LiteNetLib-protocol transport, end to end over loopback.
//!
//! Two kinds of test. Loopback pairs run two real `LnlNetManager`s against each other (the
//! shape of the C# `CompactMergeTransportTests` and `CompactMergedTests`, ported here). The raw
//! tests drive a hand-rolled UDP client that speaks the wire format byte for byte — the
//! handshake, then whatever malformed datagram the test wants to feed the server — because a
//! symmetric bug in an implementation that only ever talks to itself would otherwise pass.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use basis_network_core::configuration::LNLTransportConfig;
use basis_network_core::transport::basis_network_shell::{
    ConnectionRequest, DeliveryMethod, DisconnectReason, EventBasedNetListener, NetManager, NetPeerRef, PeerIdAllocator, SendError,
};
use basis_network_core::transport::connection_target::ConnectionTarget;
use basis_network_core::transport::iroh_network_impl::IrohRuntime;
use basis_network_core::transport::lnl_network_impl::internal_packets::{NetConnectAcceptPacket, NetConnectRequestPacket};
use basis_network_core::transport::lnl_network_impl::net_utils::{socket_address_bytes, utc_now_ticks};
use basis_network_core::transport::lnl_network_impl::{ConnectionState, LnlNetManager, LnlSettings, NetConstants, NetPacket, PacketProperty};
use basis_network_core::{BasisNetworkCommons, NetDataReader, NetDataWriter};
use parking_lot::Mutex;
use serial_test::serial;

const CONNECT_KEY: &str = "compact-merge-test";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

type Received = (u8, DeliveryMethod, Vec<u8>);
type Disconnected = (NetPeerRef, DisconnectReason, Vec<u8>);
type Unconnected = (SocketAddr, Vec<u8>);

/// One end of a loopback pair: a manager plus everything its listener saw.
struct Endpoint {
    manager: Arc<LnlNetManager>,
    received: Arc<Mutex<Vec<Received>>>,
    connected: Arc<Mutex<Vec<NetPeerRef>>>,
    disconnected: Arc<Mutex<Vec<Disconnected>>>,
    unconnected: Arc<Mutex<Vec<Unconnected>>>,
    requests: Arc<AtomicUsize>,
}

fn settings(tune: impl FnOnce(&mut LnlSettings)) -> LnlSettings {
    let mut s = LnlSettings::from_config(&LNLTransportConfig::default(), true);
    s.update_time_ms = 2;
    s.mtu_discovery = false;
    s.mtu_override = 1200;
    s.merge_hold_ms = 0.0;
    s.max_unreliable_queue_per_peer = 8192;
    s.disconnect_timeout_ms = 60_000.0;
    s.ping_interval_ms = 1000.0;
    tune(&mut s);
    s
}

impl Endpoint {
    /// Accepts any request whose payload is `CONNECT_KEY`; rejects the rest with a reason.
    fn new(tune: impl FnOnce(&mut LnlSettings)) -> Self {
        let listener = EventBasedNetListener::new();
        let received = Arc::new(Mutex::new(Vec::new()));
        let connected = Arc::new(Mutex::new(Vec::new()));
        let disconnected = Arc::new(Mutex::new(Vec::new()));
        let unconnected = Arc::new(Mutex::new(Vec::new()));
        let requests = Arc::new(AtomicUsize::new(0));
        {
            let requests = requests.clone();
            listener.connection_request_event.subscribe(Arc::new(move |request: Arc<dyn ConnectionRequest>| {
                requests.fetch_add(1, Ordering::Relaxed);
                let key = request.data().get_string().unwrap_or_default();
                if key == CONNECT_KEY {
                    request.accept().expect("accept");
                } else {
                    let mut w = NetDataWriter::new();
                    w.put_string("wrong key").unwrap();
                    request.reject(&w).expect("reject");
                }
            }));
        }
        {
            let received = received.clone();
            listener.network_receive_event.subscribe(Arc::new(move |_peer, mut reader: NetDataReader, channel, method| {
                received.lock().push((channel, method, reader.get_remaining_bytes()));
            }));
        }
        {
            let connected = connected.clone();
            listener.peer_connected_event.subscribe(Arc::new(move |peer| connected.lock().push(peer)));
        }
        {
            let disconnected = disconnected.clone();
            listener.peer_disconnected_event.subscribe(Arc::new(move |peer, mut info| {
                disconnected.lock().push((peer, info.reason, info.additional_data.get_remaining_bytes()));
            }));
        }
        {
            let unconnected = unconnected.clone();
            listener.network_receive_unconnected_event.subscribe(Arc::new(move |remote, mut reader: NetDataReader| {
                unconnected.lock().push((remote, reader.get_remaining_bytes()));
            }));
        }
        let manager = Arc::new(LnlNetManager::with_settings(listener, settings(tune)));
        Self { manager, received, connected, disconnected, unconnected, requests }
    }

    fn start(&self) -> u16 {
        self.manager.start(IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST), 0).unwrap_or_else(|e| panic!("start: {}", e.report()));
        self.manager.local_port()
    }

    fn received_count(&self) -> usize {
        self.received.lock().len()
    }

    fn connect_to(&self, port: u16, key: &str) -> NetPeerRef {
        let mut w = NetDataWriter::new();
        w.put_string(key).unwrap();
        self.manager.connect("127.0.0.1", port, &w).unwrap_or_else(|e| panic!("connect: {}", e.report()))
    }
}

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

fn message(seed: usize, length: usize) -> Vec<u8> {
    (0..length).map(|i| (seed * 31 + i * 7) as u8).collect()
}

/// A connected pair; the client's peer is what the tests send on.
fn connect_pair(server: &Endpoint, client: &Endpoint) -> NetPeerRef {
    let port = server.start();
    client.start();
    let peer = client.connect_to(port, CONNECT_KEY);
    assert!(
        wait_for(|| peer.is_connected() && server.manager.connected_peers_count() == 1 && !client.connected.lock().is_empty(), HANDSHAKE_TIMEOUT),
        "peers never reached Connected"
    );
    peer
}

// ─────────────────────────────────────────────────────────────────────────────
// Handshake
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial(lnl_transport)]
fn client_connects_and_both_sides_learn_the_ids() {
    let server = Endpoint::new(|_| {});
    let client = Endpoint::new(|_| {});
    let peer = connect_pair(&server, &client);
    assert_eq!(server.requests.load(Ordering::Relaxed), 1, "one request per connect, however often the request is resent");
    let server_peer = server.connected.lock()[0].clone();
    assert_eq!(server_peer.id(), 0, "the first admitted peer takes id 0");
    assert_eq!(peer.remote_id(), server_peer.id(), "the accept carries the server-assigned id");
    assert_eq!(server_peer.remote_id(), peer.id(), "the request carried the client's own id");
    assert_eq!(server_peer.address(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert!(server.manager.peer(0).is_some());
    assert_eq!(client.manager.first_peer().map(|p| p.connection_state()), Some(ConnectionState::Connected));
    // A second connect to the same server is the same peer.
    let again = client.connect_to(server.manager.local_port(), CONNECT_KEY);
    assert!(basis_network_core::transport::basis_network_shell::peers_equal(&again, &peer));
    assert_eq!(client.manager.connected_peers_count(), 1);
}

#[test]
#[serial(lnl_transport)]
fn rejected_connection_delivers_the_reject_data() {
    let server = Endpoint::new(|_| {});
    let client = Endpoint::new(|_| {});
    let port = server.start();
    client.start();
    let peer = client.connect_to(port, "not the key");
    assert!(wait_for(|| !client.disconnected.lock().is_empty(), HANDSHAKE_TIMEOUT), "the client never heard the verdict");
    let (gone, reason, data) = client.disconnected.lock()[0].clone();
    assert!(basis_network_core::transport::basis_network_shell::peers_equal(&gone, &peer));
    assert_eq!(reason, DisconnectReason::ConnectionRejected);
    assert_eq!(NetDataReader::new(data).get_string().unwrap(), "wrong key");
    assert!(!peer.is_connected());
    assert_eq!(server.manager.connected_peers_count(), 0);
    assert!(server.connected.lock().is_empty(), "a rejected peer is never announced as connected");
}

#[test]
#[serial(lnl_transport)]
fn connecting_to_nobody_fails_after_the_attempts_run_out() {
    let client = Endpoint::new(|s| {
        s.reconnect_delay_ms = 50.0;
        s.max_connect_attempts = 3;
    });
    client.start();
    let dead: UdpSocket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = dead.local_addr().unwrap().port();
    let peer = client.connect_to(port, CONNECT_KEY);
    assert!(wait_for(|| !client.disconnected.lock().is_empty(), Duration::from_secs(5)));
    assert_eq!(client.disconnected.lock()[0].1, DisconnectReason::ConnectionFailed);
    assert!(!peer.is_connected());
    // Sends to a peer that never connected are dropped, not errors.
    assert_eq!(peer.send(b"x", 0, DeliveryMethod::ReliableOrdered), Ok(()));
}

#[test]
#[serial(lnl_transport)]
fn connect_before_start_and_bad_targets_are_errors() {
    let client = Endpoint::new(|_| {});
    let w = NetDataWriter::new();
    let err = client.manager.connect("127.0.0.1", 4296, &w).err().expect("connect before start must fail");
    assert_eq!(err.code(), basis_error::ErrorCode::Conflict);
    client.start();
    let err = client.manager.connect("not a host name at all", 0, &w).err().expect("unparseable target");
    assert!(matches!(err.code(), basis_error::ErrorCode::InvalidArgument | basis_error::ErrorCode::Dns), "{}", err.report());
}

// ─────────────────────────────────────────────────────────────────────────────
// Delivery
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial(lnl_transport)]
fn reliable_ordered_messages_arrive_whole_and_in_order() {
    let server = Endpoint::new(|_| {});
    let client = Endpoint::new(|_| {});
    let peer = connect_pair(&server, &client);
    let count = 300;
    for i in 0..count {
        peer.send(&message(i, 20 + i % 500), 5, DeliveryMethod::ReliableOrdered).unwrap();
    }
    assert!(wait_for(|| server.received_count() == count, DELIVERY_TIMEOUT), "got {} of {count}", server.received_count());
    let received = server.received.lock().clone();
    for (i, (channel, method, data)) in received.iter().enumerate() {
        assert_eq!(*channel, 5);
        assert_eq!(*method, DeliveryMethod::ReliableOrdered);
        assert_eq!(*data, message(i, 20 + i % 500), "message {i} differs");
    }
    // Far more than a window's worth went through: the acks moved the window.
    assert!(count > NetConstants::DEFAULT_WINDOW_SIZE);
}

#[test]
#[serial(lnl_transport)]
fn every_delivery_method_round_trips() {
    let server = Endpoint::new(|_| {});
    let client = Endpoint::new(|_| {});
    let peer = connect_pair(&server, &client);
    let methods = [
        DeliveryMethod::ReliableUnordered,
        DeliveryMethod::Sequenced,
        DeliveryMethod::ReliableOrdered,
        DeliveryMethod::ReliableSequenced,
        DeliveryMethod::Unreliable,
    ];
    for (i, method) in methods.iter().enumerate() {
        peer.send(&message(i, 40), i as u8, *method).unwrap();
    }
    assert!(wait_for(|| server.received_count() == methods.len(), DELIVERY_TIMEOUT), "got {}", server.received_count());
    let received = server.received.lock().clone();
    for (i, method) in methods.iter().enumerate() {
        let got = received.iter().find(|(c, _, _)| *c == i as u8).unwrap_or_else(|| panic!("nothing on channel {i}"));
        assert_eq!(got.1, *method);
        assert_eq!(got.2, message(i, 40));
    }
}

#[test]
#[serial(lnl_transport)]
fn traffic_flows_both_ways_at_once() {
    let server = Endpoint::new(|_| {});
    let client = Endpoint::new(|_| {});
    let client_peer = connect_pair(&server, &client);
    let server_peer = server.connected.lock()[0].clone();
    for i in 0..100 {
        client_peer.send(&message(i, 50), 1, DeliveryMethod::ReliableOrdered).unwrap();
        server_peer.send(&message(1000 + i, 60), 2, DeliveryMethod::ReliableOrdered).unwrap();
    }
    assert!(wait_for(|| server.received_count() == 100 && client.received_count() == 100, DELIVERY_TIMEOUT));
    assert!(client.received.lock().iter().enumerate().all(|(i, (c, _, d))| *c == 2 && *d == message(1000 + i, 60)));
}

#[test]
#[serial(lnl_transport)]
fn large_reliable_messages_are_fragmented_and_reassembled() {
    let server = Endpoint::new(|_| {});
    let client = Endpoint::new(|_| {});
    let peer = connect_pair(&server, &client);
    // Well over the 1200-byte MTU: dozens of fragments, all of them reliable.
    let big = message(7, 40_000);
    peer.send(&big, 3, DeliveryMethod::ReliableOrdered).unwrap();
    let unordered = message(8, 5_000);
    peer.send(&unordered, 4, DeliveryMethod::ReliableUnordered).unwrap();
    assert!(wait_for(|| server.received_count() == 2, DELIVERY_TIMEOUT), "got {}", server.received_count());
    let received = server.received.lock().clone();
    let big_got = received.iter().find(|(c, _, _)| *c == 3).unwrap();
    assert_eq!(big_got.1, DeliveryMethod::ReliableOrdered);
    assert_eq!(big_got.2.len(), big.len());
    assert!(big_got.2 == big, "reassembled bytes differ");
    let un_got = received.iter().find(|(c, _, _)| *c == 4).unwrap();
    assert_eq!(un_got.2, unordered);
}

#[test]
#[serial(lnl_transport)]
fn sends_the_transport_cannot_carry_are_refused_not_dropped() {
    let server = Endpoint::new(|_| {});
    let client = Endpoint::new(|_| {});
    let peer = connect_pair(&server, &client);
    let too_big = vec![1u8; 1300];
    match peer.send(&too_big, 0, DeliveryMethod::Unreliable) {
        Err(SendError::TooBig { size, limit, method }) => {
            assert_eq!(size, 1300);
            assert_eq!(limit, 1200 - NetConstants::UNRELIABLE_HEADER_SIZE);
            assert_eq!(method, DeliveryMethod::Unreliable);
        }
        other => panic!("expected TooBig, got {other:?}"),
    }
    assert!(matches!(peer.send(&too_big, 0, DeliveryMethod::Sequenced), Err(SendError::TooBig { .. })));
    assert!(matches!(peer.send(&too_big, 0, DeliveryMethod::ReliableSequenced), Err(SendError::TooBig { .. })));
    assert_eq!(peer.send(b"x", 64, DeliveryMethod::Unreliable), Err(SendError::BadChannel { channel: 64, max: 64 }));
    assert!(matches!(peer.send_unreliable_raw_merge(b"abc", 2, 5, 0, -1, 0), Err(SendError::BadRange { .. })));
    // The largest unreliable payload that fits goes through untouched.
    let fits = message(9, 1200 - NetConstants::UNRELIABLE_HEADER_SIZE);
    peer.send(&fits, 0, DeliveryMethod::Unreliable).unwrap();
    assert!(wait_for(|| server.received_count() == 1, DELIVERY_TIMEOUT));
    assert_eq!(server.received.lock()[0].2, fits);
}

#[test]
#[serial(lnl_transport)]
fn raw_merge_send_patches_one_byte_per_receiver() {
    let server = Endpoint::new(|_| {});
    let client = Endpoint::new(|_| {});
    let peer = connect_pair(&server, &client);
    let shared = message(3, 32);
    peer.send_unreliable_raw_merge(&shared, 4, 20, BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL, 2, 0xEE).unwrap();
    peer.send_unreliable_raw_merge(&shared, 4, 20, BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL, -1, 0).unwrap();
    assert!(wait_for(|| server.received_count() == 2, DELIVERY_TIMEOUT));
    let received = server.received.lock().clone();
    let mut patched = shared[4..24].to_vec();
    patched[2] = 0xEE;
    assert!(received.iter().any(|(c, m, d)| *c == BasisNetworkCommons::PLAYER_AVATAR_HIGH_CHANNEL && *m == DeliveryMethod::Unreliable && *d == patched));
    assert!(received.iter().any(|(_, _, d)| *d == shared[4..24]));
}

// ─────────────────────────────────────────────────────────────────────────────
// CompactMerged framing — port of CompactMergeTransportTests.cs / CompactMergedTests.cs
// ─────────────────────────────────────────────────────────────────────────────

fn unreliable_survive(compact: bool) {
    let server = Endpoint::new(|s| s.compact_merge_enabled = compact);
    let client = Endpoint::new(|s| s.compact_merge_enabled = compact);
    let peer = connect_pair(&server, &client);
    let lnl = peer.as_any().downcast_ref::<basis_network_core::transport::LnlNetPeer>().expect("an LNL peer");
    assert_eq!(lnl.compact_merge_active(), compact);
    // Lengths straddle the one-byte length boundary; both framings have to carry all of it.
    // Channels 3 and 4 are voice: the Basis transport drains those ahead of everything else,
    // so a strict-order check keeps to the bulk channels.
    let lengths = [1usize, 8, 64, 200, 255, 256, 300, 700];
    let channels = [0u8, 1, 2, 5, 6, 7, 8, 9];
    let mut sent = Vec::new();
    for i in 0..120 {
        let channel = channels[i % 8];
        let data = message(i, lengths[i % lengths.len()]);
        sent.push((channel, data.clone()));
        peer.send(&data, channel, DeliveryMethod::Unreliable).unwrap();
    }
    assert!(wait_for(|| server.received_count() == sent.len(), DELIVERY_TIMEOUT), "expected {}, got {}", sent.len(), server.received_count());
    let received = server.received.lock().clone();
    for (i, (channel, data)) in sent.iter().enumerate() {
        assert_eq!(received[i].1, DeliveryMethod::Unreliable);
        assert_eq!(received[i].0, *channel);
        assert_eq!(received[i].2, *data);
    }
}

#[test]
#[serial(lnl_transport)]
fn unreliable_messages_survive_compact_framing() {
    unreliable_survive(true);
}

#[test]
#[serial(lnl_transport)]
fn unreliable_messages_survive_legacy_framing() {
    unreliable_survive(false);
}

#[test]
#[serial(lnl_transport)]
fn disabling_compact_on_one_end_only_still_round_trips_both_ways() {
    // The switch is send-side; both framings are always decoded.
    let server = Endpoint::new(|s| s.compact_merge_enabled = true);
    let client = Endpoint::new(|s| s.compact_merge_enabled = false);
    let client_peer = connect_pair(&server, &client);
    let server_peer = server.connected.lock()[0].clone();
    let up = message(1, 120);
    client_peer.send(&up, 3, DeliveryMethod::Unreliable).unwrap();
    assert!(wait_for(|| server.received_count() == 1, DELIVERY_TIMEOUT));
    assert_eq!(server.received.lock()[0], (3, DeliveryMethod::Unreliable, up));
    let down = message(2, 130);
    server_peer.send(&down, 4, DeliveryMethod::Unreliable).unwrap();
    assert!(wait_for(|| client.received_count() == 1, DELIVERY_TIMEOUT));
    assert_eq!(client.received.lock()[0], (4, DeliveryMethod::Unreliable, down));
}

#[test]
#[serial(lnl_transport)]
fn mixed_reliable_and_unreliable_do_not_corrupt_each_other() {
    // The regression the C# suite guards: Ack and Channeled packets go through the same merge
    // buffer, so a datagram holding both framings deserialised as garbage on the far side.
    let server = Endpoint::new(|_| {});
    let client = Endpoint::new(|_| {});
    let peer = connect_pair(&server, &client);
    let mut expected: Vec<(u8, DeliveryMethod, Vec<u8>)> = Vec::new();
    for i in 0..200 {
        let channel = (i % 4) as u8;
        let method = if i % 3 == 0 { DeliveryMethod::ReliableOrdered } else { DeliveryMethod::Unreliable };
        let data = message(i + 500, 20 + (i % 260));
        expected.push((channel, method, data.clone()));
        peer.send(&data, channel, method).unwrap();
    }
    assert!(wait_for(|| server.received_count() == expected.len(), DELIVERY_TIMEOUT), "expected {}, got {}", expected.len(), server.received_count());
    let mut outstanding = expected;
    for got in server.received.lock().iter() {
        let index = outstanding.iter().position(|e| *e == *got).unwrap_or_else(|| panic!("received a message nobody sent: channel {}, {} bytes", got.0, got.2.len()));
        outstanding.remove(index);
    }
    assert!(outstanding.is_empty());
}

#[test]
#[serial(lnl_transport)]
fn compact_framing_puts_fewer_bytes_on_the_wire() {
    let measure = |compact: bool| {
        let server = Endpoint::new(|s| s.compact_merge_enabled = compact);
        let client = Endpoint::new(|s| s.compact_merge_enabled = compact);
        let peer = connect_pair(&server, &client);
        let before = client.manager.statistics().bytes_sent;
        for i in 0..300 {
            peer.send(&message(i, 80), (i % 4) as u8, DeliveryMethod::Unreliable).unwrap();
        }
        assert!(wait_for(|| server.received_count() == 300, DELIVERY_TIMEOUT));
        client.manager.statistics().bytes_sent - before
    };
    let compact = measure(true);
    let legacy = measure(false);
    println!("300 x 80 B unreliable: legacy {legacy} B, compact {compact} B");
    assert!(compact < legacy, "compact framing sent {compact} bytes, legacy sent {legacy}");
}

#[test]
#[serial(lnl_transport)]
fn a_burst_of_unreliable_sends_shares_datagrams() {
    // 100 small unreliable sends queued in one go must leave as far fewer datagrams than
    // messages: that is what merging is for.
    let server = Endpoint::new(|_| {});
    let client = Endpoint::new(|_| {});
    let peer = connect_pair(&server, &client);
    let before = client.manager.statistics().packets_sent;
    for i in 0..100 {
        peer.send(&message(i, 40), 1, DeliveryMethod::Unreliable).unwrap();
    }
    assert!(wait_for(|| server.received_count() == 100, DELIVERY_TIMEOUT));
    let datagrams = client.manager.statistics().packets_sent - before;
    println!("100 x 40 B unreliable left as {datagrams} datagrams");
    assert!(datagrams < 50, "{datagrams} datagrams for 100 messages: nothing was merged");
}

// ─────────────────────────────────────────────────────────────────────────────
// Disconnects and liveness
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial(lnl_transport)]
fn disconnect_with_data_reaches_the_other_side() {
    let server = Endpoint::new(|_| {});
    let client = Endpoint::new(|_| {});
    let peer = connect_pair(&server, &client);
    peer.disconnect_with(b"bye now");
    assert!(wait_for(|| !server.disconnected.lock().is_empty() && !client.disconnected.lock().is_empty(), HANDSHAKE_TIMEOUT));
    let (_, server_reason, server_data) = server.disconnected.lock()[0].clone();
    assert_eq!(server_reason, DisconnectReason::RemoteConnectionClose);
    assert_eq!(server_data, b"bye now");
    let (_, client_reason, _) = client.disconnected.lock()[0].clone();
    assert_eq!(client_reason, DisconnectReason::DisconnectPeerCalled);
    assert!(!peer.is_connected());
    assert_eq!(server.manager.connected_peers_count(), 0);
    // The server side acknowledged the shutdown, so the client's peer settled to Disconnected.
    assert!(wait_for(|| client.manager.first_peer().map(|p| p.connection_state()) == Some(ConnectionState::Disconnected), HANDSHAKE_TIMEOUT));
}

#[test]
#[serial(lnl_transport)]
fn stopping_a_manager_tells_its_peers() {
    let server = Endpoint::new(|_| {});
    let client = Endpoint::new(|_| {});
    let _peer = connect_pair(&server, &client);
    client.manager.stop();
    assert!(wait_for(|| !server.disconnected.lock().is_empty(), HANDSHAKE_TIMEOUT));
    assert_eq!(server.disconnected.lock()[0].1, DisconnectReason::RemoteConnectionClose);
    assert!(!client.manager.is_running());
    assert_eq!(client.manager.connected_peers_count(), 0);
}

#[test]
#[serial(lnl_transport)]
fn a_silent_peer_times_out_and_pings_keep_a_quiet_one_alive() {
    let server = Endpoint::new(|s| {
        s.disconnect_timeout_ms = 1500.0;
        s.ping_interval_ms = 200.0;
    });
    let quiet = Endpoint::new(|s| s.ping_interval_ms = 200.0);
    let quiet_peer = connect_pair(&server, &quiet);
    // No user traffic for three timeouts: the pings alone keep it up, and measure the RTT.
    std::thread::sleep(Duration::from_millis(4500));
    assert!(quiet_peer.is_connected());
    assert!(server.disconnected.lock().is_empty(), "a peer that answers pings must not time out");
    assert!(quiet_peer.time_since_last_packet() < 1500.0);
    assert!(quiet_peer.round_trip_time() < 200, "loopback RTT {} ms", quiet_peer.round_trip_time());

    // Now one that vanishes without a word.
    let silent = Endpoint::new(|_| {});
    silent.start();
    let silent_peer = silent.connect_to(server.manager.local_port(), CONNECT_KEY);
    assert!(wait_for(|| silent_peer.is_connected() && server.manager.connected_peers_count() == 2, HANDSHAKE_TIMEOUT));
    silent.manager.stop_silently();
    assert!(wait_for(|| !server.disconnected.lock().is_empty(), Duration::from_secs(6)), "the server never timed the silent peer out");
    let (gone, reason, _) = server.disconnected.lock()[0].clone();
    assert_eq!(reason, DisconnectReason::Timeout);
    assert_eq!(gone.remote_id(), silent_peer.id());
    assert_eq!(server.manager.connected_peers_count(), 1);
    assert!(quiet_peer.is_connected(), "the well-behaved peer is untouched");
}

#[test]
#[serial(lnl_transport)]
fn a_reconnect_from_the_same_address_replaces_the_old_connection() {
    let server = Endpoint::new(|_| {});
    let client = Endpoint::new(|_| {});
    let first = connect_pair(&server, &client);
    // The client restarts on the same port: a new connect time, so the server sees a reconnect.
    let port = client.manager.local_port();
    client.manager.stop_silently();
    let client2 = Endpoint::new(|_| {});
    client2.manager.start(IpAddr::V4(Ipv4Addr::LOCALHOST), IpAddr::V6(Ipv6Addr::LOCALHOST), port).unwrap_or_else(|e| panic!("{}", e.report()));
    let second = client2.connect_to(server.manager.local_port(), CONNECT_KEY);
    assert!(wait_for(|| second.is_connected(), HANDSHAKE_TIMEOUT));
    assert!(wait_for(|| server.disconnected.lock().iter().any(|(_, r, _)| *r == DisconnectReason::Reconnect), HANDSHAKE_TIMEOUT));
    assert_eq!(server.connected.lock().len(), 2);
    assert_eq!(server.manager.connected_peers_count(), 1);
    assert!(!first.is_connected());
    second.send(b"after reconnect", 0, DeliveryMethod::ReliableOrdered).unwrap();
    assert!(wait_for(|| server.received_count() == 1, DELIVERY_TIMEOUT));
}

// ─────────────────────────────────────────────────────────────────────────────
// Unconnected messages: the server-info probe
// ─────────────────────────────────────────────────────────────────────────────

#[test]
#[serial(lnl_transport)]
fn unconnected_messages_carry_the_server_info_probe() {
    let server = Endpoint::new(|_| {});
    let port = server.start();
    // The server answers every unconnected query with a canned info line, as the Basis
    // handler does.
    {
        let manager = server.manager.clone();
        let listener_hook: Arc<Mutex<Option<Arc<LnlNetManager>>>> = Arc::new(Mutex::new(Some(manager)));
        let hook = listener_hook.clone();
        let received = server.unconnected.clone();
        // Re-subscribe on the manager's listener by connecting a second handler through the
        // recorded queue: the fixture recorded the datagram, this replies to it.
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut answered = 0;
            while Instant::now() < deadline {
                let pending: Vec<(SocketAddr, Vec<u8>)> = received.lock().drain(..).collect();
                for (remote, query) in pending {
                    let mut reader = NetDataReader::new(query);
                    assert_eq!(reader.get_uint().unwrap(), BasisNetworkCommons::SERVER_INFO_QUERY_MAGIC);
                    let _version = reader.get_ushort().unwrap();
                    let nonce = reader.get_ushort().unwrap();
                    let mut w = NetDataWriter::new();
                    w.put_uint(BasisNetworkCommons::SERVER_INFO_RESPONSE_MAGIC);
                    w.put_ushort(BasisNetworkCommons::SERVER_INFO_PROTOCOL_VERSION);
                    w.put_ushort(nonce);
                    w.put_ushort(3);
                    w.put_ushort(64);
                    w.put_string("Loopback Server").unwrap();
                    w.put_string("welcome").unwrap();
                    if let Some(m) = hook.lock().as_ref() {
                        assert!(m.send_unconnected_message(&w, remote));
                        answered += 1;
                    }
                }
                if answered > 0 {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
    }
    let target = ConnectionTarget::new("litenetlib", &format!("127.0.0.1:{port}"));
    let result = IrohRuntime::block_on(LnlNetManager::probe(target, 4000)).unwrap();
    assert!(result.reachable, "probe failed: {}", result.error);
    assert_eq!(result.name, "Loopback Server");
    assert_eq!(result.motd, "welcome");
    assert_eq!((result.online, result.max), (3, 64));
    assert_eq!(result.resolved_address, Some(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    assert!(result.round_trip_ms < 4000);

    // Nobody home: the probe reports a timeout rather than hanging.
    let dead = UdpSocket::bind("127.0.0.1:0").unwrap();
    let target = ConnectionTarget::new("litenetlib", &format!("127.0.0.1:{}", dead.local_addr().unwrap().port()));
    let result = IrohRuntime::block_on(LnlNetManager::probe(target, 300)).unwrap();
    assert!(!result.reachable && result.timed_out);
}

#[test]
#[serial(lnl_transport)]
fn peer_ids_come_from_one_pool_when_managers_share_it() {
    let ids = PeerIdAllocator::new();
    assert_eq!(ids.allocate(), 0);
    assert_eq!(ids.allocate(), 1);
    ids.release(0);
    assert_eq!(ids.allocate(), 0, "the lowest free id is reused first");
    assert_eq!(ids.live_count(), 2);
    ids.reset();
    assert_eq!(ids.live_count(), 0);
    assert_eq!(ids.allocate(), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Raw wire tests: a UDP socket speaking the protocol by hand
// ─────────────────────────────────────────────────────────────────────────────

/// A client written against the wire format alone, so the server is tested against bytes a
/// real LiteNetLib peer would send rather than against this crate's own encoder.
struct RawClient {
    socket: UdpSocket,
    server: SocketAddr,
    connect_time: i64,
    connection_number: u8,
    remote_id: i32,
}

impl RawClient {
    fn handshake(server_port: u16) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let server = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), server_port);
        let connect_time = utc_now_ticks();
        let mut payload = NetDataWriter::new();
        payload.put_string(CONNECT_KEY).unwrap();
        let request = NetConnectRequestPacket::make(payload.as_read_only_span(), &socket_address_bytes(server), connect_time, 7);
        socket.send_to(request.raw(), server).unwrap();
        let mut buffer = [0u8; 2048];
        let (n, _) = socket.recv_from(&mut buffer).unwrap();
        let packet = NetPacket::from_bytes(buffer[..n].to_vec());
        assert_eq!(packet.property(), Some(PacketProperty::ConnectAccept), "expected a connect accept, got {:?}", packet.property());
        let accept = NetConnectAcceptPacket::from_data(&packet).expect("a well-formed accept");
        assert_eq!(accept.connection_time, connect_time);
        Self { socket, server, connect_time, connection_number: accept.connection_number, remote_id: accept.peer_id }
    }

    fn send(&self, mut bytes: Vec<u8>) {
        bytes[0] = (bytes[0] & 0x9F) | (self.connection_number << 5);
        self.socket.send_to(&bytes, self.server).unwrap();
    }

    fn send_unreliable(&self, channel: u8, payload: &[u8]) {
        let mut bytes = vec![PacketProperty::Unreliable as u8, channel];
        bytes.extend_from_slice(payload);
        self.send(bytes);
    }

    fn send_compact(&self, body: &[u8]) {
        let mut bytes = vec![PacketProperty::CompactMerged as u8];
        bytes.extend_from_slice(body);
        self.send(bytes);
    }

    /// Reads datagrams until one of `property` arrives or the timeout passes.
    fn wait_for_property(&self, property: PacketProperty, timeout: Duration) -> Option<NetPacket> {
        let deadline = Instant::now() + timeout;
        let mut buffer = [0u8; 2048];
        while Instant::now() < deadline {
            match self.socket.recv_from(&mut buffer) {
                Ok((n, _)) => {
                    let packet = NetPacket::from_bytes(buffer[..n].to_vec());
                    if packet.property() == Some(property) {
                        return Some(packet);
                    }
                }
                Err(_) => return None,
            }
        }
        None
    }
}

#[test]
#[serial(lnl_transport)]
fn older_protocol_id_is_rejected_at_the_handshake() {
    // A peer without the compact parser would drop every CompactMerged datagram as an unknown
    // property and lose all unreliable traffic silently, so it must not reach the traffic phase.
    let server = Endpoint::new(|_| {});
    let port = server.start();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    // [0] property, [1..4] protocol id, [5..12] connect time, [13..16] peer id, [17] address size, then the address.
    let mut request = vec![0u8; 18 + 16];
    request[0] = PacketProperty::ConnectRequest as u8;
    request[1..5].copy_from_slice(&13i32.to_le_bytes());
    request[5..13].copy_from_slice(&1i64.to_le_bytes());
    request[13..17].copy_from_slice(&1i32.to_le_bytes());
    request[17] = 16;
    socket.send_to(&request, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)).unwrap();
    let mut buffer = [0u8; 64];
    let (n, _) = socket.recv_from(&mut buffer).expect("an InvalidProtocol reply");
    assert_eq!(buffer[0] & 0x1F, PacketProperty::InvalidProtocol as u8);
    assert_eq!(n, 1);
    assert_eq!(server.manager.connected_peers_count(), 0);
    assert_eq!(server.requests.load(Ordering::Relaxed), 0, "the handler never even saw it");
}

#[test]
#[serial(lnl_transport)]
fn a_hand_rolled_client_completes_the_handshake_and_is_answered() {
    let server = Endpoint::new(|s| s.ping_interval_ms = 100.0);
    let port = server.start();
    let client = RawClient::handshake(port);
    // Wait on the event, not the counter: the count is bumped just before the listener is
    // called, so a wait on the count can win the race against the vector being filled.
    assert!(wait_for(|| !server.connected.lock().is_empty(), HANDSHAKE_TIMEOUT), "the server never raised PeerConnected");
    let server_peer = server.connected.lock()[0].clone();
    assert_eq!(server_peer.remote_id(), 7, "the id the raw client claimed");
    assert_eq!(client.remote_id, server_peer.id());
    assert_eq!(client.connection_number, 0);
    // A plain unreliable packet on channel 9 is delivered as such.
    client.send_unreliable(9, b"hello");
    assert!(wait_for(|| server.received_count() == 1, DELIVERY_TIMEOUT));
    assert_eq!(server.received.lock()[0], (9, DeliveryMethod::Unreliable, b"hello".to_vec()));
    // The server pings, byte layout [property][sequence:2].
    let ping = client.wait_for_property(PacketProperty::Ping, Duration::from_secs(3)).expect("a ping within the interval");
    assert_eq!(ping.size(), 3);
    assert!(ping.sequence() >= 1);
    // A reliable message from the server is a Channeled packet with the channel id and sequence,
    // and the server keeps resending it until it is acked.
    server_peer.send(b"reliable", 2, DeliveryMethod::ReliableOrdered).unwrap();
    let channeled = client.wait_for_property(PacketProperty::Channeled, Duration::from_secs(3)).expect("a channeled packet");
    assert_eq!(channeled.channel_id(), 2 * NetConstants::CHANNEL_TYPE_COUNT as u8 + DeliveryMethod::ReliableOrdered as u8);
    assert_eq!(channeled.sequence(), 0);
    assert_eq!(&channeled.raw()[4..], b"reliable");
    let resent = client.wait_for_property(PacketProperty::Channeled, Duration::from_secs(3)).expect("a resend, since nothing acked it");
    assert_eq!(resent.sequence(), 0);
    // A disconnect from the wire ends the peer with the data attached.
    let mut bye = vec![0u8; 9 + 3];
    bye[0] = PacketProperty::Disconnect as u8;
    bye[1..9].copy_from_slice(&client.connect_time.to_le_bytes());
    bye[9..].copy_from_slice(b"end");
    client.send(bye);
    assert!(wait_for(|| !server.disconnected.lock().is_empty(), HANDSHAKE_TIMEOUT));
    let (_, reason, data) = server.disconnected.lock()[0].clone();
    assert_eq!(reason, DisconnectReason::RemoteConnectionClose);
    assert_eq!(data, b"end");
    assert!(client.wait_for_property(PacketProperty::ShutdownOk, Duration::from_secs(3)).is_some(), "the shutdown is acknowledged");
}

#[test]
#[serial(lnl_transport)]
fn malformed_compact_merged_is_dropped_without_breaking_the_peer() {
    // Port of MalformedCompactMerged_IsDroppedWithoutBreakingPeer and its neighbours.
    let server = Endpoint::new(|_| {});
    let port = server.start();
    let client = RawClient::handshake(port);
    assert!(wait_for(|| !server.connected.lock().is_empty(), HANDSHAKE_TIMEOUT), "the server never raised PeerConnected");
    let malformed: Vec<Vec<u8>> = vec![
        vec![5],
        vec![0x85, 0x01],
        vec![5, 3, 0x10],
        vec![0x85, 0x00, 0x01, 0x10],
        vec![0x85, 1, 0, 0x10],
        vec![0x41, 4, 2, 0, 0, 0],
        vec![0x40, 1, 2],
        vec![0x40, 4, 0, 0, 0, 0],
        vec![0x40, 4, 18, 0, 0, 0],
        vec![],
        // Short form claiming 200 bytes with 4 present; long form claiming 60000 with 3.
        vec![3, 200, 1, 2, 3, 4],
        vec![3 | 0x80, 0x60, 0xEA, 1, 2, 3],
        vec![3 | 0x80, 0x10],
    ];
    for body in &malformed {
        client.send_compact(body);
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(server.received_count(), 0, "a malformed container delivered something");
    // The peer is intact: a valid message still lands.
    client.send_compact(&[63, 0]);
    client.send_compact(&[3, 1, 0xA1, 4, 5, 0xB1, 5, 1, 0xC1]); // stops before the ragged second entry
    client.send_unreliable(0, &[0xAB, 0xCD]);
    assert!(wait_for(|| server.received_count() == 3, DELIVERY_TIMEOUT), "got {}", server.received_count());
    let received = server.received.lock().clone();
    assert_eq!(received[0], (63, DeliveryMethod::Unreliable, Vec::new()));
    assert_eq!(received[1], (3, DeliveryMethod::Unreliable, vec![0xA1]));
    assert_eq!(received[2], (0, DeliveryMethod::Unreliable, vec![0xAB, 0xCD]));
    assert!(server.disconnected.lock().is_empty());
}

#[test]
#[serial(lnl_transport)]
fn well_formed_compact_runs_and_random_garbage_never_break_the_server() {
    let server = Endpoint::new(|_| {});
    let port = server.start();
    let client = RawClient::handshake(port);
    assert!(wait_for(|| !server.connected.lock().is_empty(), HANDSHAKE_TIMEOUT), "the server never raised PeerConnected");

    // A well-formed run delivers every entry exactly, in order.
    let mut body = Vec::new();
    let mut expected = Vec::new();
    for e in 0..12u8 {
        let length = usize::from(e) * 23 % 130;
        let payload = message(usize::from(e), length);
        let channel = (e * 5) % 64;
        let written = basis_network_core::transport::lnl_network_impl::CompactMerge::entry_size(length);
        let start = body.len();
        body.resize(start + written, 0);
        basis_network_core::transport::lnl_network_impl::CompactMerge::write_unreliable_entry(&mut body, start, channel, &payload);
        expected.push((channel, DeliveryMethod::Unreliable, payload));
    }
    client.send_compact(&body);
    assert!(wait_for(|| server.received_count() == expected.len(), DELIVERY_TIMEOUT));
    assert_eq!(*server.received.lock(), expected);
    server.received.lock().clear();

    // Then a storm of garbage under every property the peer path knows, none of which may
    // panic a receive task or disconnect the peer.
    let mut seed = 0x1234_5678u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for i in 0..4000 {
        let len = (next() % 400) as usize;
        let mut bytes: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        if bytes.is_empty() {
            bytes.push(0);
        }
        bytes[0] = match i % 5 {
            0 => PacketProperty::CompactMerged as u8,
            1 => PacketProperty::Merged as u8,
            2 => PacketProperty::Channeled as u8,
            3 => PacketProperty::Ack as u8,
            _ => (next() % 32) as u8,
        };
        client.send(bytes);
    }
    std::thread::sleep(Duration::from_millis(200));
    client.send_unreliable(1, b"still here");
    assert!(wait_for(|| server.received.lock().iter().any(|(c, _, d)| *c == 1 && d == b"still here"), DELIVERY_TIMEOUT), "the peer did not survive the garbage");
    assert!(server.disconnected.lock().is_empty());
    assert_eq!(server.manager.connected_peers_count(), 1);
}

#[test]
#[serial(lnl_transport)]
fn packets_from_strangers_get_peer_not_found() {
    let server = Endpoint::new(|_| {});
    let port = server.start();
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    socket.send_to(&[PacketProperty::Unreliable as u8, 0, 1, 2], SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)).unwrap();
    let mut buffer = [0u8; 16];
    let (n, _) = socket.recv_from(&mut buffer).expect("a PeerNotFound reply");
    assert_eq!((n, buffer[0] & 0x1F), (1, PacketProperty::PeerNotFound as u8));
    // A datagram that is not even a header is dropped, not answered.
    socket.send_to(&[PacketProperty::Channeled as u8, 0], SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)).unwrap();
    assert!(socket.recv_from(&mut buffer).is_err(), "nothing answers garbage");
    assert_eq!(server.received_count(), 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Reliable send-queue bound: a client that stops reading costs a disconnect, not memory.
// ─────────────────────────────────────────────────────────────────────────────

/// A raw client that completes the handshake and then never acknowledges anything: the server's
/// reliable window fills, the outgoing queue grows, and the byte budget must stop it.
#[test]
#[serial(lnl_transport)]
fn a_peer_that_never_reads_is_disconnected_not_buffered_without_limit() {
    let server = Endpoint::new(|s| {
        // A small budget and a short grace so the test is quick; the mechanism is the same at
        // any size.
        s.max_reliable_queue_bytes_per_peer = 256 * 1024;
        s.reliable_queue_grace_ms = 1000.0;
    });
    let port = server.start();
    // A hand-rolled client that handshakes and then goes silent — it never sends an ack, so the
    // server's window never advances and its outgoing queue is the only thing that can grow.
    let client = RawClient::handshake(port);
    assert!(wait_for(|| !server.connected.lock().is_empty(), HANDSHAKE_TIMEOUT), "the server never raised PeerConnected");
    let server_peer = server.connected.lock()[0].clone();

    // Push far more than the budget of reliable data at the silent peer. Each message is 4 KiB;
    // 256 of them is 1 MiB against a 256 KiB budget, so most are refused.
    let message = vec![0xAB_u8; 4096];
    let mut refused = 0;
    let mut accepted = 0;
    for _ in 0..256 {
        match server_peer.send(&message, 5, DeliveryMethod::ReliableOrdered) {
            Ok(()) => accepted += 1,
            Err(SendError::QueueFull { budget, .. }) => {
                assert_eq!(budget, 256 * 1024);
                refused += 1;
            }
            Err(other) => panic!("unexpected send error: {other:?}"),
        }
    }
    assert!(refused > 0, "the budget never refused a send: {accepted} accepted, {refused} refused");
    // Everything the server holds for this peer is inside the budget plus one window's worth of
    // packets already handed to the channel — nowhere near the 1 MiB it was asked to send.
    println!("silent peer: {accepted} queued, {refused} refused against a 256 KiB budget");

    // The peer stays silent, so the server disconnects it once the grace period passes rather
    // than holding what it accepted forever.
    assert!(
        wait_for(|| !server.disconnected.lock().is_empty(), Duration::from_secs(8)),
        "the server never disconnected a peer that stopped reading"
    );
    let (gone, reason, _) = server.disconnected.lock()[0].clone();
    assert_eq!(reason, DisconnectReason::SendQueueOverBudget);
    // The disconnected peer is the one the server admitted: same server-side id, and it declared
    // the id the raw client claimed in its connect request.
    assert_eq!(gone.id(), server_peer.id());
    assert_eq!(gone.remote_id(), 7);
    assert_eq!(server.manager.connected_peers_count(), 0);
    let _ = client; // kept alive so its socket does not close early and change the reason
}

/// A peer that reads normally is never touched by the budget, however much it is sent.
#[test]
#[serial(lnl_transport)]
fn a_reading_peer_is_never_disconnected_by_the_budget() {
    let server = Endpoint::new(|s| {
        s.max_reliable_queue_bytes_per_peer = 256 * 1024;
        s.reliable_queue_grace_ms = 1000.0;
    });
    let client = Endpoint::new(|_| {});
    let client_peer = connect_pair(&server, &client);
    let server_peer = server.connected.lock()[0].clone();
    // Far more than the budget, but the client is draining it as fast as it arrives.
    let message = vec![0xCD_u8; 2000];
    let total = 2000;
    let mut refused = 0;
    for i in 0..total {
        if server_peer.send(&message, 6, DeliveryMethod::ReliableOrdered).is_err() {
            refused += 1;
        }
        if i % 64 == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert!(wait_for(|| client.received_count() >= total - refused, DELIVERY_TIMEOUT), "got {} of {}", client.received_count(), total - refused);
    std::thread::sleep(Duration::from_millis(1500));
    assert!(server.disconnected.lock().is_empty(), "a reading peer was disconnected by the send-queue budget");
    assert!(client_peer.is_connected());
    // Every message the budget did briefly refuse under a burst is a message the sender saw fail
    // and could retry; nothing was silently dropped.
    println!("reading peer: {refused}/{total} momentarily refused under burst, none lost");
}

// A control that the budget refuses a single message larger than the whole budget: the accounting
// is on payload bytes, so a 4 KiB reliable message against a 1 KiB budget is refused every time,
// deterministically, whatever the drain speed — the property the iroh path relies on too.
#[test]
#[serial(lnl_transport)]
fn a_reliable_message_larger_than_the_whole_budget_is_always_refused() {
    let server = Endpoint::new(|s| {
        s.max_reliable_queue_bytes_per_peer = 1024;
        s.reliable_queue_grace_ms = 60_000.0; // long, so this tests the refusal, not the watchdog
    });
    let client = Endpoint::new(|_| {});
    let peer = connect_pair(&server, &client);
    let server_peer = server.connected.lock()[0].clone();
    // Small messages under the budget get through.
    server_peer.send(&vec![1u8; 500], 5, DeliveryMethod::ReliableOrdered).unwrap();
    // A message bigger than the entire budget can never fit and is refused whatever the queue state.
    for _ in 0..5 {
        assert!(matches!(server_peer.send(&vec![2u8; 4096], 5, DeliveryMethod::ReliableOrdered), Err(SendError::QueueFull { budget: 1024, .. })));
    }
    // The under-budget message still arrives; the client is unaffected.
    assert!(wait_for(|| client.received_count() >= 1, DELIVERY_TIMEOUT));
    assert_eq!(client.received.lock()[0].2.len(), 500);
    let _ = peer;
    server.manager.stop();
    client.manager.stop();
}

/// Incomplete fragment sets are bounded by BYTES, not by set count: one set may hold 65535
/// fragments, so the set count alone limits nothing useful. A sender that opens messages and
/// never finishes them must be unable to pin more than its budget.
#[test]
#[serial(lnl_transport)]
fn unfinished_fragment_sets_are_bounded_by_bytes() {
    let server = Endpoint::new(|s| {
        // 64 KiB of half-finished reassembly per peer, well under what the loop below sends.
        s.max_fragment_bytes_per_peer = 64 * 1024;
    });
    let port = server.start();
    let client = RawClient::handshake(port);
    assert!(wait_for(|| !server.connected.lock().is_empty(), HANDSHAKE_TIMEOUT));

    // Open many fragment sets and never complete any: each carries part 0 of 4, so the server
    // holds it waiting for parts it will never get.
    let payload = vec![0x5A_u8; 900];
    for fragment_id in 0..400u16 {
        let mut p = NetPacket::with_size(NetConstants::FRAGMENTED_HEADER_TOTAL_SIZE + payload.len());
        p.set_property(PacketProperty::Channeled);
        p.set_channel_id(2 * NetConstants::CHANNEL_TYPE_COUNT as u8 + DeliveryMethod::ReliableOrdered as u8);
        p.set_sequence(fragment_id);
        p.mark_fragmented();
        p.set_fragment_id(fragment_id);
        p.set_fragment_part(0);
        p.set_fragments_total(4);
        p.raw_mut()[NetConstants::FRAGMENTED_HEADER_TOTAL_SIZE..].copy_from_slice(&payload);
        client.send(p.raw().to_vec());
    }
    std::thread::sleep(Duration::from_millis(400));

    // 400 sets x ~910 bytes is ~356 KiB offered against a 64 KiB budget: the server dropped the
    // excess rather than holding it, and nothing was delivered (no set ever completed).
    assert_eq!(server.received_count(), 0, "an incomplete fragment set was delivered");
    assert!(server.disconnected.lock().is_empty(), "the peer was disconnected rather than the fragments dropped");

    // The peer is still usable: an ordinary message still gets through.
    client.send_unreliable(1, b"still here");
    assert!(
        wait_for(|| server.received.lock().iter().any(|(c, _, d)| *c == 1 && d == b"still here"), DELIVERY_TIMEOUT),
        "the peer stopped working after its fragment budget was reached"
    );
}
