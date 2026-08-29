//! Peer-to-peer "direct connect" signalling lifecycle through the real
//! `BasisServerP2PBroker::handle_p2p_message` entry point: request → accept → decline/cancel →
//! link-up (offload) → link-lost (re-arm) → disconnect teardown, plus the deny branches (instance
//! lock, target offline, self-request, pair mismatch). These build real signal frames, feed them
//! through the routing switch, and assert on what each fake peer receives on the P2P channel.

use std::sync::Arc;

use basis_network_core::SerializableBasis::BasisP2PSignalMessage;
use basis_network_core::transport::basis_network_shell::NetPeer;
use basis_network_core::transport::{DisconnectInfo, DisconnectReason};
use basis_network_core::{BasisNetworkCommons as C, NetDataReader, NetDataWriter};
use basis_network_server::NetworkServer;
use basis_network_server::core::basis_server_handle_events::BasisServerHandleEvents;
use basis_network_server::p2p::BasisServerP2PBroker as B;
use basis_network_server::security::BasisGlobalLockManager;
use basis_server_tests::support::{FakePeer, LifecycleSupport as L, MapAuthIdentity, ServerStaticsScope};
use serial_test::serial;

fn new_token() -> String {
    format!("tok-{}", uuid::Uuid::new_v4().simple())
}

/// Feed a P2P signal frame (sub-type byte + serialized body) through the real routing switch.
fn inject(from: &Arc<FakePeer>, sub: u8, other_player_id: u16, token: &str) {
    let mut w = NetDataWriter::with_capacity(96);
    w.put_byte(sub);
    BasisP2PSignalMessage { other_player_id, session_token: token.to_string(), ephemeral_public_key: None }.serialize(&mut w).expect("signal");
    B::handle_p2p_message(NetDataReader::new(w.copy_data()), &from.as_ref());
}

fn parse(data: &[u8]) -> (u8, u16, String) {
    let mut r = NetDataReader::from_slice(data);
    let sub = r.get_byte().expect("sub");
    let mut m = BasisP2PSignalMessage::default();
    m.deserialize(&mut r).expect("signal body");
    (sub, m.other_player_id, m.session_token)
}

/// Every P2P-channel signal the peer received, decoded to (sub, other, token).
fn signals(peer: &FakePeer) -> Vec<(u8, u16, String)> {
    peer.sent_on(C::P2P_CHANNEL).iter().map(|s| parse(&s.data)).collect()
}

fn received(peer: &FakePeer, sub: u8, other: u16, token: &str) -> bool {
    signals(peer).iter().any(|s| s.0 == sub && s.1 == other && s.2 == token)
}

fn register(peer: &Arc<FakePeer>) {
    NetworkServer::authenticated_peers().insert(peer.id(), peer.as_ref());
}

fn pair() -> (i32, i32, Arc<FakePeer>, Arc<FakePeer>) {
    let init_id = L::next_peer_id();
    let target_id = L::next_peer_id();
    let initiator = L::peer(init_id);
    let target = L::peer(target_id);
    register(&initiator);
    register(&target);
    (init_id, target_id, initiator, target)
}

fn remote_close() -> DisconnectInfo {
    DisconnectInfo { reason: DisconnectReason::RemoteConnectionClose, socket_error_code: 0, additional_data: NetDataReader::from_slice(&[]) }
}

// ── Request ──

#[test]
#[serial(network_statics)]
fn request_happy_path_creates_session_arms_initiator_and_forwards_to_target() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let (init_id, target_id, initiator, target) = pair();
    let token = new_token();

    inject(&initiator, C::P2P_SUB_REQUEST, target_id as u16, &token);

    assert!(B::has_session_for_tests(&token));
    assert!(received(&target, C::P2P_SUB_REQUEST, init_id as u16, &token));
    assert!(received(&initiator, C::P2P_SUB_SERVER_ARMED, target_id as u16, &token));
}

#[test]
#[serial(network_statics)]
fn request_target_offline_cancels_initiator_and_creates_no_session() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let init_id = L::next_peer_id();
    let missing_target = L::next_peer_id(); // never added to the authenticated table
    let initiator = L::peer(init_id);
    register(&initiator);
    let token = new_token();

    inject(&initiator, C::P2P_SUB_REQUEST, missing_target as u16, &token);

    assert!(!B::has_session_for_tests(&token));
    assert!(received(&initiator, C::P2P_SUB_CANCEL, missing_target as u16, &token));
}

#[test]
#[serial(network_statics)]
fn request_to_self_is_dropped_with_no_reply_and_no_session() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let id = L::next_peer_id();
    let peer = L::peer(id);
    register(&peer);
    let token = new_token();

    inject(&peer, C::P2P_SUB_REQUEST, id as u16, &token);

    assert!(!B::has_session_for_tests(&token));
    assert!(signals(&peer).is_empty());
}

#[test]
#[serial(network_statics)]
fn request_when_direct_connect_locked_for_non_admin_is_cancelled_and_creates_no_session() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let init_id = L::next_peer_id();
    let target_id = L::next_peer_id();
    let identity = MapAuthIdentity::new();
    identity.register(&format!("p2p-user-{}", uuid::Uuid::new_v4().simple()), init_id); // known peer, but granted no permissions
    NetworkServer::set_auth_identity(Some(identity));
    let initiator = L::peer(init_id);
    let target = L::peer(target_id);
    register(&initiator);
    register(&target);
    let token = new_token();

    let was_locked = BasisGlobalLockManager::direct_connect_locked();
    if !was_locked {
        BasisGlobalLockManager::toggle_direct_connect();
    }
    inject(&initiator, C::P2P_SUB_REQUEST, target_id as u16, &token);
    let session = B::has_session_for_tests(&token);
    let cancelled = received(&initiator, C::P2P_SUB_CANCEL, target_id as u16, &token);
    let target_signals = signals(&target);
    if !was_locked {
        BasisGlobalLockManager::toggle_direct_connect();
    }

    assert!(!session);
    assert!(cancelled);
    assert!(target_signals.is_empty()); // target is never told about a locked-out request
}

// ── Accept ──

#[test]
#[serial(network_statics)]
fn accept_from_target_forwards_accept_to_initiator() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let (init_id, target_id, initiator, target) = pair();
    let token = new_token();

    inject(&initiator, C::P2P_SUB_REQUEST, target_id as u16, &token);
    inject(&target, C::P2P_SUB_ACCEPT, init_id as u16, &token);

    assert!(received(&initiator, C::P2P_SUB_ACCEPT, target_id as u16, &token));
}

#[test]
#[serial(network_statics)]
fn accept_for_unknown_token_is_dropped() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let (init_id, _target_id, initiator, target) = pair();
    let token = new_token();

    inject(&target, C::P2P_SUB_ACCEPT, init_id as u16, &token); // no prior Request

    assert!(!B::has_session_for_tests(&token));
    assert!(signals(&initiator).is_empty());
}

#[test]
#[serial(network_statics)]
fn accept_from_non_target_peer_is_dropped() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let (init_id, target_id, initiator, _target) = pair();
    let stranger = L::peer(L::next_peer_id());
    register(&stranger);
    let token = new_token();

    inject(&initiator, C::P2P_SUB_REQUEST, target_id as u16, &token);
    initiator.clear_sent(); // drop the ServerArmed so only a (wrongly) forwarded Accept would show

    inject(&stranger, C::P2P_SUB_ACCEPT, init_id as u16, &token);

    assert!(!signals(&initiator).iter().any(|s| s.0 == C::P2P_SUB_ACCEPT));
}

// ── Decline / Cancel ──

#[test]
#[serial(network_statics)]
fn decline_is_relayed_to_initiator_and_drops_the_session() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let (init_id, target_id, initiator, target) = pair();
    let token = new_token();

    inject(&initiator, C::P2P_SUB_REQUEST, target_id as u16, &token);
    inject(&target, C::P2P_SUB_DECLINE, init_id as u16, &token);

    assert!(received(&initiator, C::P2P_SUB_DECLINE, target_id as u16, &token));
    assert!(!B::has_session_for_tests(&token));
}

#[test]
#[serial(network_statics)]
fn cancel_is_relayed_to_target_and_drops_the_session() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let (init_id, target_id, initiator, target) = pair();
    let token = new_token();

    inject(&initiator, C::P2P_SUB_REQUEST, target_id as u16, &token);
    inject(&initiator, C::P2P_SUB_CANCEL, target_id as u16, &token);

    assert!(received(&target, C::P2P_SUB_CANCEL, init_id as u16, &token));
    assert!(!B::has_session_for_tests(&token));
}

// ── Full round trip: link up (offload) then link lost (re-arm) ──

#[test]
#[serial(network_statics)]
fn request_accept_link_up_offloads_then_link_lost_re_arms_but_keeps_session() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let (a_id, b_id, a, b) = pair();
    let token = new_token();

    inject(&a, C::P2P_SUB_REQUEST, b_id as u16, &token);
    inject(&b, C::P2P_SUB_ACCEPT, a_id as u16, &token);

    inject(&a, C::P2P_SUB_LINK_UP, b_id as u16, &token);
    assert!(!B::is_p2p_offloaded(a_id, b_id)); // one side up only
    inject(&b, C::P2P_SUB_LINK_UP, a_id as u16, &token);
    assert!(B::is_p2p_offloaded(a_id, b_id)); // both up -> offloaded
    assert!(received(&a, C::P2P_SUB_OFFLOADED, a_id as u16, &token) || received(&a, C::P2P_SUB_OFFLOADED, b_id as u16, &token));

    // Link drops on one side: relay must resume (offload cleared) but the session survives for re-punch.
    inject(&a, C::P2P_SUB_LINK_LOST, b_id as u16, &token);
    assert!(!B::is_p2p_offloaded(a_id, b_id));
    assert!(B::has_session_for_tests(&token));
    assert!(received(&b, C::P2P_SUB_LINK_LOST, a_id as u16, &token));
}

// ── Disconnect + reconnect ──

#[test]
#[serial(network_statics)]
fn peer_disconnect_mid_session_notifies_survivor_and_tears_down_the_session() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let (init_id, target_id, initiator, target) = pair();
    let token = new_token();

    inject(&initiator, C::P2P_SUB_REQUEST, target_id as u16, &token);
    target.clear_sent(); // ignore the earlier Request forward

    // This is exactly what cleanup_peer_subsystems calls on a real disconnect.
    B::remove_peer(init_id);

    assert!(!B::has_session_for_tests(&token));
    assert!(received(&target, C::P2P_SUB_CANCEL, init_id as u16, &token));
}

#[test]
#[serial(network_statics)]
fn after_offloaded_peer_disconnects_reconnect_on_same_id_is_not_still_offloaded_and_can_request_again() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let (a_id, b_id, a, b) = pair();
    let token = new_token();

    // Full establish + offload.
    inject(&a, C::P2P_SUB_REQUEST, b_id as u16, &token);
    inject(&b, C::P2P_SUB_ACCEPT, a_id as u16, &token);
    inject(&a, C::P2P_SUB_LINK_UP, b_id as u16, &token);
    inject(&b, C::P2P_SUB_LINK_UP, a_id as u16, &token);
    assert!(B::is_p2p_offloaded(a_id, b_id));

    // A drops; the transport later hands the same id back to the rejoiner.
    B::remove_peer(a_id);
    assert!(!B::is_p2p_offloaded(a_id, b_id)); // stale offload must not linger

    let a_reconnect = L::peer(a_id);
    register(&a_reconnect);
    let token2 = new_token();
    inject(&a_reconnect, C::P2P_SUB_REQUEST, b_id as u16, &token2);

    assert!(B::has_session_for_tests(&token2));
    assert!(received(&b, C::P2P_SUB_REQUEST, a_id as u16, &token2));
}

// ── Cross-subsystem blast radius of the stale-disconnect bug ──

/// cleanup_peer_subsystems also calls the broker's remove_peer, so a stale predecessor's late
/// disconnect must not tear down the LIVE peer's active direct-connect session — the "direct
/// connect works, then dies after a rejoin" symptom.
#[test]
#[serial(network_statics)]
fn stale_disconnect_does_not_tear_down_the_live_peers_direct_connect_session() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    NetworkServer::set_auth_identity(Some(MapAuthIdentity::new()));

    let id = L::next_peer_id();
    let other_id = L::next_peer_id();
    let live = L::peer(id); // reconnected peer that owns the id now
    let other = L::peer(other_id);
    let stale = L::peer(id); // disconnected predecessor, same id
    register(&live);
    register(&other);

    // The live peer has an active direct connection to `other`.
    let token = new_token();
    inject(&live, C::P2P_SUB_REQUEST, other_id as u16, &token);
    inject(&other, C::P2P_SUB_ACCEPT, id as u16, &token);
    assert!(B::has_session_for_tests(&token));

    // The predecessor's disconnect finally lands.
    BasisServerHandleEvents::handle_peer_disconnected(stale.as_ref(), remote_close());

    assert!(B::has_session_for_tests(&token), "a stranger's disconnect tore down the live peer's direct-connect session");
}

// ── Routing ──

#[test]
#[serial(network_statics)]
fn unknown_sub_type_is_ignored_with_no_state_change() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let id = L::next_peer_id();
    let peer = L::peer(id);
    register(&peer);
    let token = new_token();

    inject(&peer, 99, id as u16, &token);

    assert!(!B::has_session_for_tests(&token));
    assert!(signals(&peer).is_empty());
}

/// A frame that ends before its body does is a protocol error from one client; it must be
/// dropped without a reply and without touching the session table.
#[test]
#[serial(network_statics)]
fn truncated_signal_frames_are_dropped_without_a_reply() {
    let _scope = ServerStaticsScope::new();
    B::reset_for_tests();
    let (_init_id, target_id, initiator, target) = pair();

    for frame in [vec![], vec![C::P2P_SUB_REQUEST], vec![C::P2P_SUB_REQUEST, target_id as u8], vec![C::P2P_SUB_REQUEST, 0, 0, 0x40]] {
        B::handle_p2p_message(NetDataReader::new(frame), &initiator.as_ref());
    }

    assert!(signals(&initiator).is_empty());
    assert!(signals(&target).is_empty());
}
