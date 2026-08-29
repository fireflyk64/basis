//! Join announcements are coalesced and flushed from a worker thread, so which records reach
//! which peer is decided by join order rather than by the call that produced them. These pin that
//! rule: a peer receives exactly the joins newer than its own, because everything older was already
//! in the player list it got on arrival.

use basis_network_core::SerializableBasis::ServerReadyBatchMessage;
use basis_network_core::transport::basis_network_shell::NetPeer;
use basis_network_core::{BasisNetworkCommons, NetDataReader, NetDataWriter};
use basis_network_server::NetworkServer;
use basis_network_server::core::basis_server_handle_events::JoinBroadcast;
use basis_server_tests::support::{FakePeer, ServerStaticsScope};
use serial_test::serial;

fn record_for(player_id: u16) -> Vec<u8> {
    // The payload is an opaque ServerReadyMessage blob to the broadcaster; only its bytes and the
    // count that frames them matter here, so a minimal distinguishable record is enough.
    let mut writer = NetDataWriter::new();
    writer.put_ushort(player_id);
    writer.copy_data()
}

fn batch_in(framed: &[u8]) -> ServerReadyBatchMessage {
    let mut reader = NetDataReader::from_slice(framed);
    let mut batch = ServerReadyBatchMessage::default();
    batch.deserialize(&mut reader).expect("batch");
    batch
}

fn payload_ids_in(framed: &[u8]) -> Vec<u16> {
    let batch = batch_in(framed);
    let mut payload = NetDataReader::from_slice(&batch.payload);
    (0..batch.count).map(|_| payload.get_ushort().expect("id")).collect()
}

fn sent_batch(peer: &FakePeer) -> Vec<u8> {
    let sends = peer.sent_on(BasisNetworkCommons::CREATE_REMOTE_PLAYERS_FOR_NEW_PEER_CHANNEL);
    assert_eq!(sends.len(), 1, "expected exactly one join batch");
    sends[0].data.clone()
}

fn install(peer: &std::sync::Arc<FakePeer>) -> i64 {
    let seq = JoinBroadcast::next_seq();
    JoinBroadcast::register_peer(peer.id(), seq);
    NetworkServer::authenticated_peers().insert(peer.id(), peer.as_ref());
    seq
}

#[test]
#[serial(network_statics)]
fn flush_sends_only_joins_newer_than_each_peers_own() {
    let _scope = ServerStaticsScope::new();
    JoinBroadcast::stop();

    let early = FakePeer::new(9101);
    let middle = FakePeer::new(9102);
    let late = FakePeer::new(9103);
    install(&early);
    let middle_seq = install(&middle);
    let late_seq = install(&late);
    NetworkServer::rebuild_peer_snapshot();

    // middle and late are the two joins being announced in this flush.
    JoinBroadcast::enqueue(middle_seq, 9102, record_for(9102));
    JoinBroadcast::enqueue(late_seq, 9103, record_for(9103));

    JoinBroadcast::flush();

    // Was already here: learns about both newcomers.
    assert_eq!(payload_ids_in(&sent_batch(&early)), vec![9102, 9103]);
    // Joined between them: gets the later one only — never a copy of itself, and never the
    // records it already received in its own arrival list.
    assert_eq!(payload_ids_in(&sent_batch(&middle)), vec![9103]);
    // Newest join: everything in this batch is at or before its own arrival, so nothing is due.
    assert!(late.sent_on(BasisNetworkCommons::CREATE_REMOTE_PLAYERS_FOR_NEW_PEER_CHANNEL).is_empty());

    for id in [9101, 9102, 9103] {
        JoinBroadcast::unregister_peer(id);
    }
}

#[test]
#[serial(network_statics)]
fn flush_coalesces_many_joins_into_one_send_per_peer() {
    let _scope = ServerStaticsScope::new();
    JoinBroadcast::stop();

    let observer = FakePeer::new(9200);
    install(&observer);
    NetworkServer::rebuild_peer_snapshot();

    const JOINS: u16 = 25;
    for i in 0..JOINS {
        JoinBroadcast::enqueue(JoinBroadcast::next_seq(), 9300 + i32::from(i), record_for(9300 + i));
    }

    JoinBroadcast::flush();

    // The whole point of the coalescing: 25 joins cost one packet, not 25.
    assert_eq!(batch_in(&sent_batch(&observer)).count, JOINS);
    JoinBroadcast::unregister_peer(9200);
}

fn departure_ids_in(peer: &FakePeer) -> Vec<u16> {
    let sends = peer.sent_on(BasisNetworkCommons::DISCONNECTION_CHANNEL);
    assert_eq!(sends.len(), 1, "expected exactly one departure notice");
    let mut reader = NetDataReader::from_slice(&sends[0].data);
    let mut ids = Vec::new();
    while reader.available_bytes() >= 2 {
        ids.push(reader.get_ushort().expect("id"));
    }
    ids
}

#[test]
#[serial(network_statics)]
fn flush_coalesces_departures_into_one_send_per_peer() {
    let _scope = ServerStaticsScope::new();
    JoinBroadcast::stop();

    let watcher = FakePeer::new(9500);
    install(&watcher);
    NetworkServer::rebuild_peer_snapshot();

    JoinBroadcast::enqueue_leave(9601);
    JoinBroadcast::enqueue_leave(9602);
    JoinBroadcast::enqueue_leave(9603);

    JoinBroadcast::flush();

    // Three departures, one packet — the client reads ids until the buffer runs out.
    assert_eq!(departure_ids_in(&watcher), vec![9601, 9602, 9603]);
    JoinBroadcast::unregister_peer(9500);
}

#[test]
#[serial(network_statics)]
fn flush_drops_both_when_a_player_leaves_before_its_join_was_announced() {
    let _scope = ServerStaticsScope::new();
    JoinBroadcast::stop();

    let watcher = FakePeer::new(9700);
    install(&watcher);
    NetworkServer::rebuild_peer_snapshot();

    // Joins and leaves ride different channels, so a departure could otherwise overtake the
    // matching arrival and leave a player spawned forever. Cancelling the pair removes the race.
    const FLAPPER: i32 = 9701;
    JoinBroadcast::enqueue(JoinBroadcast::next_seq(), FLAPPER, record_for(FLAPPER as u16));
    JoinBroadcast::enqueue_leave(FLAPPER);

    JoinBroadcast::flush();

    assert_eq!(watcher.sent_count(), 0);
    JoinBroadcast::unregister_peer(9700);
}

#[test]
#[serial(network_statics)]
fn flush_with_nothing_pending_sends_nothing() {
    let _scope = ServerStaticsScope::new();
    JoinBroadcast::stop();

    let peer = FakePeer::new(9400);
    install(&peer);
    NetworkServer::rebuild_peer_snapshot();

    JoinBroadcast::flush();

    assert_eq!(peer.sent_count(), 0);
    JoinBroadcast::unregister_peer(9400);
}
