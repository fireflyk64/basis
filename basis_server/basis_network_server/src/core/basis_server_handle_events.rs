//! Port of `Core/BasisServerHandleEvents.cs`: connection admission, disconnect teardown, the join
//! broadcaster, voice fan-out and the resource handlers.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use basis_error::{BasisError, BasisResult, ErrorCode};
use basis_network_core::SerializableBasis::{
    AdminRequest, AdminRequestMode, AudioSegmentDataMessage, BytesMessage, ClientAvatarChangeMessage, ClientBodyFitMessage,
    ClientMetaDataMessage, LocalLoadResource, LocalAvatarSyncMessage, ModifyResource, NetIDMessage, PlayerIdMessage, PreloadReadyMessage,
    ReadyMessage, ServerAudioSegmentMessage, ServerAvatarChangeMessage, ServerBodyFitMessage, ServerMetaDataMessage, ServerReadyBatchMessage,
    ServerReadyMessage, ServerUniqueIDMessages, UnLoadResource, VoiceReceiversMessage,
};
use basis_network_core::compression::{BasisAvatarBitPacking, BasisNetworkCompressionExtensions, BitQuality};
use basis_network_core::identity::BasisUserRestrictionMode;
use basis_network_core::mathematics::Vector3;
use basis_network_core::sanitization::basis_display_name_sanitizer::BasisDisplayNameSanitizer;
use basis_network_core::statistics::basis_network_statistics::BasisNetworkStatistics;
use basis_network_core::transport::basis_network_shell::{NetEvent, SubscriptionId, peers_equal};
use basis_network_core::{
    BNL, BasisNetworkCommons, BasisNetworkVersion, ConnectionRequest, DeliveryMethod, DisconnectInfo, NetDataReader, NetDataWriter,
    NetPacketReader, NetPeerRef,
};
use dashmap::DashMap;
use parking_lot::{Condvar, Mutex};

use crate::NetworkServer;
use crate::handlers::{BasisNetworkHandleErrorReport, BasisNetworkPIPCamera};
use crate::identity::BasisNetworkIDDatabase;
use crate::messaging::{BasisNetworkMessageProcessor, BasisServerMessageRegistry};
use crate::networking::{
    BasisImageBandwidthGovernor, BasisNetworkContentShare, BasisNetworkImageCache, BasisNetworkOwnership, BasisNetworkingGeneric,
    BasisSavedState,
};
use crate::p2p::BasisServerP2PBroker;
use crate::reduction::BasisServerReductionSystemEvents;
use crate::resources::{BasisNetworkPreloadResourceManagement, BasisNetworkResourceManagement, BasisNetworkServerLibrary};
use crate::rest_api::BasisServerInfoQuery;
use crate::security::{
    BasisAudioRangeLimitManager, BasisAvatarScaleLimitManager, BasisCrashReportStateManager, BasisGlobalLockManager,
    BasisHeadlessAudioStateManager, BasisHeadlessConnectionPolicyManager, BasisOpusFrameDurationStateManager,
    BasisOpusPacketLossStateManager, BasisPlayerModeration, BasisRejoinLockManager, BasisResourceLimitManager,
    BasisUserOpusBitrateStateManager, PermNodes, PermissionIntegration,
};

pub type OnAuthReceived = dyn Fn(NetPacketReader, NetPeerRef) + Send + Sync;
pub type OnServerReceived = dyn Fn(&NetPeerRef, NetDataReader, DeliveryMethod) + Send + Sync;

/// Coalesces "a player joined" notifications instead of fanning each one out inline.
///
/// Announcing a join costs one send per already-connected peer, and that ran on the transport
/// event thread — the same thread that dispatches auth responses. Joins are gathered here and
/// flushed from a worker thread as one ServerReadyBatchMessage per peer, on the channel the
/// client already uses for the initial player list: the event thread is free again, and K joins
/// inside a window cost one send per peer instead of K.
///
/// Ordering is by join sequence. A peer only receives records newer than its own join, because
/// everything older was already in the player list it got on arrival — that single rule covers
/// both "don't spawn a player to itself" and "don't spawn anyone twice".
pub struct JoinBroadcast;

struct Record {
    seq: i64,
    peer_id: i32,
    payload: Vec<u8>,
}

struct JoinState {
    pending: Vec<Record>,
    pending_leaves: Vec<u16>,
}

static JOIN_STATE: Mutex<JoinState> = Mutex::new(JoinState { pending: Vec::new(), pending_leaves: Vec::new() });
static JOIN_SIGNAL: Condvar = Condvar::new();
static JOIN_SIGNALLED: Mutex<bool> = Mutex::new(false);
static PEER_SEQ: LazyLock<DashMap<i32, i64>> = LazyLock::new(DashMap::new);
static JOIN_SEQ: AtomicI64 = AtomicI64::new(0);
static JOIN_RUNNING: AtomicBool = AtomicBool::new(false);
static JOIN_WORKER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

impl JoinBroadcast {
    pub const FLUSH_INTERVAL_MS: u64 = 50;

    pub fn next_seq() -> i64 {
        JOIN_SEQ.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub fn register_peer(peer_id: i32, seq: i64) {
        PEER_SEQ.insert(peer_id, seq);
    }

    pub fn registered_seq_for(peer_id: i32) -> i64 {
        PEER_SEQ.get(&peer_id).map(|s| *s).unwrap_or_else(Self::next_seq)
    }

    pub fn unregister_peer(peer_id: i32) {
        PEER_SEQ.remove(&peer_id);
    }

    fn signal() {
        *JOIN_SIGNALLED.lock() = true;
        JOIN_SIGNAL.notify_one();
    }

    pub fn start() -> BasisResult<()> {
        Self::stop();
        JOIN_RUNNING.store(true, Ordering::Release);
        let handle = std::thread::Builder::new()
            .name("JoinBroadcast".to_string())
            .spawn(Self::worker_loop)
            .map_err(|e| BasisError::wrap(basis_error::FaultKind::Transient, ErrorCode::Io, e))?;
        *JOIN_WORKER.lock() = Some(handle);
        Ok(())
    }

    pub fn stop() {
        JOIN_RUNNING.store(false, Ordering::Release);
        Self::signal();
        let worker = JOIN_WORKER.lock().take();
        if let Some(worker) = worker
            && worker.thread().id() != std::thread::current().id()
        {
            // The C# waited 500 ms; a flush that is mid-send finishes on its own afterwards.
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            while !worker.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
        let mut state = JOIN_STATE.lock();
        state.pending.clear();
        // Departures must be dropped too: a restarted server announcing the previous session's
        // leavers would tell clients to despawn players that never existed.
        state.pending_leaves.clear();
        drop(state);
        PEER_SEQ.clear();
    }

    pub fn enqueue(seq: i64, peer_id: i32, payload: Vec<u8>) {
        JOIN_STATE.lock().pending.push(Record { seq, peer_id, payload });
        Self::signal();
    }

    /// Departures are announced the same way joins are: one send per peer per flush instead of
    /// one per peer per departure. If the leaver's join is still sitting in this batch, both are
    /// dropped: nobody was ever told the player existed, so there is nothing to undo.
    pub fn enqueue_leave(peer_id: i32) {
        let mut state = JOIN_STATE.lock();
        if let Some(pending_join) = state.pending.iter().position(|r| r.peer_id == peer_id) {
            state.pending.remove(pending_join);
            return;
        }
        state.pending_leaves.push(peer_id as u16);
        drop(state);
        Self::signal();
    }

    fn worker_loop() {
        while JOIN_RUNNING.load(Ordering::Acquire) {
            {
                let mut signalled = JOIN_SIGNALLED.lock();
                if !*signalled {
                    JOIN_SIGNAL.wait_for(&mut signalled, Duration::from_millis(Self::FLUSH_INTERVAL_MS));
                }
                *signalled = false;
            }
            if !JOIN_RUNNING.load(Ordering::Acquire) {
                break;
            }
            if std::panic::catch_unwind(Self::flush).is_err() {
                BNL::log_error("JoinBroadcast flush failed: a send handler panicked");
            }
        }
    }

    pub fn flush() {
        // The batch payload ceiling is enforced HERE, on the snapshot, not inside frame(). Taking
        // a bounded oldest-first prefix leaves the tail queued exactly once and guarantees
        // progress: at least one record per flush, and every frame() suffix of the prefix fits
        // the cap by construction.
        let (batch, leaves) = {
            let mut state = JOIN_STATE.lock();
            let batch: Vec<Record> = if state.pending.is_empty() {
                Vec::new()
            } else {
                state.pending.sort_by_key(|r| r.seq);
                let mut take = 0;
                let mut payload_bytes = 0usize;
                while take < state.pending.len() {
                    payload_bytes += state.pending[take].payload.len();
                    if take > 0 && payload_bytes > ServerReadyBatchMessage::MAX_PAYLOAD_BYTES {
                        break;
                    }
                    take += 1;
                }
                state.pending.drain(..take).collect()
            };
            let leaves = std::mem::take(&mut state.pending_leaves);
            (batch, leaves)
        };
        if batch.is_empty() && leaves.is_empty() {
            return;
        }
        let peers = NetworkServer::peer_snapshot();
        if peers.is_empty() {
            return;
        }

        // Peers that joined before this whole batch take the identical bytes, which is the common
        // case; only the joiners inside the batch need a trimmed copy of their own.
        let mut framed_by_start: HashMap<usize, Vec<u8>> = HashMap::new();
        let mut sent: i64 = 0;
        let mut bytes: i64 = 0;
        for peer in peers.iter() {
            let peer_seq = PEER_SEQ.get(&peer.id()).map(|s| *s).unwrap_or(0);
            let mut start = 0;
            while start < batch.len() && batch[start].seq <= peer_seq {
                start += 1;
            }
            if start >= batch.len() {
                continue;
            }
            let framed = framed_by_start.entry(start).or_insert_with(|| Self::frame(&batch, start));
            match peer.send(framed, BasisNetworkCommons::CREATE_REMOTE_PLAYERS_FOR_NEW_PEER_CHANNEL, DeliveryMethod::ReliableOrdered) {
                Ok(()) => {
                    sent += 1;
                    bytes += framed.len() as i64;
                }
                Err(e) => BNL::log_error(format!("Failed to announce joins to peer {}: {e}", peer.id())),
            }
        }
        if sent > 0 {
            BasisNetworkStatistics::record_outbound_batch(BasisNetworkCommons::CREATE_REMOTE_PLAYERS_FOR_NEW_PEER_CHANNEL, sent, bytes);
        }
        // Departures after arrivals, so a spawn always precedes any despawn in the same flush.
        Self::flush_leaves(&peers, &leaves);
    }

    fn flush_leaves(peers: &[NetPeerRef], leaves: &[u16]) {
        if leaves.is_empty() {
            return;
        }
        // The client reads departure ids until the buffer runs out, so a batch is just the ids
        // concatenated — no framing and no client change needed.
        let mut writer = NetworkServer::rent_writer();
        for leave in leaves {
            writer.put_ushort(*leave);
        }
        let mut sent: i64 = 0;
        let mut bytes: i64 = 0;
        if NetworkServer::check_validated(&writer) {
            for peer in peers {
                // A peer in this batch is already gone; skip rather than announce its own exit.
                if leaves.iter().any(|l| i32::from(*l) == peer.id()) {
                    continue;
                }
                match peer.send_writer(&writer, BasisNetworkCommons::DISCONNECTION_CHANNEL, DeliveryMethod::ReliableOrdered) {
                    Ok(()) => {
                        sent += 1;
                        bytes += writer.length() as i64;
                    }
                    Err(e) => BNL::log_error(format!("Failed to announce departures to peer {}: {e}", peer.id())),
                }
            }
        }
        NetworkServer::return_writer(writer);
        if sent > 0 {
            BasisNetworkStatistics::record_outbound_batch(BasisNetworkCommons::DISCONNECTION_CHANNEL, sent, bytes);
        }
    }

    fn frame(batch: &[Record], start: usize) -> Vec<u8> {
        let mut payload = NetworkServer::rent_writer();
        let mut framed = NetworkServer::rent_writer();
        let mut count: u16 = 0;
        for record in &batch[start..] {
            payload.put_bytes(&record.payload);
            count = count.saturating_add(1);
        }
        let mut message = ServerReadyBatchMessage { count, payload: payload.copy_data(), was_compressed: false };
        let bytes = match message.serialize(&mut framed) {
            Ok(()) => framed.copy_data(),
            Err(e) => {
                BNL::log_error(format!("JoinBroadcast could not frame a batch of {count}: {e}"));
                Vec::new()
            }
        };
        NetworkServer::return_writer(payload);
        NetworkServer::return_writer(framed);
        bytes
    }

    /// Records queued and not yet flushed. Tests.
    pub fn pending_count() -> usize {
        JOIN_STATE.lock().pending.len()
    }
}

static AUTH_RECEIVED: LazyLock<NetEvent<OnAuthReceived>> = LazyLock::new(NetEvent::default);
static SERVER_RECEIVED: LazyLock<NetEvent<OnServerReceived>> = LazyLock::new(NetEvent::default);
static LISTENER_SUBSCRIPTIONS: Mutex<Vec<(u8, SubscriptionId)>> = Mutex::new(Vec::new());

thread_local! {
    static EXCLUDED_SET: std::cell::RefCell<HashSet<i32>> = std::cell::RefCell::new(HashSet::with_capacity(64));
}

pub struct BasisServerHandleEvents;

impl BasisServerHandleEvents {
    // ── Server Events Setup ────────────────────────────────────────────────

    pub fn subscribe_server_events() -> BasisResult<()> {
        let listener = NetworkServer::listener().ok_or_else(|| BasisError::permanent(ErrorCode::Conflict, "subscribe_server_events before setup_server"))?;
        Self::unsubscribe_server_events();
        let mut subs = LISTENER_SUBSCRIPTIONS.lock();
        subs.push((0, listener.connection_request_event.subscribe(Arc::new(Self::handle_connection_request))));
        subs.push((1, listener.peer_disconnected_event.subscribe(Arc::new(Self::handle_peer_disconnected))));
        subs.push((2, listener.network_receive_event.subscribe(Arc::new(|peer, reader, channel, dm| {
            BasisNetworkMessageProcessor::process_message(&peer, reader, channel, dm)
        }))));
        subs.push((3, listener.network_error_event.subscribe(Arc::new(Self::on_network_error))));
        drop(subs);
        BasisServerInfoQuery::subscribe();
        JoinBroadcast::start()
    }

    pub fn unsubscribe_server_events() {
        let subs: Vec<(u8, SubscriptionId)> = std::mem::take(&mut *LISTENER_SUBSCRIPTIONS.lock());
        if let Some(listener) = NetworkServer::listener() {
            for (kind, id) in subs {
                match kind {
                    0 => listener.connection_request_event.unsubscribe(id),
                    1 => listener.peer_disconnected_event.unsubscribe(id),
                    2 => listener.network_receive_event.unsubscribe(id),
                    _ => listener.network_error_event.unsubscribe(id),
                }
            }
        }
        BasisServerInfoQuery::unsubscribe();
    }

    pub fn stop_worker() {
        JoinBroadcast::stop();
        if let Some(server) = NetworkServer::server() {
            server.stop();
        }
        Self::unsubscribe_server_events();
    }

    pub fn subscribe_auth_received(handler: Arc<OnAuthReceived>) -> SubscriptionId {
        AUTH_RECEIVED.subscribe(handler)
    }

    pub fn unsubscribe_auth_received(id: SubscriptionId) {
        AUTH_RECEIVED.unsubscribe(id);
    }

    pub fn subscribe_server_received(handler: Arc<OnServerReceived>) -> SubscriptionId {
        SERVER_RECEIVED.subscribe(handler)
    }

    pub fn unsubscribe_server_received(id: SubscriptionId) {
        SERVER_RECEIVED.unsubscribe(id);
    }

    /// The C# `OnServerReceived?.Invoke(peer, reader, dm)` for the ServerBoundChannel.
    pub fn raise_server_received(peer: &NetPeerRef, reader: NetPacketReader, delivery_method: DeliveryMethod) {
        for handler in SERVER_RECEIVED.snapshot() {
            handler(peer, reader.clone(), delivery_method);
        }
    }

    pub fn handle_auth(reader: NetPacketReader, peer: &NetPeerRef) {
        for handler in AUTH_RECEIVED.snapshot() {
            handler(reader.clone(), peer.clone());
        }
    }

    // ── Network Event Handlers ─────────────────────────────────────────────

    pub fn on_network_error(end_point: std::net::SocketAddr, socket_error: i32) {
        BNL::log_error(format!("Endpoint {end_point} was reported with error {socket_error}"));
    }

    // ── Peer Connection and Disconnection ──────────────────────────────────

    /// Runs the idempotent per-peer subsystem cleanup shared by graceful disconnects and
    /// reconnect-collision eviction. Does NOT broadcast a disconnect to other peers and does NOT
    /// reset server-wide state — the caller decides whether either is appropriate.
    fn cleanup_peer_subsystems(peer: &NetPeerRef, id: i32) -> bool {
        // The auth-identity map is the primary UUID source, but it is empty when UseAuthIdentity
        // is off and can already be evicted on a reconnect collision. The stored connect metadata
        // carries the same server-computed UUID and is still present here.
        let mut uuid = NetworkServer::net_id_to_uuid(peer).unwrap_or_default();
        if uuid.is_empty()
            && let Some(meta) = BasisSavedState::get_last_player_meta_data(peer)
            && !meta.player_uuid.is_empty()
        {
            uuid = meta.player_uuid;
        }
        if let Some(identity) = NetworkServer::auth_identity() {
            identity.remove_connection_expected(id, peer);
        }

        // A predecessor's disconnect can land after a reconnect has already taken the same id.
        // Every teardown below is keyed by id alone, so running it for a peer that no longer owns
        // the slot dismantles the live peer's state instead. An id held by nobody still cleans
        // up, so a peer rejected before auth completed keeps releasing whatever partial state it
        // made.
        if let Some(holder) = NetworkServer::authenticated_peers().get(&id)
            && !peers_equal(holder.value(), peer)
        {
            return false;
        }

        if !uuid.is_empty() {
            PermissionIntegration::remove_player_meta(&uuid);
            PermissionIntegration::evict_permission_cache(&uuid);
            BasisNetworkHandleErrorReport::remove_user(&uuid);
            BasisNetworkResourceManagement::remove_peer_resources(&uuid);
        }
        BasisNetworkOwnership::remove_player_ownership(id);
        BasisSavedState::remove_player(id);
        BasisServerReductionSystemEvents::remove_player(id);
        BasisNetworkPIPCamera::remove_player(id);
        BasisNetworkContentShare::remove_player_spheres(id);
        BasisNetworkImageCache::remove_player_images(id);
        // Drops this peer's egress bucket and any replay still queued for it. Without this a
        // recycled player id would inherit the previous holder's spent budget.
        BasisImageBandwidthGovernor::remove_peer(id);
        BasisNetworkPreloadResourceManagement::remove_peer(id);
        BasisUserOpusBitrateStateManager::clear_for_peer(id);
        BasisServerP2PBroker::remove_peer(id);
        BasisNetworkIDDatabase::remove_peer(id);
        BasisNetworkingGeneric::remove_peer_scene_egress(id);
        BasisNetworkMessageProcessor::clear_peer_errors(id);
        BasisServerMessageRegistry::clear_subscription(id);
        JoinBroadcast::unregister_peer(id);

        // Value-matched, mirroring reject_with_reason: the guard above raced against a reconnect
        // that may have claimed the id since.
        NetworkServer::remove_authenticated_peer_if_same(id, peer)
    }

    pub fn handle_peer_disconnected(peer: NetPeerRef, _info: DisconnectInfo) {
        let id = peer.id();
        if Self::cleanup_peer_subsystems(&peer, id) {
            NetworkServer::rebuild_peer_snapshot();
            BNL::log(format!("Peer removed: {id}"));
        } else {
            BNL::log(format!("Peer {id} was not in AuthenticatedPeers (likely rejected before auth completed)."));
        }
        if NetworkServer::authenticated_peers().is_empty() {
            BasisNetworkIDDatabase::reset();
            BasisNetworkResourceManagement::reset();
            BasisNetworkContentShare::reset();
        }
        JoinBroadcast::enqueue_leave(id);
    }

    // ── Utility Methods ────────────────────────────────────────────────────

    pub fn reject_request_with_reason(request: &Arc<dyn ConnectionRequest>, reason: &str) {
        let mut writer = NetworkServer::rent_writer();
        let _ = writer.put_string(reason);
        if let Err(e) = request.reject(&writer) {
            BNL::log_error(format!("Reject failed: {e}"));
        }
        NetworkServer::return_writer(writer);
        BNL::log_error(format!("Rejected for reason: {reason}"));
    }

    /// Rejects a pending connection with a structured payload the client can branch on (see
    /// `BasisNetworkCommons::REJECT_KIND_*`). Older clients read it defensively as an (empty)
    /// string and fall back to a generic message.
    pub fn reject_structured(request: &Arc<dyn ConnectionRequest>, kind: u8, aux0: u16, aux1: u16, message: &str) {
        let mut writer = NetworkServer::rent_writer();
        writer.put_uint(BasisNetworkCommons::REJECT_MAGIC);
        writer.put_byte(kind);
        writer.put_ushort(aux0);
        writer.put_ushort(aux1);
        let _ = writer.put_string(message);
        if let Err(e) = request.reject(&writer) {
            BNL::log_error(format!("Reject failed: {e}"));
        }
        NetworkServer::return_writer(writer);
        BNL::log_error(format!("Rejected (kind {kind}): {message}"));
    }

    pub fn reject_version_mismatch(request: &Arc<dyn ConnectionRequest>, server_version: u16, client_version: u16) {
        let guidance = if client_version < server_version {
            "Update your Basis client to match the server."
        } else {
            "This server is running an older Basis build than your client."
        };
        Self::reject_structured(
            request,
            BasisNetworkCommons::REJECT_KIND_VERSION_MISMATCH,
            server_version,
            client_version,
            &format!("This server needs client protocol v{server_version}; your client is v{client_version}. {guidance}"),
        );
    }

    /// Rejects an already-accepted peer: evicts it (only if it still owns its slot) and disconnects
    /// with the reason as payload.
    pub fn reject_with_reason(peer: &NetPeerRef, reason: &str) {
        let id = peer.id();
        let mut writer = NetworkServer::rent_writer();
        let _ = writer.put_string(reason);
        let reason_bytes = writer.copy_data();
        NetworkServer::return_writer(writer);
        // Key-value-matched remove: "Peer already exists" rejects the duplicate, so only evict if
        // the stored peer is actually this one — otherwise we'd kick the alive peer that owns the
        // slot.
        if NetworkServer::remove_authenticated_peer_if_same(id, peer) {
            NetworkServer::rebuild_peer_snapshot();
        }
        peer.disconnect_with(&reason_bytes);
        BNL::log_error(format!("Rejected after accept with reason: {reason}"));
    }

    /// The disallow reason when headless clients are refused and `meta_data` is one.
    pub fn is_headless_disallowed(meta_data: &ClientMetaDataMessage) -> Option<String> {
        if !BasisHeadlessConnectionPolicyManager::headless_disallowed() || !BasisHeadlessConnectionPolicyManager::is_headless_client(meta_data) {
            return None;
        }
        Some(BasisHeadlessConnectionPolicyManager::DISALLOWED_REASON.to_string())
    }

    // ── Connection Handling ────────────────────────────────────────────────

    pub fn handle_connection_request(request: Arc<dyn ConnectionRequest>) {
        if BasisPlayerModeration::is_ip_banned(&request.remote_end_point().ip().to_string()) {
            Self::reject_request_with_reason(&request, "Banned IP");
            return;
        }
        let configuration = NetworkServer::configuration_or_default();
        let server_count = NetworkServer::connected_peers_count();
        if server_count >= configuration.peer_limit {
            Self::reject_structured(
                &request,
                BasisNetworkCommons::REJECT_KIND_SERVER_FULL,
                0,
                0,
                &format!("This server is full ({server_count}/{}). Please try again later.", configuration.peer_limit),
            );
            return;
        }

        let mut data = request.data();
        let Ok(client_version) = data.get_ushort() else {
            Self::reject_request_with_reason(&request, "Invalid client data.");
            return;
        };
        let server_version = BasisNetworkVersion::server_version();
        if client_version != server_version {
            Self::reject_version_mismatch(&request, server_version, client_version);
            return;
        }
        if configuration.use_auth {
            let Some(auth_bytes) = BytesMessage.deserialize(&mut data) else {
                Self::reject_request_with_reason(&request, "Malformed auth payload");
                return;
            };
            let authenticated = NetworkServer::auth().is_some_and(|auth| auth.is_authenticated(&auth_bytes));
            if !authenticated {
                Self::reject_request_with_reason(&request, "Authentication failed, Auth rejected");
                return;
            }
        } else {
            // We still want to read the data to move the needle along.
            let _ = BytesMessage.deserialize(&mut data);
        }

        if configuration.use_auth_identity {
            let Some(identity) = NetworkServer::auth_identity() else {
                Self::reject_request_with_reason(&request, "Fatal Connection Issue: no auth identity");
                return;
            };
            match request.accept() {
                Ok(new_peer) => identity.process_connection(&configuration, &request, data, &new_peer),
                Err(e) => BNL::log_error(format!("Accept failed: {e}")),
            }
            return;
        }

        let mut ready_message = ReadyMessage::default();
        let deserialized = ready_message.deserialize(&mut data).is_ok() && ready_message.was_deserialized_correctly();
        if deserialized && let Some(reason) = Self::is_headless_disallowed(&ready_message.player_meta_data_message) {
            Self::reject_request_with_reason(&request, &reason);
            return;
        }
        match request.accept() {
            Ok(new_peer) => {
                if deserialized {
                    let uuid = ready_message.player_meta_data_message.player_uuid.clone();
                    Self::on_network_accepted(&new_peer, ready_message, &uuid);
                }
            }
            Err(e) => BNL::log_error(format!("Accept failed: {e}")),
        }
    }

    pub fn on_network_accepted(new_peer: &NetPeerRef, mut ready_message: ReadyMessage, uuid: &str) {
        let peer_id = new_peer.id();
        let configuration = NetworkServer::configuration_or_default();

        // AllowList gate. Both auth paths (DID challenge + plain ReadyMessage) funnel through
        // here with a verified UUID, so this is the single point that enforces AllowList on
        // entry. Banlist is enforced separately at connection time.
        if configuration.basis_user_restriction_mode == BasisUserRestrictionMode::AllowList
            && NetworkServer::allow_list().is_some_and(|list| !list.is_allowed(uuid))
        {
            BNL::log(format!("Rejecting peer {peer_id} (UUID {uuid}) — not on allowlist."));
            Self::reject_with_reason(new_peer, "You are not on the allowlist.");
            return;
        }
        if configuration.basis_user_restriction_mode == BasisUserRestrictionMode::BanList
            && NetworkServer::ban_list().is_some_and(|list| list.is_banned(uuid))
        {
            BNL::log(format!("Rejecting peer {peer_id} (UUID {uuid}) — on banlist."));
            Self::reject_with_reason(new_peer, "You are not permitted on this server.");
            return;
        }
        // Rejoin-only lockdown: only UUIDs captured when the mode was enabled may (re)connect.
        // Config-editor admins always bypass so an admin can't lock themselves out.
        if configuration.basis_user_restriction_mode == BasisUserRestrictionMode::RejoinOnly
            && !BasisRejoinLockManager::is_allowed(uuid)
            && !PermissionIntegration::has_valid_requirement_uuid(uuid, PermNodes::CONFIGURATION_EDITOR)
        {
            BNL::log(format!("Rejecting peer {peer_id} (UUID {uuid}) — server locked to current players (rejoin-only)."));
            Self::reject_with_reason(new_peer, "The server is locked — only players already here may rejoin.");
            return;
        }

        let sanitized_display_name = BasisDisplayNameSanitizer::sanitize(&ready_message.player_meta_data_message.player_display_name);
        if sanitized_display_name.is_empty() {
            BNL::log(format!("Rejecting peer {peer_id} (UUID {uuid}) — empty or invisible display name."));
            Self::reject_with_reason(new_peer, "Choose a non-empty username.");
            return;
        }
        ready_message.player_meta_data_message.player_display_name = sanitized_display_name;

        let mut added = Self::try_add_authenticated(peer_id, new_peer);
        if !added {
            // Reconnect collision: the transport recycled this peer-id slot before the previous
            // disconnect's subsystem cleanup completed. The old entry is stale because the
            // transport never hands out two live peers with the same id — evict it and retry.
            let stale = NetworkServer::authenticated_peers().get(&peer_id).map(|p| p.value().clone());
            if let Some(stale) = stale
                && !peers_equal(&stale, new_peer)
            {
                BNL::log(format!("Reconnect collision on peer id {peer_id}; evicting stale entry and accepting new connection."));
                Self::cleanup_peer_subsystems(&stale, peer_id);
                added = Self::try_add_authenticated(peer_id, new_peer);
            }
        }
        if !added {
            Self::reject_with_reason(new_peer, "Peer already exists.");
            return;
        }

        new_peer.set_tag(Some(NetworkServer::authenticated_peer_tag()));
        NetworkServer::rebuild_peer_snapshot();
        // Claim this peer's place in the join order before anything is announced, so the "only
        // records newer than my own join" rule has a value to compare against.
        JoinBroadcast::register_peer(peer_id, JoinBroadcast::next_seq());
        BNL::log(format!("Peer connected: {peer_id}"));
        // Never assume the UUID provided by the user is good; always recalc on the server.
        ready_message.player_meta_data_message.player_uuid = uuid.to_string();
        PermissionIntegration::store_player_meta(uuid, ready_message.player_meta_data_message.clone());

        let manager = PermissionIntegration::manager();
        let mut server_meta = ServerMetaDataMessage {
            client_meta_data_message: ready_message.player_meta_data_message.clone(),
            sync_interval: configuration.bsrs_millisecond_default_interval,
            base_multiplier: configuration.bsr_base_multiplier,
            increase_rate: configuration.bsrs_increase_rate,
            slowest_send_rate: configuration.bsr_slowest_send_rate,
            peer_limit: configuration.peer_limit,
            uplink_delta_enabled: configuration.enable_uplink_avatar_delta,
            image_share_egress_megabits_per_second: configuration.image_share_egress_megabits_per_second,
            image_pickup_range_meters: configuration.image_pickup_range_meters.max(0.0),
            ..Default::default()
        };
        server_meta.set_permissions(&manager.get_all_allowed_rules(uuid), Some(&manager.get_all_denied_rules(uuid)));
        let mut writer = NetworkServer::rent_writer();
        if server_meta.serialize(&mut writer).is_ok() {
            NetworkServer::try_send(new_peer, &writer, BasisNetworkCommons::META_DATA_CHANNEL, DeliveryMethod::ReliableOrdered);
        }

        BasisServerMessageRegistry::send_supply_to(new_peer);

        if let Some(messages) = BasisNetworkIDDatabase::get_all_network_id() {
            let mut ids = ServerUniqueIDMessages { message_count: u16::try_from(messages.len()).unwrap_or(u16::MAX), messages: Some(messages) };
            writer.reset();
            if ids.serialize(&mut writer).is_ok() {
                NetworkServer::try_send(new_peer, &writer, BasisNetworkCommons::NET_ID_ASSIGNS_CHANNEL, DeliveryMethod::ReliableOrdered);
            }
        }
        NetworkServer::return_writer(writer);

        Self::send_remote_spawn_message(new_peer, ready_message);

        BasisNetworkResourceManagement::send_out_all_resources(new_peer);
        BasisNetworkServerLibrary::send_library_to_peer(new_peer);
        BasisNetworkOwnership::send_out_ownership_information(new_peer);
        BasisNetworkPIPCamera::send_pip_state_to_peer(new_peer);
        BasisNetworkContentShare::send_all_spheres_to_peer(new_peer);
        BasisNetworkImageCache::offer_cached_images_to_peer(new_peer);
        BasisGlobalLockManager::send_lock_state_to_peer(new_peer);
        BasisHeadlessAudioStateManager::send_state_to_peer(new_peer);
        BasisHeadlessConnectionPolicyManager::send_state_to_peer(new_peer);
        BasisOpusPacketLossStateManager::send_state_to_peer(new_peer);
        BasisOpusFrameDurationStateManager::send_state_to_peer(new_peer);
        BasisUserOpusBitrateStateManager::send_state_to_peer(new_peer);
        BasisUserOpusBitrateStateManager::send_global_state_to_peer(new_peer);
        BasisCrashReportStateManager::send_state_to_peer(new_peer);
        BasisAudioRangeLimitManager::send_state_to_peer(new_peer);
        BasisAvatarScaleLimitManager::send_state_to_peer(new_peer);
        BasisResourceLimitManager::send_state_to_peer(new_peer);
        BasisPlayerModeration::send_reduction_settings_to_peer(new_peer);
        BasisPlayerModeration::send_image_bandwidth_to_peer(new_peer);
        BasisPlayerModeration::send_peer_limit_to_peer(new_peer);
        Self::send_shout_state_to_peer(new_peer);
    }

    fn try_add_authenticated(peer_id: i32, peer: &NetPeerRef) -> bool {
        match NetworkServer::authenticated_peers().entry(peer_id) {
            dashmap::mapref::entry::Entry::Occupied(_) => false,
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(peer.clone());
                true
            }
        }
    }

    // ── Avatar and Voice Handling ──────────────────────────────────────────

    pub fn send_avatar_message_to_clients(mut reader: NetPacketReader, peer: &NetPeerRef) {
        // Leading kind byte multiplexes this channel — see BasisNetworkCommons::AVATAR_CHANGE_KIND_*.
        let Ok(kind) = reader.get_byte() else {
            return;
        };
        if kind == BasisNetworkCommons::AVATAR_CHANGE_KIND_BODY_FIT {
            Self::send_body_fit_message_to_clients(reader, peer);
            return;
        }
        let mut change = ClientAvatarChangeMessage::default();
        if change.deserialize(&mut reader).is_err() {
            return;
        }
        // Global avatar lock: drop the change outright — neither broadcast nor saved, so a late
        // joiner isn't handed an avatar the lock exists to keep out of the instance.
        if BasisGlobalLockManager::avatars_locked() {
            let has_bypass = NetworkServer::net_id_to_uuid(peer)
                .is_some_and(|uuid| PermissionIntegration::has_valid_requirement_uuid(&uuid, PermNodes::RESOURCE_LOCK_BYPASS_AVATAR));
            if !has_bypass {
                BNL::log(format!("Avatar loading is globally disabled. Rejected avatar change from peer {}", peer.id()));
                BasisPlayerModeration::send_back_message(peer, "Avatar loading is currently disabled by an admin.");
                return;
            }
        }
        let mut server_message =
            ServerAvatarChangeMessage { client_avatar_change_message: change.clone(), ushort_player_id: PlayerIdMessage::new(peer.id() as u16) };
        BasisSavedState::add_last_avatar_change(peer, change);
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(BasisNetworkCommons::AVATAR_CHANGE_KIND_FULL);
        if server_message.serialize(&mut writer).is_ok() {
            NetworkServer::broadcast_message_to_clients_excluding(
                &writer,
                BasisNetworkCommons::AVATAR_CHANGE_MESSAGE_CHANNEL,
                peer,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        }
        NetworkServer::return_writer(writer);
    }

    /// A body-fit-only update: merge it into this peer's saved avatar record (so a late joiner
    /// receives the current proportions) and relay it to everyone else. Deliberately not gated
    /// by the global avatar lock — nothing is being loaded.
    fn send_body_fit_message_to_clients(mut reader: NetPacketReader, peer: &NetPeerRef) {
        let mut body_fit = ClientBodyFitMessage::default();
        if body_fit.deserialize(&mut reader).is_err() {
            return;
        }
        BasisSavedState::update_body_fit(peer, &body_fit);
        let mut server_message = ServerBodyFitMessage { body_fit, ushort_player_id: PlayerIdMessage::new(peer.id() as u16) };
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(BasisNetworkCommons::AVATAR_CHANGE_KIND_BODY_FIT);
        if server_message.serialize(&mut writer).is_ok() {
            NetworkServer::broadcast_message_to_clients_excluding(
                &writer,
                BasisNetworkCommons::AVATAR_CHANGE_MESSAGE_CHANNEL,
                peer,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        }
        NetworkServer::return_writer(writer);
    }

    /// True when the global voice lock is on and this peer lacks basis.voice.lockbypass.
    pub fn is_voice_blocked_for(peer: &NetPeerRef) -> bool {
        BasisGlobalLockManager::voice_chat_locked() && !PermissionIntegration::has_valid_requirement(peer, PermNodes::VOICE_LOCK_BYPASS)
    }

    pub fn is_voice_blocked_for_uuid(uuid: &str) -> bool {
        BasisGlobalLockManager::voice_chat_locked() && !PermissionIntegration::has_valid_requirement_uuid(uuid, PermNodes::VOICE_LOCK_BYPASS)
    }

    pub fn handle_voice_message(mut reader: NetPacketReader, peer: &NetPeerRef) {
        if Self::is_voice_blocked_for(peer) {
            // Dropped silently — voice arrives ~50x/sec per speaker, so a reply or log line per
            // dropped packet would be a far worse amplification vector than the traffic itself.
            return;
        }
        let mut audio_segment = AudioSegmentDataMessage::default();
        if audio_segment.deserialize(&mut reader).is_err() {
            return;
        }
        let mut server_audio = ServerAudioSegmentMessage { audio_segment_data: audio_segment, ..Default::default() };
        Self::send_voice_message_to_clients(&mut server_audio, peer, DeliveryMethod::Unreliable);
    }

    /// Shout voice sent by a client on ShoutVoiceChannel. Only processed if the sender is
    /// authorized for shout mode; broadcast to ALL connected peers.
    pub fn handle_shout_voice_message(mut reader: NetPacketReader, peer: &NetPeerRef) {
        if !BasisSavedState::is_in_shout_mode(peer.id()) {
            BNL::log_error(format!("Peer {} sent shout voice but is not in shout mode. Ignoring.", peer.id()));
            return;
        }
        if Self::is_voice_blocked_for(peer) {
            return;
        }
        let mut audio_segment = AudioSegmentDataMessage::default();
        if audio_segment.deserialize(&mut reader).is_err() {
            return;
        }
        let mut server_audio = ServerAudioSegmentMessage { audio_segment_data: audio_segment, player_id_message: PlayerIdMessage::new(peer.id() as u16) };
        // Serialize once, then send raw to each peer.
        let mut writer = NetworkServer::rent_writer();
        if server_audio.serialize(&mut writer).is_ok() {
            let data = writer.as_read_only_span();
            let len = data.len();
            let channel = BasisNetworkCommons::SHOUT_VOICE_CHANNEL;
            for client in NetworkServer::peer_snapshot().iter() {
                if client.id() != peer.id() && client.send_unreliable_raw_merge(data, 0, len, channel, -1, 0).is_ok() {
                    BasisNetworkStatistics::record_outbound(channel, len);
                }
            }
        }
        NetworkServer::return_writer(writer);
    }

    /// Broadcasts a shout mode state change to all clients via the AdminChannel.
    pub fn broadcast_shout_mode_state(target_player_id: u16, enabled: bool, initiator_player_id: u16) {
        let mut writer = NetworkServer::rent_writer();
        let mode = if enabled { AdminRequestMode::EnableShoutMode } else { AdminRequestMode::DisableShoutMode };
        if AdminRequest::default().serialize(&mut writer, mode).is_ok() {
            writer.put_ushort(target_player_id);
            writer.put_ushort(initiator_player_id);
            NetworkServer::broadcast_message_to_clients(
                &writer,
                BasisNetworkCommons::ADMIN_CHANNEL,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        }
        NetworkServer::return_writer(writer);
    }

    /// Sends current shout mode states to a newly connected peer.
    pub fn send_shout_state_to_peer(new_peer: &NetPeerRef) {
        let shout_players = BasisSavedState::get_all_shout_mode_players();
        if shout_players.is_empty() {
            return;
        }
        let mut writer = NetworkServer::rent_writer();
        for peer_id in shout_players {
            writer.reset();
            if AdminRequest::default().serialize(&mut writer, AdminRequestMode::EnableShoutMode).is_ok() {
                writer.put_ushort(peer_id as u16);
                writer.put_ushort(peer_id as u16);
                NetworkServer::try_send(new_peer, &writer, BasisNetworkCommons::ADMIN_CHANNEL, DeliveryMethod::ReliableOrdered);
            }
        }
        NetworkServer::return_writer(writer);
    }

    pub fn send_voice_message_to_clients(audio_segment: &mut ServerAudioSegmentMessage, sender: &NetPeerRef, _method: DeliveryMethod) {
        let Some(target_peers) = BasisSavedState::get_resolved_voice_peers(sender) else {
            return;
        };
        // Snapshot under the list lock so a concurrent rebuild or remove_player can't race the
        // reads. The lock is short — just a ref copy.
        let snapshot: Vec<NetPeerRef> = target_peers.lock().clone();
        if snapshot.is_empty() {
            return;
        }
        audio_segment.player_id_message = PlayerIdMessage::new(sender.id() as u16);
        let large_id = sender.id() > i32::from(u8::MAX);
        let channel = if large_id { BasisNetworkCommons::VOICE_LARGE_CHANNEL } else { BasisNetworkCommons::VOICE_CHANNEL };

        // Serialize once, then send raw to each peer.
        let mut writer = NetworkServer::rent_writer();
        if audio_segment.serialize_sized(&mut writer, large_id).is_ok() {
            let data = writer.as_read_only_span();
            let len = data.len();
            let has_offloaded = BasisServerP2PBroker::has_offloaded_pairs();
            for client in &snapshot {
                if has_offloaded && BasisServerP2PBroker::is_p2p_offloaded(sender.id(), client.id()) {
                    continue;
                }
                if client.send_unreliable_raw_merge(data, 0, len, channel, -1, 0).is_ok() {
                    BasisNetworkStatistics::record_outbound(channel, len);
                }
            }
        }
        NetworkServer::return_writer(writer);
    }

    pub fn update_voice_receivers(mut reader: NetPacketReader, peer: &NetPeerRef, large_count: bool) {
        let mut message = VoiceReceiversMessage::default();
        if message.deserialize(&mut reader, large_count).is_err() {
            return;
        }
        BasisSavedState::add_last_voice_receivers(peer, &mut message);
    }

    /// Inverted mode: the message contains IDs to EXCLUDE. Everyone else is a recipient.
    pub fn update_voice_receivers_inverted(mut reader: NetPacketReader, peer: &NetPeerRef, large_count: bool) {
        let mut excluded = VoiceReceiversMessage::default();
        if excluded.deserialize(&mut reader, large_count).is_err() {
            return;
        }
        let sender_id = peer.id();
        let peers = BasisSavedState::get_or_create_resolved_list(sender_id);
        let mut peers = peers.lock();
        peers.clear();
        match excluded.users.as_ref() {
            None => {
                for entry in NetworkServer::authenticated_peers().iter() {
                    if *entry.key() != sender_id {
                        peers.push(entry.value().clone());
                    }
                }
            }
            Some(users) if excluded.users_length == 0 => {
                let _ = users;
                for entry in NetworkServer::authenticated_peers().iter() {
                    if *entry.key() != sender_id {
                        peers.push(entry.value().clone());
                    }
                }
            }
            Some(users) => EXCLUDED_SET.with(|set| {
                let mut set = set.borrow_mut();
                set.clear();
                for user in users.iter().take(excluded.users_length) {
                    set.insert(i32::from(*user));
                }
                for entry in NetworkServer::authenticated_peers().iter() {
                    if *entry.key() != sender_id && !set.contains(entry.key()) {
                        peers.push(entry.value().clone());
                    }
                }
            }),
        }
        drop(peers);
        excluded.return_pool();
    }

    /// Bitfield mode: each set bit at position N means playerID N is a recipient.
    /// Wire format: `[byteCount: ushort][bitfield bytes]`.
    pub fn update_voice_receivers_bitfield(mut reader: NetPacketReader, peer: &NetPeerRef) {
        let sender_id = peer.id();
        if reader.available_bytes() < 2 {
            return;
        }
        let Ok(byte_count) = reader.get_ushort() else {
            return;
        };
        if byte_count == 0 || reader.available_bytes() < usize::from(byte_count) {
            return;
        }
        let peers = BasisSavedState::get_or_create_resolved_list(sender_id);
        let mut peers = peers.lock();
        peers.clear();
        for byte_idx in 0..usize::from(byte_count) {
            let Ok(b) = reader.get_byte() else {
                break;
            };
            if b == 0 {
                continue;
            }
            let base_id = (byte_idx * 8) as i32;
            for bit in 0..8 {
                if b & (1 << bit) != 0 {
                    let player_id = base_id + bit;
                    if player_id != sender_id
                        && let Some(found) = NetworkServer::authenticated_peers().get(&player_id)
                    {
                        peers.push(found.value().clone());
                    }
                }
            }
        }
    }

    // ── Spawn and Client List Handling ─────────────────────────────────────

    pub fn send_remote_spawn_message(auth_client: &NetPeerRef, ready_message: ReadyMessage) {
        let joiner_pose = ready_message.local_avatar_sync_message.clone();
        let server_ready_message = Self::load_initial_state(auth_client, ready_message);
        Self::notify_existing_clients(server_ready_message, auth_client);
        Self::send_client_list_to_new_client(auth_client, &joiner_pose);
    }

    pub fn load_initial_state(auth_client: &NetPeerRef, ready_message: ReadyMessage) -> ServerReadyMessage {
        BasisServerReductionSystemEvents::add_message(auth_client, ready_message.local_avatar_sync_message.clone(), 0);
        BasisSavedState::add_last_ready_message(auth_client, &ready_message);
        ServerReadyMessage { local_ready_message: ready_message, player_id_message: PlayerIdMessage::new(auth_client.id() as u16) }
    }

    /// Notify existing clients about a new player (through the join broadcaster).
    pub fn notify_existing_clients(mut server_ready_message: ServerReadyMessage, auth_client: &NetPeerRef) {
        let mut writer = NetworkServer::rent_writer();
        if server_ready_message.serialize(&mut writer).is_ok() && NetworkServer::check_validated(&writer) {
            JoinBroadcast::enqueue(JoinBroadcast::registered_seq_for(auth_client.id()), auth_client.id(), writer.copy_data());
        }
        NetworkServer::return_writer(writer);
    }

    /// Tells a joining client about every player already present, batched into compressed runs
    /// rather than one packet per player.
    pub fn send_client_list_to_new_client(auth_client: &NetPeerRef, joiner_pose: &LocalAvatarSyncMessage) {
        // The joiner's own position, taken from the pose it just sent. Used to pick each player's
        // quality tier; a zero here simply means everyone is measured from the origin. A
        // short/absent payload falls back to the origin (and therefore to High for everyone).
        let viewer_position = joiner_pose
            .array
            .as_deref()
            .filter(|a| a.len() >= BasisAvatarBitPacking::WRITE_POSITION)
            .and_then(BasisNetworkCompressionExtensions::read_position)
            .unwrap_or_default();

        let peers = NetworkServer::peer_snapshot();
        let mut batch_buffer = NetworkServer::rent_writer();
        let mut send_writer = NetworkServer::rent_writer();
        let mut batched: u16 = 0;
        for peer in peers.iter() {
            if peers_equal(peer, auth_client) {
                continue;
            }
            let Some(mut message) = Self::create_server_ready_message_for_peer(peer, viewer_position) else {
                continue;
            };
            if let Err(e) = message.serialize(&mut batch_buffer) {
                BNL::log_error(format!("Failed to serialize the ready message for peer {}: {e}", peer.id()));
                continue;
            }
            batched = batched.saturating_add(1);
            if batch_buffer.length() >= ServerReadyBatchMessage::MAX_PAYLOAD_BYTES {
                Self::flush_ready_batch(auth_client, &mut batch_buffer, &mut send_writer, &mut batched);
            }
        }
        Self::flush_ready_batch(auth_client, &mut batch_buffer, &mut send_writer, &mut batched);
        NetworkServer::return_writer(send_writer);
        NetworkServer::return_writer(batch_buffer);
    }

    fn flush_ready_batch(auth_client: &NetPeerRef, batch_buffer: &mut NetDataWriter, send_writer: &mut NetDataWriter, batched: &mut u16) {
        if *batched == 0 {
            return;
        }
        let mut batch = ServerReadyBatchMessage { count: *batched, payload: batch_buffer.copy_data(), was_compressed: false };
        send_writer.reset();
        match batch.serialize(send_writer) {
            Ok(()) => NetworkServer::try_send(
                auth_client,
                send_writer,
                BasisNetworkCommons::CREATE_REMOTE_PLAYERS_FOR_NEW_PEER_CHANNEL,
                DeliveryMethod::ReliableOrdered,
            ),
            Err(e) => BNL::log_error(format!("Failed to send client list: {e}")),
        }
        batch_buffer.reset();
        *batched = 0;
    }

    /// `viewer_position` is where the joining player is; it selects the quality tier for `peer`,
    /// exactly as the steady-state send loop would.
    fn create_server_ready_message_for_peer(peer: &NetPeerRef, viewer_position: Vector3) -> Option<ServerReadyMessage> {
        let record = BasisSavedState::get_last_avatar_change_state(peer);
        let have_avatar = record.as_ref().is_some_and(|r| r.byte_array.is_some());
        let change_state = if have_avatar {
            record.unwrap_or_default()
        } else {
            BNL::log(format!(
                "No avatar state yet for peer {}; sending placeholder spawn so the remote player is created on the joining client.",
                peer.id()
            ));
            // Carry the fit through even with no avatar yet: a body-fit update can land before
            // the avatar change (recalibration mid-load).
            let (arm, leg, torso) = record.as_ref().map(|r| (r.arm_scale, r.leg_scale, r.torso_scale)).unwrap_or((1.0, 1.0, 1.0));
            ClientAvatarChangeMessage { load_mode: 0, byte_array: None, local_avatar_index: 0, arm_scale: arm, leg_scale: leg, torso_scale: torso }
        };

        // Distance-tiered: a joiner gets the same quality for this player that the reduction
        // system would pick on its next tick, instead of a full High payload for everyone.
        let sync_state = BasisServerReductionSystemEvents::try_get_join_snapshot(viewer_position, peer.id()).unwrap_or_else(|| LocalAvatarSyncMessage {
            data_quality_level: BitQuality::High as u8,
            array: Some(vec![0u8; NetworkServer::high_quality_length()]),
            additional_avatar_datas: None,
            additional_avatar_data_size: 0,
            linked_avatar_index: 0,
        });
        let meta_data = BasisSavedState::get_last_player_meta_data(peer).unwrap_or_else(|| {
            BNL::log_error("Unable to get Last Player Meta Data! Using Error Fallback");
            ClientMetaDataMessage { player_display_name: "Error".to_string(), player_uuid: String::new(), player_platform: String::new() }
        });
        Some(ServerReadyMessage {
            local_ready_message: ReadyMessage {
                local_avatar_sync_message: sync_state,
                client_avatar_change_message: change_state,
                player_meta_data_message: meta_data,
            },
            player_id_message: PlayerIdMessage::new(peer.id() as u16),
        })
    }

    // ── Network ID Generation and resources ────────────────────────────────

    pub fn net_id_assign(mut reader: NetPacketReader, peer: &NetPeerRef) {
        let mut message = NetIDMessage::default();
        if message.deserialize(&mut reader).is_err() {
            return;
        }
        // Returns a message with the ushort back to the client, or sends it to everyone if new.
        if let Err(e) = BasisNetworkIDDatabase::add_or_find_network_id(peer, &message.player_id) {
            // A limit refusal is already logged once per session by the database; logging it per
            // message would hand a capped client a log storm.
            if e.code() != ErrorCode::Limit {
                BNL::log_error(format!("NetID assignment for '{}' failed: {}", message.player_id, e.report()));
            }
        }
    }

    pub fn load_resource(mut reader: NetPacketReader, peer: &NetPeerRef, uuid: &str) {
        let mut resource = LocalLoadResource::default();
        if resource.deserialize(&mut reader).is_err() {
            return;
        }
        let is_privileged = PermissionIntegration::has_valid_requirement(peer, PermNodes::PROTECTION);
        resource.is_admin_locked = is_privileged;
        resource.uuid_of_creator = uuid.to_string();
        if !is_privileged {
            resource.persist = false;
            resource.r#static = false;
            resource.static_admin_locked = false;
        }
        match resource.mode {
            0 => {
                if BasisGlobalLockManager::props_locked() && !PermissionIntegration::has_valid_requirement_uuid(uuid, PermNodes::RESOURCE_LOCK_BYPASS_PROP) {
                    BNL::log(format!("Prop loading is globally disabled. Rejected request from {uuid}"));
                    BasisPlayerModeration::send_back_message(peer, "Prop loading is currently disabled by an admin.");
                    return;
                }
                if !PermissionIntegration::has_valid_requirement_uuid(uuid, PermNodes::RESOURCE_LOAD_PROP) {
                    BNL::log_error(format!("Invalid Request To Load Gameobject From {uuid}"));
                    return;
                }
            }
            1 => {
                if BasisGlobalLockManager::worlds_locked() && !PermissionIntegration::has_valid_requirement_uuid(uuid, PermNodes::RESOURCE_LOCK_BYPASS_WORLD) {
                    BNL::log(format!("World loading is globally disabled. Rejected request from {uuid}"));
                    BasisPlayerModeration::send_back_message(peer, "World loading is currently disabled by an admin.");
                    return;
                }
                if !PermissionIntegration::has_valid_requirement_uuid(uuid, PermNodes::RESOURCE_LOAD_WORLD) {
                    BNL::log_error(format!("Invalid Request To Load Scene From {uuid}"));
                    return;
                }
            }
            other => {
                BNL::log_error(format!("Missing Mode {other}"));
                return;
            }
        }
        // Route based on load strategy
        match resource.load_strategy {
            0 => BasisNetworkResourceManagement::load_resource(resource),
            2 => BasisNetworkPreloadResourceManagement::start_synchronized_load(resource),
            3 => BasisNetworkResourceManagement::predownload_resource(resource),
            _ => {
                BNL::log_error("Falling Back to Resource Load, Unsupported Load Strategy");
                BasisNetworkResourceManagement::load_resource(resource);
            }
        }
    }

    pub fn handle_preload_ready(mut reader: NetPacketReader, peer: &NetPeerRef) {
        let mut ready = PreloadReadyMessage::default();
        if ready.deserialize(&mut reader).is_err() {
            return;
        }
        BasisNetworkPreloadResourceManagement::handle_client_ready(&ready.loaded_net_id, peer.id(), ready.is_ready);
    }

    pub fn unload_resource(mut reader: NetPacketReader, peer: &NetPeerRef) {
        let mut unload = UnLoadResource::default();
        if !unload.deserialize(&mut reader) {
            return;
        }
        // Tier comes from the stored record, not the packet: Mode is client-supplied and is
        // never compared against the target, so a user denied world-unload could send Mode 0
        // and have the prop permission checked instead.
        let Some(target_mode) = BasisNetworkResourceManagement::ushort_network_database().get(&unload.loaded_net_id).map(|r| r.mode) else {
            BNL::log_error(format!("Trying to unload an object that does not exist! ID Provided was [{}]", unload.loaded_net_id));
            return;
        };
        match target_mode {
            0 => {
                if !PermissionIntegration::has_valid_requirement(peer, PermNodes::RESOURCE_UNLOAD_PROP) {
                    return;
                }
            }
            1 => {
                if !PermissionIntegration::has_valid_requirement(peer, PermNodes::RESOURCE_UNLOAD_WORLD) {
                    return;
                }
            }
            _ => {
                BNL::log_error(format!("Missing Mode {}", unload.mode));
                return;
            }
        }
        BasisNetworkResourceManagement::unload_resource(&mut unload, peer);
    }

    pub fn handle_modify_resource(mut reader: NetPacketReader, peer: &NetPeerRef) {
        let mut modify = ModifyResource::default();
        if modify.deserialize(&mut reader).is_err() {
            return;
        }
        // Authorization (creator or moderator) is enforced inside set_static.
        BasisNetworkResourceManagement::set_static(&mut modify, peer);
    }
}
