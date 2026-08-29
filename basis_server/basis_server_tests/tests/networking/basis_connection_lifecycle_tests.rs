//! Client↔server "direct connect" lifecycle: the full up-and-down through the real
//! `BasisServerHandleEvents` state machine — `handle_connection_request` (accept/deny),
//! `on_network_accepted` (admission gates + membership) and `handle_peer_disconnected` (teardown +
//! broadcast) — plus the reconnect/collision races. The transport, peer and connection request are
//! traits, so the whole sequence is driven synchronously with no socket.
//!
//! `DisconnectingNullPeer_DoesNotThrow` has no Rust counterpart: a peer reference cannot be null.

use std::sync::Arc;

use basis_network_core::BasisNetworkCommons as C;
use basis_network_core::compression::{BasisAvatarBitPacking, BitQuality};
use basis_network_core::configuration::Configuration;
use basis_network_core::identity::BasisUserRestrictionMode;
use basis_network_core::protocol::BasisNetworkVersion;
use basis_network_core::transport::basis_network_shell::peers_equal;
use basis_network_server::auth::IAuthIdentity;
use basis_network_core::transport::{DisconnectInfo, DisconnectReason};
use basis_network_core::{NetDataReader, NetDataWriter};
use basis_network_server::NetworkServer;
use basis_network_server::core::basis_server_handle_events::{BasisServerHandleEvents as H, JoinBroadcast};
use basis_network_server::security::{BasisAllowList, BasisBanList, BasisPlayerModeration, BasisRejoinLockManager};
use basis_server_tests::support::{FakeAuth, FakeNetManager, FakePeer, LifecycleSupport as L, MapAuthIdentity, ServerStaticsScope};
use serial_test::serial;

fn info(reason: DisconnectReason) -> DisconnectInfo {
    DisconnectInfo { reason, socket_error_code: 0, additional_data: NetDataReader::from_slice(&[]) }
}

fn stored(id: i32) -> Option<basis_network_core::NetPeerRef> {
    NetworkServer::authenticated_peers().get(&id).map(|p| p.value().clone())
}

fn assert_stored_is(id: i32, peer: &Arc<FakePeer>) {
    let stored = stored(id).expect("the peer is registered");
    assert!(peers_equal(&stored, &peer.as_ref()), "a different peer holds slot {id}");
}

struct Installed {
    manager: Arc<FakeNetManager>,
    auth: Arc<FakeAuth>,
    identity: Arc<MapAuthIdentity>,
    allow: Arc<BasisAllowList>,
    ban: Arc<BasisBanList>,
}

fn install(mode: BasisUserRestrictionMode, use_auth: bool) -> Installed {
    NetworkServer::set_configuration(Configuration { peer_limit: 100, use_auth, use_auth_identity: false, basis_user_restriction_mode: mode, ..Configuration::default() });
    let manager = FakeNetManager::new(0);
    NetworkServer::set_server(Some(manager.as_ref()));
    let auth = FakeAuth::new(true);
    NetworkServer::set_auth(Some(auth.clone()));
    let identity = MapAuthIdentity::new();
    NetworkServer::set_auth_identity(Some(identity.clone()));
    let allow = Arc::new(BasisAllowList::in_memory());
    NetworkServer::set_allow_list(Some(allow.clone()));
    let ban = Arc::new(BasisBanList::in_memory());
    NetworkServer::set_ban_list(Some(ban.clone()));
    NetworkServer::set_high_quality_length(BasisAvatarBitPacking::convert_to_size(BitQuality::High));
    Installed { manager, auth, identity, allow, ban }
}

// ── handle_connection_request: the pre-accept deny gate and the happy accept ──

#[test]
#[serial(network_statics)]
fn banned_ip_is_rejected_before_any_data_is_read() {
    let _scope = ServerStaticsScope::new();
    let s = install(BasisUserRestrictionMode::Normal, false);
    BasisPlayerModeration::set_use_file_on_disc(false);

    // Seed an IP ban the only way the server can: ban a connected player's address.
    let banned_ip = "198.51.100.23";
    let victim_id = L::next_peer_id();
    let victim_uuid = L::new_uuid();
    s.identity.register(&victim_uuid, victim_id);
    let victim = L::peer_at(victim_id, banned_ip);
    NetworkServer::authenticated_peers().insert(victim_id, victim.as_ref());

    BasisPlayerModeration::ip_ban(&victim_uuid, "seed");
    assert!(BasisPlayerModeration::is_ip_banned(banned_ip));

    let req = L::request_from(Vec::new(), None, banned_ip);
    H::handle_connection_request(req.as_request());
    let (rejected, accepted, reason) = (req.was_rejected(), req.was_accepted(), L::reject_reason(&req.reject_payload()));

    BasisPlayerModeration::unban_ip(banned_ip);
    NetworkServer::authenticated_peers().remove(&victim_id);

    assert!(rejected);
    assert!(!accepted);
    assert_eq!(reason, "Banned IP");
}

#[test]
#[serial(network_statics)]
fn server_full_is_rejected_with_structured_server_full_kind() {
    let _scope = ServerStaticsScope::new();
    let s = install(BasisUserRestrictionMode::Normal, false);
    NetworkServer::update_configuration(|c| c.peer_limit = 4);
    s.manager.connected_peers.store(4, std::sync::atomic::Ordering::Relaxed);

    let req = L::request(Vec::new(), None);
    H::handle_connection_request(req.as_request());

    assert!(req.was_rejected());
    assert!(!req.was_accepted());
    let (magic, kind, _, _, _) = L::reject_structured(&req.reject_payload());
    assert_eq!(magic, C::REJECT_MAGIC);
    assert_eq!(kind, C::REJECT_KIND_SERVER_FULL);
}

#[test]
#[serial(network_statics)]
fn missing_version_ushort_is_rejected_as_invalid_client_data() {
    let _scope = ServerStaticsScope::new();
    install(BasisUserRestrictionMode::Normal, false);

    let req = L::request(Vec::new(), None);
    H::handle_connection_request(req.as_request());

    assert!(req.was_rejected());
    assert!(!req.was_accepted());
    assert_eq!(L::reject_reason(&req.reject_payload()), "Invalid client data.");
}

#[test]
#[serial(network_statics)]
fn version_mismatch_is_rejected_with_structured_version_mismatch_kind() {
    let _scope = ServerStaticsScope::new();
    install(BasisUserRestrictionMode::Normal, false);

    let wrong = BasisNetworkVersion::server_version().wrapping_add(1);
    let req = L::request(L::connect_payload(wrong, None, None), None);
    H::handle_connection_request(req.as_request());

    assert!(req.was_rejected());
    assert!(!req.was_accepted());
    let (magic, kind, aux0, aux1, _) = L::reject_structured(&req.reject_payload());
    assert_eq!(magic, C::REJECT_MAGIC);
    assert_eq!(kind, C::REJECT_KIND_VERSION_MISMATCH);
    assert_eq!(aux0, BasisNetworkVersion::server_version());
    assert_eq!(aux1, wrong);
}

#[test]
#[serial(network_statics)]
fn malformed_auth_payload_is_rejected_when_auth_enabled() {
    let _scope = ServerStaticsScope::new();
    install(BasisUserRestrictionMode::Normal, true);

    // Correct version, then a BytesMessage length that overruns the buffer → deserialize fails.
    let mut w = NetDataWriter::with_capacity(8);
    w.put_ushort(BasisNetworkVersion::server_version());
    w.put_ushort(500); // claims 500 bytes, none follow
    let req = L::request(w.copy_data(), None);
    H::handle_connection_request(req.as_request());

    assert!(req.was_rejected());
    assert!(!req.was_accepted());
    assert_eq!(L::reject_reason(&req.reject_payload()), "Malformed auth payload");
}

#[test]
#[serial(network_statics)]
fn wrong_password_is_rejected_when_auth_enabled() {
    let _scope = ServerStaticsScope::new();
    let s = install(BasisUserRestrictionMode::Normal, true);
    s.auth.set_result(false);

    let req = L::request(L::connect_payload(BasisNetworkVersion::server_version(), Some(&[1, 2, 3]), None), None);
    H::handle_connection_request(req.as_request());

    assert!(req.was_rejected());
    assert!(!req.was_accepted());
    assert_eq!(L::reject_reason(&req.reject_payload()), "Authentication failed, Auth rejected");
}

#[test]
#[serial(network_statics)]
fn valid_ready_message_is_accepted_and_registers_the_peer() {
    let _scope = ServerStaticsScope::new();
    install(BasisUserRestrictionMode::Normal, false);

    let id = L::next_peer_id();
    let uuid = L::new_uuid();
    let peer = L::peer(id);
    let ready = L::make_ready(&uuid, "Connie");
    let req = L::request(L::connect_payload(BasisNetworkVersion::server_version(), Some(&[1]), Some(&ready)), Some(&peer));

    H::handle_connection_request(req.as_request());

    assert!(req.was_accepted());
    assert!(!req.was_rejected());
    assert_stored_is(id, &peer);
    assert!(NetworkServer::is_authenticated_peer(&peer.as_ref()));
    // The peer must have received its ServerMetaData on the metadata channel.
    assert!(!peer.sent_on(C::META_DATA_CHANNEL).is_empty());
}

// ── on_network_accepted: the post-accept admission gates and the membership bookkeeping ──

#[test]
#[serial(network_statics)]
fn allow_list_mode_unlisted_uuid_is_rejected_and_not_registered() {
    let _scope = ServerStaticsScope::new();
    install(BasisUserRestrictionMode::AllowList, false);

    let id = L::next_peer_id();
    let uuid = L::new_uuid();
    let peer = L::peer(id);

    H::on_network_accepted(&peer.as_ref(), L::make_ready(&uuid, "NotAllowed"), &uuid);

    assert!(stored(id).is_none());
    assert_eq!(peer.disconnect_calls(), 1);
    assert_eq!(L::disconnect_reason(&peer), "You are not on the allowlist.");
}

#[test]
#[serial(network_statics)]
fn allow_list_mode_listed_uuid_is_registered() {
    let _scope = ServerStaticsScope::new();
    let s = install(BasisUserRestrictionMode::AllowList, false);

    let id = L::next_peer_id();
    let uuid = L::new_uuid();
    s.allow.add_to_allowlist(&uuid).expect("allowlist");
    let peer = L::peer(id);

    H::on_network_accepted(&peer.as_ref(), L::make_ready(&uuid, "Allowed"), &uuid);

    assert_stored_is(id, &peer);
    assert_eq!(peer.disconnect_calls(), 0);
}

#[test]
#[serial(network_statics)]
fn ban_list_mode_banned_uuid_is_rejected() {
    let _scope = ServerStaticsScope::new();
    let s = install(BasisUserRestrictionMode::BanList, false);

    let id = L::next_peer_id();
    let uuid = L::new_uuid();
    s.ban.add_to_ban_list(&uuid).expect("banlist");
    let peer = L::peer(id);

    H::on_network_accepted(&peer.as_ref(), L::make_ready(&uuid, "Banned"), &uuid);

    assert!(stored(id).is_none());
    assert_eq!(peer.disconnect_calls(), 1);
    assert_eq!(L::disconnect_reason(&peer), "You are not permitted on this server.");
}

#[test]
#[serial(network_statics)]
fn rejoin_only_mode_uncaptured_uuid_is_rejected() {
    let _scope = ServerStaticsScope::new();
    install(BasisUserRestrictionMode::RejoinOnly, false);
    BasisRejoinLockManager::clear(); // nobody captured → nobody may join

    let id = L::next_peer_id();
    let uuid = L::new_uuid();
    let peer = L::peer(id);

    H::on_network_accepted(&peer.as_ref(), L::make_ready(&uuid, "Stranger"), &uuid);

    assert!(stored(id).is_none());
    assert_eq!(peer.disconnect_calls(), 1);
    assert_eq!(L::disconnect_reason(&peer), "The server is locked — only players already here may rejoin.");
}

#[test]
#[serial(network_statics)]
fn empty_display_name_is_rejected() {
    let _scope = ServerStaticsScope::new();
    install(BasisUserRestrictionMode::Normal, false);

    let id = L::next_peer_id();
    let uuid = L::new_uuid();
    let peer = L::peer(id);

    H::on_network_accepted(&peer.as_ref(), L::make_ready(&uuid, ""), &uuid);

    assert!(stored(id).is_none());
    assert_eq!(peer.disconnect_calls(), 1);
    assert_eq!(L::disconnect_reason(&peer), "Choose a non-empty username.");
}

#[test]
#[serial(network_statics)]
fn normal_mode_valid_peer_is_registered_exactly_once() {
    let _scope = ServerStaticsScope::new();
    install(BasisUserRestrictionMode::Normal, false);

    let id = L::next_peer_id();
    let uuid = L::new_uuid();
    let peer = L::peer(id);

    H::on_network_accepted(&peer.as_ref(), L::make_ready(&uuid, "Fine"), &uuid);

    assert_stored_is(id, &peer);
    assert!(NetworkServer::peer_snapshot().iter().any(|p| peers_equal(p, &peer.as_ref())));
    assert_eq!(peer.disconnect_calls(), 0);
}

#[test]
#[serial(network_statics)]
fn reconnect_collision_evicts_stale_peer_and_the_new_peer_wins_the_slot() {
    let _scope = ServerStaticsScope::new();
    install(BasisUserRestrictionMode::Normal, false);

    let id = L::next_peer_id();
    let fresh_uuid = L::new_uuid();
    let stale = L::peer(id);
    let fresh = L::peer(id); // same id, different object (recycled slot)

    // Stale peer already occupies the slot when the reconnection is accepted.
    NetworkServer::authenticated_peers().insert(id, stale.as_ref());
    NetworkServer::rebuild_peer_snapshot();

    H::on_network_accepted(&fresh.as_ref(), L::make_ready(&fresh_uuid, "Fresh"), &fresh_uuid);

    assert_stored_is(id, &fresh);
    assert_eq!(NetworkServer::authenticated_peers().iter().filter(|e| *e.key() == id).count(), 1);
}

#[test]
#[serial(network_statics)]
fn re_accepting_the_same_peer_object_is_rejected_as_already_exists() {
    let _scope = ServerStaticsScope::new();
    install(BasisUserRestrictionMode::Normal, false);

    let id = L::next_peer_id();
    let uuid = L::new_uuid();
    let peer = L::peer(id);
    NetworkServer::authenticated_peers().insert(id, peer.as_ref());
    NetworkServer::rebuild_peer_snapshot();

    // Re-admitting the identical object cannot collision-evict itself, so it is refused.
    H::on_network_accepted(&peer.as_ref(), L::make_ready(&uuid, "Twice"), &uuid);

    assert_eq!(peer.disconnect_calls(), 1);
    assert_eq!(L::disconnect_reason(&peer), "Peer already exists.");
    assert!(stored(id).is_none());
}

// ── handle_peer_disconnected: teardown, the disconnect broadcast, and never-authenticated peers ──

fn connected(identity: &MapAuthIdentity, id: i32) -> Arc<FakePeer> {
    let uuid = L::new_uuid();
    identity.register(&uuid, id);
    let peer = L::peer(id);
    NetworkServer::authenticated_peers().insert(id, peer.as_ref());
    peer
}

#[test]
#[serial(network_statics)]
fn disconnecting_authenticated_peer_removes_it_and_rebuilds_the_snapshot() {
    let _scope = ServerStaticsScope::new();
    let s = install(BasisUserRestrictionMode::Normal, false);

    let id = L::next_peer_id();
    let peer = connected(&s.identity, id);
    NetworkServer::rebuild_peer_snapshot();

    H::handle_peer_disconnected(peer.as_ref(), info(DisconnectReason::RemoteConnectionClose));

    assert!(stored(id).is_none());
    assert!(!NetworkServer::peer_snapshot().iter().any(|p| peers_equal(p, &peer.as_ref())));
}

#[test]
#[serial(network_statics)]
fn disconnect_broadcast_notifies_every_other_peer_with_the_leaver_id() {
    let _scope = ServerStaticsScope::new();
    let s = install(BasisUserRestrictionMode::Normal, false);

    let leaving_id = L::next_peer_id();
    let leaving = connected(&s.identity, leaving_id);
    let a = connected(&s.identity, L::next_peer_id());
    let b = connected(&s.identity, L::next_peer_id());
    NetworkServer::rebuild_peer_snapshot();

    // The broadcaster's queues are process-wide, so drop anything another test left pending;
    // otherwise a stale id rides along in this test's packet and is read as the leaver.
    JoinBroadcast::stop();

    H::handle_peer_disconnected(leaving.as_ref(), info(DisconnectReason::RemoteConnectionClose));
    // Departures are coalesced now, so the notice goes out on the next flush rather than inline.
    JoinBroadcast::flush();

    // Both remaining peers get one disconnect notice carrying the leaver's ushort id.
    for witness in [&a, &b] {
        let sent = witness.sent.lock();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].channel, C::DISCONNECTION_CHANNEL);
        assert_eq!(NetDataReader::from_slice(&sent[0].data).get_ushort().expect("id"), leaving_id as u16);
    }
    // The peer that left is never sent its own removal.
    assert_eq!(leaving.sent_count(), 0);
}

#[test]
#[serial(network_statics)]
fn disconnecting_never_authenticated_peer_is_a_graceful_no_op() {
    let _scope = ServerStaticsScope::new();
    install(BasisUserRestrictionMode::Normal, false);

    let id = L::next_peer_id();
    let stranger = L::peer(id); // never inserted into the authenticated table

    H::handle_peer_disconnected(stranger.as_ref(), info(DisconnectReason::ConnectionFailed));

    assert!(stored(id).is_none());
}

// ── The reconnect races ──

#[test]
#[serial(network_statics)]
fn connect_disconnect_reconnect_same_id_leaves_the_new_peer_registered() {
    let _scope = ServerStaticsScope::new();
    let s = install(BasisUserRestrictionMode::Normal, false);
    let id = L::next_peer_id();

    // Up.
    let first_uuid = L::new_uuid();
    let first = L::peer(id);
    s.identity.register(&first_uuid, id);
    H::on_network_accepted(&first.as_ref(), L::make_ready(&first_uuid, "First"), &first_uuid);
    assert_stored_is(id, &first);

    // Down.
    H::handle_peer_disconnected(first.as_ref(), info(DisconnectReason::RemoteConnectionClose));
    assert!(stored(id).is_none());

    // Up again on the recycled id with a fresh peer object.
    let second_uuid = L::new_uuid();
    let second = L::peer(id);
    s.identity.register(&second_uuid, id);
    H::on_network_accepted(&second.as_ref(), L::make_ready(&second_uuid, "Second"), &second_uuid);

    assert_stored_is(id, &second);
}

#[test]
#[serial(network_statics)]
fn stale_disconnect_after_reconnect_collision_does_not_evict_the_live_peer() {
    let _scope = ServerStaticsScope::new();
    install(BasisUserRestrictionMode::Normal, false);

    let id = L::next_peer_id();
    let stale = L::peer(id);
    let live = L::peer(id); // reconnection that already won the slot

    // Post-collision state: the live peer holds the slot; the stale peer's disconnect event is
    // still in flight and now fires with the same id.
    NetworkServer::authenticated_peers().insert(id, live.as_ref());
    NetworkServer::rebuild_peer_snapshot();

    H::handle_peer_disconnected(stale.as_ref(), info(DisconnectReason::RemoteConnectionClose));

    // Invariant: a stale peer's teardown must only remove itself, never the live peer that owns
    // the id now.
    assert_stored_is(id, &live);
}

#[test]
#[serial(network_statics)]
fn stale_disconnect_after_reconnect_collision_still_releases_its_own_auth_state() {
    let _scope = ServerStaticsScope::new();
    let s = install(BasisUserRestrictionMode::Normal, false);

    let id = L::next_peer_id();
    let stale = L::peer(id);
    let live = L::peer(id);

    s.identity.register_owner(&L::new_uuid(), id, stale.as_ref());
    NetworkServer::authenticated_peers().insert(id, live.as_ref());
    NetworkServer::rebuild_peer_snapshot();

    H::handle_peer_disconnected(stale.as_ref(), info(DisconnectReason::RemoteConnectionClose));

    assert!(s.identity.released().contains(&id));
    assert_stored_is(id, &live);
}

#[test]
#[serial(network_statics)]
fn disconnect_arriving_on_a_different_wrapper_still_tears_the_peer_down() {
    let _scope = ServerStaticsScope::new();
    let s = install(BasisUserRestrictionMode::Normal, false);

    let id = L::next_peer_id();
    let connected = L::peer(id);
    let uuid = L::new_uuid();
    s.identity.register_owner(&uuid, id, connected.as_ref());
    H::on_network_accepted(&connected.as_ref(), L::make_ready(&uuid, "Wrapped"), &uuid);
    assert!(stored(id).is_some());

    H::handle_peer_disconnected(connected.wrap().as_ref(), info(DisconnectReason::RemoteConnectionClose));

    assert!(stored(id).is_none());
    assert!(s.identity.released().contains(&id));
}

#[test]
#[serial(network_statics)]
fn stale_disconnect_does_not_release_the_live_peers_auth_state() {
    let _scope = ServerStaticsScope::new();
    let s = install(BasisUserRestrictionMode::Normal, false);

    let id = L::next_peer_id();
    let stale = L::peer(id);
    let live = L::peer(id);

    s.identity.register_owner(&L::new_uuid(), id, live.as_ref());
    NetworkServer::authenticated_peers().insert(id, live.as_ref());
    NetworkServer::rebuild_peer_snapshot();

    H::handle_peer_disconnected(stale.as_ref(), info(DisconnectReason::RemoteConnectionClose));

    assert!(!s.identity.released().contains(&id));
    let uuid = s.identity.net_id_to_uuid(&live.as_ref());
    assert!(uuid.is_some_and(|u| !u.is_empty()), "the live peer lost its identity to a stale peer's disconnect");
}

/// A connection request whose transport refuses the accept must not leave a half-registered peer
/// behind — the server logs the failure and moves on.
#[test]
#[serial(network_statics)]
fn accept_failure_from_the_transport_registers_nothing() {
    let _scope = ServerStaticsScope::new();
    install(BasisUserRestrictionMode::Normal, false);

    let uuid = L::new_uuid();
    let ready = L::make_ready(&uuid, "Ghost");
    // No peer to hand back: the transport's accept fails.
    let req = L::request(L::connect_payload(BasisNetworkVersion::server_version(), Some(&[1]), Some(&ready)), None);
    let before = NetworkServer::authenticated_peers().len();

    H::handle_connection_request(req.as_request());

    assert!(req.was_accepted(), "the gate reached accept");
    assert!(!req.was_rejected());
    assert_eq!(NetworkServer::authenticated_peers().len(), before);
}
