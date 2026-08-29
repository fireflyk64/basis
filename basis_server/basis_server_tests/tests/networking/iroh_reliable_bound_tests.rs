//! The iroh transport's reliable send-queue byte budget: a peer holding more reliable data than
//! its budget has further sends refused, so a client that stalls or a hostile one cannot make
//! the server buffer without limit.
//!
//! The refusal is deterministic — a message that would take the queue past the budget is refused
//! whatever the drain speed — so it is what a loopback test can prove. The watchdog that
//! disconnects a peer whose queue stays full is the same mechanism the LiteNetLib suite exercises
//! end to end against a non-acking client (`lnl_transport_tests`); QUIC's own flow control makes
//! a "non-reading" iroh peer impossible to fake over loopback, where every stream is drained
//! automatically, so it is not re-proved here.

use std::sync::Arc;
use std::time::{Duration, Instant};

use basis_network_core::configuration::IrohTransportConfig;
use basis_network_core::transport::basis_network_shell::{ConnectionRequest, DeliveryMethod, EventBasedNetListener, NetPeerRef, SendError};
use basis_network_core::transport::iroh_network_impl::IrohNetManager;
use basis_network_core::transport::{NetManager, PeerIdAllocator};
use basis_network_core::NetDataWriter;
use parking_lot::Mutex;
use serial_test::serial;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

fn wait_for(condition: impl Fn() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    condition()
}

/// A server on the iroh stack with a tiny configured reliable budget, and a client connected to
/// it. The server-side peer is what the test sends on.
struct Pair {
    server: Arc<IrohNetManager>,
    client: Arc<IrohNetManager>,
    server_peer: NetPeerRef,
}

fn connect_with_budget(reliable_budget_bytes: i32) -> Pair {
    let ids = PeerIdAllocator::new();
    let server_listener = EventBasedNetListener::new();
    let client_listener = EventBasedNetListener::new();
    let server_connected: Arc<Mutex<Vec<NetPeerRef>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let connected = server_connected.clone();
        server_listener.connection_request_event.subscribe(Arc::new(|r: Arc<dyn ConnectionRequest>| {
            r.accept().expect("accept");
        }));
        server_listener.peer_connected_event.subscribe(Arc::new(move |p| connected.lock().push(p)));
    }
    let mut config = IrohTransportConfig { relay_mode: "disabled".to_string(), ..Default::default() };
    config.max_reliable_queue_bytes_per_peer = reliable_budget_bytes;
    config.reliable_queue_grace_ms = 60_000; // long: this test proves the refusal, not the watchdog

    let server = Arc::new(IrohNetManager::with_id_allocator(server_listener, config.clone(), false, None, ids.clone()));
    let client = Arc::new(IrohNetManager::with_id_allocator(client_listener, config, false, None, ids));
    server.start_default().unwrap_or_else(|e| panic!("server start: {}", e.report()));
    client.start_default().unwrap_or_else(|e| panic!("client start: {}", e.report()));
    let target = server.connection_string();
    let mut writer = NetDataWriter::new();
    writer.put_string("iroh-budget-test").unwrap();
    client.connect(&target, 0, &writer).unwrap_or_else(|e| panic!("connect: {}", e.report()));
    assert!(wait_for(|| !server_connected.lock().is_empty(), HANDSHAKE_TIMEOUT), "the server never accepted the connection");
    let server_peer = server_connected.lock()[0].clone();
    Pair { server, client, server_peer }
}

#[test]
#[serial(network_statics)]
fn a_reliable_message_larger_than_the_whole_budget_is_refused() {
    let pair = connect_with_budget(4096);
    // A message bigger than the entire budget can never fit, whatever the queue state.
    for _ in 0..5 {
        match pair.server_peer.send(&vec![0xAB_u8; 8192], 5, DeliveryMethod::ReliableOrdered) {
            Err(SendError::QueueFull { budget, .. }) => assert_eq!(budget, 4096usize),
            other => panic!("expected QueueFull, got {other:?}"),
        }
    }
    // A message under the budget is accepted (the accounting is on payload bytes, not a blanket
    // refusal).
    assert_eq!(pair.server_peer.send(&vec![0xCD_u8; 1000], 5, DeliveryMethod::ReliableOrdered), Ok(()));
    pair.server.stop();
    pair.client.stop();
}

#[test]
#[serial(network_statics)]
fn a_configured_budget_bounds_what_one_peer_can_make_the_server_queue() {
    // The client is real and reading, so on loopback the queue drains; to prove the bound we set
    // a budget smaller than a single burst and fire faster than the sender can be scheduled.
    // Whether any individual send is refused depends on timing, but the invariant does not: the
    // server's queued reliable bytes for the peer never exceed the budget by more than one
    // in-flight message, however much is thrown at it.
    let budget: usize = 64 * 1024;
    let pair = connect_with_budget(budget as i32);
    let message = vec![0x7E_u8; 2000];
    let mut refused = 0usize;
    let mut accepted = 0usize;
    for _ in 0..1000 {
        match pair.server_peer.send(&message, 6, DeliveryMethod::ReliableOrdered) {
            Ok(()) => accepted += 1,
            Err(SendError::QueueFull { queued, budget: b }) => {
                assert_eq!(b, budget);
                assert!(queued <= budget, "refused with {queued} queued, over the {budget} budget");
                refused += 1;
            }
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
    println!("iroh reliable budget: {accepted} accepted, {refused} refused against a {budget}-byte budget");
    // Something was refused (the loop outruns a loopback drain at this budget), which is the
    // whole point: the queue is bounded rather than growing to the megabyte the loop tried to send.
    assert!(refused > 0, "the budget never engaged: {accepted} accepted, {refused} refused");
    pair.server.stop();
    pair.client.stop();
}
