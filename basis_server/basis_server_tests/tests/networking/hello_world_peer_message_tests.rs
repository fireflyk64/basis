//! Port of `HelloWorldPeerMessageTests.cs`.
//!
//! End-to-end peer messaging over a real server and real sockets: sixteen hello clients join
//! through the full handshake — version check, password, DID challenge/response, and the
//! metadata reply that admits the peer — and hold a full mesh of directed conversations.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use basis_hello_world_client::BasisHelloClient;
use basis_server_tests::support::{HelloWorldServerFixture, wait_until};
use serial_test::serial;

const CLIENT_COUNT: usize = 16;
const JOIN_TIMEOUT: Duration = Duration::from_secs(20);
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// The message the request asked for: "hello0A_0F" is client 10 talking to client 15.
fn message_for(from: usize, to: usize) -> String {
    format!("hello{from:02X}_{to:02X}")
}

fn join_clients(server: &HelloWorldServerFixture, count: usize) -> Vec<Arc<BasisHelloClient>> {
    let mut clients = Vec::with_capacity(count);
    for i in 0..count {
        let client = BasisHelloClient::new(&format!("Hello{i:02X}")).unwrap_or_else(|e| panic!("client {i}: {}", e.report()));
        let joined = client.connect(server.connection_string(), 0, HelloWorldServerFixture::PASSWORD, JOIN_TIMEOUT).unwrap_or_else(|e| panic!("client {i} could not connect: {}", e.report()));
        assert!(joined, "client {i} did not join the server at {} within {}s", server.connection_string(), JOIN_TIMEOUT.as_secs());
        clients.push(client);
    }
    // Distinct ids are what makes the addressing meaningful — two clients sharing one id would
    // let a mesh test pass while every message went to the wrong peer.
    let mut ids: Vec<u16> = clients.iter().map(|c| c.player_id()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), count, "player ids must be distinct");
    clients
}

fn disconnect_all(clients: &[Arc<BasisHelloClient>]) {
    for client in clients {
        client.disconnect();
    }
}

/// Sixteen clients join, then every one of them sends a distinct directed message to each of
/// the other fifteen — 240 messages across a full mesh. Every client must receive exactly the
/// fifteen addressed to it, from the right sender, and nothing addressed to anyone else.
#[test]
#[serial]
fn sixteen_clients_exchange_directed_messages_across_the_full_mesh() {
    let server = HelloWorldServerFixture::new();
    let clients = join_clients(&server, CLIENT_COUNT);

    let inbox: Vec<Arc<Mutex<Vec<(u16, String)>>>> = (0..CLIENT_COUNT).map(|_| Arc::new(Mutex::new(Vec::new()))).collect();
    for (i, client) in clients.iter().enumerate() {
        let bag = inbox[i].clone();
        client.on_text_received(Arc::new(move |sender, text, _| bag.lock().unwrap().push((sender, text))));
    }

    for from in 0..CLIENT_COUNT {
        for to in 0..CLIENT_COUNT {
            if from == to {
                continue;
            }
            clients[from].send_text(clients[to].player_id(), &message_for(from, to)).unwrap_or_else(|e| panic!("{}", e.report()));
        }
    }

    wait_until(
        || inbox.iter().all(|bag| bag.lock().unwrap().len() >= CLIENT_COUNT - 1),
        DELIVERY_TIMEOUT,
        || {
            let counts: Vec<String> = inbox.iter().map(|bag| bag.lock().unwrap().len().to_string()).collect();
            let total: usize = inbox.iter().map(|bag| bag.lock().unwrap().len()).sum();
            format!("only {total} of {} messages arrived (per client: {})", CLIENT_COUNT * (CLIENT_COUNT - 1), counts.join(", "))
        },
    );

    for to in 0..CLIENT_COUNT {
        let received = inbox[to].lock().unwrap().clone();
        // Exactly fifteen: an extra would mean the server relayed a message to someone it was
        // not addressed to, which is the failure that matters most here.
        assert_eq!(received.len(), CLIENT_COUNT - 1, "client {to} received {:?}", received);
        for from in 0..CLIENT_COUNT {
            if from == to {
                continue;
            }
            let expected = (clients[from].player_id(), message_for(from, to));
            assert!(received.contains(&expected), "client {to} did not get {expected:?}");
        }
    }

    // The example from the request, spelled out: client 10 (0x0A) to client 15 (0x0F).
    assert!(inbox[15].lock().unwrap().contains(&(clients[10].player_id(), "hello0A_0F".to_string())));

    disconnect_all(&clients);
}

/// The hello-world behaviour itself: a number passed around a ring of sixteen clients, each one
/// adding 1 and handing it to its neighbour. Sixteen hops means the volley crosses every
/// client-to-client edge of the ring and comes back to where it started.
#[test]
#[serial]
fn sixteen_clients_echo_numbers_around_the_ring() {
    let server = HelloWorldServerFixture::new();
    let clients = join_clients(&server, CLIENT_COUNT);

    const FINAL_VALUE: i32 = CLIENT_COUNT as i32;
    let hops: Arc<Mutex<Vec<(usize, u16, i32)>>> = Arc::new(Mutex::new(Vec::new()));
    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));

    for i in 0..CLIENT_COUNT {
        let this = clients[i].clone();
        let next = clients[(i + 1) % CLIENT_COUNT].clone();
        let hops = hops.clone();
        let finished = finished.clone();
        clients[i].on_number_received(Arc::new(move |sender, value, _| {
            hops.lock().unwrap().push((i, sender, value));
            if value >= FINAL_VALUE {
                finished.store(true, std::sync::atomic::Ordering::SeqCst);
            } else if let Err(e) = this.send_number(next.player_id(), value + 1) {
                eprintln!("hop {value} could not be passed on: {}", e.report());
            }
        }));
    }

    clients[0].send_number(clients[1].player_id(), 1).unwrap_or_else(|e| panic!("{}", e.report()));

    wait_until(
        || finished.load(std::sync::atomic::Ordering::SeqCst),
        DELIVERY_TIMEOUT,
        || {
            let hops = hops.lock().unwrap();
            let path: Vec<String> = hops.iter().map(|(r, _, v)| format!("c{r}={v}")).collect();
            format!("the volley stopped after {} hops: {}", hops.len(), path.join(" -> "))
        },
    );

    let mut ordered = hops.lock().unwrap().clone();
    ordered.sort_by_key(|(_, _, value)| *value);
    assert_eq!(ordered.len(), FINAL_VALUE as usize);
    for (hop, (receiver, sender, value)) in ordered.iter().enumerate() {
        assert_eq!(*value, hop as i32 + 1);
        assert_eq!(*receiver, (hop + 1) % CLIENT_COUNT);
        assert_eq!(*sender, clients[hop % CLIENT_COUNT].player_id());
    }

    disconnect_all(&clients);
}

/// Faults the C# suite left to the transport: a wrong password is refused rather than hung on,
/// and a client that never joined cannot send.
#[test]
#[serial]
fn a_wrong_password_is_refused_and_an_unjoined_client_cannot_send() {
    let server = HelloWorldServerFixture::new();
    let client = BasisHelloClient::new("Impostor").unwrap();
    let joined = client.connect(server.connection_string(), 0, "not-the-password", Duration::from_secs(5)).unwrap_or_else(|e| panic!("{}", e.report()));
    assert!(!joined, "a wrong password must not be admitted");
    let err = client.send_number(1, 1).expect_err("sending before joining must fail");
    assert!(err.report().contains("has not joined"), "{}", err.report());
    let again = client.connect(server.connection_string(), 0, HelloWorldServerFixture::PASSWORD, Duration::from_secs(1));
    assert!(again.is_err(), "a client connects once; the second attempt is refused");
    client.disconnect();
}
