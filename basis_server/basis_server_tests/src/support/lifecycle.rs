//! The C# lifecycle test doubles: a transport shell with a controllable peer count, a pending
//! connection that records the server's verdict, a password stub, a map-backed auth identity, and
//! the scope that snapshots and restores every server static a lifecycle test touches.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use basis_error::{BasisError, BasisResult, ErrorCode};
use basis_network_core::SerializableBasis::{BytesMessage, ClientAvatarChangeMessage, ClientMetaDataMessage, LocalAvatarSyncMessage, ReadyMessage};
use basis_network_core::compression::{BasisAvatarBitPacking, BitQuality};
use basis_network_core::configuration::Configuration;
use basis_network_core::transport::basis_network_shell::{NetManager, NetManagerRef, NetStatistics};
use basis_network_core::{ConnectionRequest, NetDataReader, NetDataWriter, NetPeerRef};
use basis_network_server::NetworkServer;
use basis_network_server::auth::{IAuth, IAuthIdentity};
use basis_network_server::security::{BasisAllowList, BasisBanList};
use parking_lot::Mutex;

use super::FakePeer;

/// Transport shell whose only job is to report a controllable connected-peer count.
pub struct FakeNetManager {
    pub connected_peers: AtomicI32,
}

impl FakeNetManager {
    pub fn new(connected_peers: i32) -> Arc<Self> {
        Arc::new(Self { connected_peers: AtomicI32::new(connected_peers) })
    }

    pub fn as_ref(self: &Arc<Self>) -> NetManagerRef {
        self.clone()
    }
}

impl NetManager for FakeNetManager {
    fn start(&self, _ipv4_address: IpAddr, _ipv6_address: IpAddr, _set_port: u16) -> BasisResult<()> {
        Ok(())
    }
    fn stop(&self) {}
    fn connect(&self, _target: &str, _port: u16, _writer: &NetDataWriter) -> BasisResult<NetPeerRef> {
        Err(BasisError::permanent(ErrorCode::Unsupported, "the fake transport does not connect"))
    }
    fn send_unconnected_message(&self, _writer: &NetDataWriter, _remote_end_point: SocketAddr) -> bool {
        true
    }
    fn statistics(&self) -> NetStatistics {
        NetStatistics::default()
    }
    fn connected_peers_count(&self) -> i32 {
        self.connected_peers.load(Ordering::Relaxed)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Pending connection stand-in. Records the reject payload the server wrote and hands back a
/// preconfigured peer from `accept`, so both the deny and the accept branches are observable.
pub struct RecordingConnectionRequest {
    data: Vec<u8>,
    remote_end_point: SocketAddr,
    peer_to_return: Option<NetPeerRef>,
    pub was_accepted: AtomicBool,
    pub was_rejected: AtomicBool,
    pub reject_payload: Mutex<Vec<u8>>,
}

impl RecordingConnectionRequest {
    pub fn new(data: Vec<u8>, remote_end_point: SocketAddr, peer_to_return: Option<NetPeerRef>) -> Arc<Self> {
        Arc::new(Self { data, remote_end_point, peer_to_return, was_accepted: AtomicBool::new(false), was_rejected: AtomicBool::new(false), reject_payload: Mutex::new(Vec::new()) })
    }

    pub fn as_request(self: &Arc<Self>) -> Arc<dyn ConnectionRequest> {
        self.clone()
    }

    pub fn was_accepted(&self) -> bool {
        self.was_accepted.load(Ordering::Relaxed)
    }

    pub fn was_rejected(&self) -> bool {
        self.was_rejected.load(Ordering::Relaxed)
    }

    pub fn reject_payload(&self) -> Vec<u8> {
        self.reject_payload.lock().clone()
    }
}

impl ConnectionRequest for RecordingConnectionRequest {
    fn data(&self) -> NetDataReader {
        NetDataReader::from_slice(&self.data)
    }
    fn remote_end_point(&self) -> SocketAddr {
        self.remote_end_point
    }
    fn accept(&self) -> BasisResult<NetPeerRef> {
        self.was_accepted.store(true, Ordering::Relaxed);
        self.peer_to_return.clone().ok_or_else(|| BasisError::permanent(ErrorCode::Internal, "this request was built without a peer to accept"))
    }
    fn reject(&self, w: &NetDataWriter) -> BasisResult<()> {
        self.was_rejected.store(true, Ordering::Relaxed);
        *self.reject_payload.lock() = w.copy_data();
        Ok(())
    }
}

/// Password auth stub with a flippable verdict.
pub struct FakeAuth {
    pub result: AtomicBool,
}

impl FakeAuth {
    pub fn new(result: bool) -> Arc<Self> {
        Arc::new(Self { result: AtomicBool::new(result) })
    }

    pub fn set_result(&self, result: bool) {
        self.result.store(result, Ordering::Relaxed);
    }
}

impl IAuth for FakeAuth {
    fn is_authenticated(&self, _bytes_msg: &[u8]) -> bool {
        self.result.load(Ordering::Relaxed)
    }
}

#[derive(Default)]
struct MapAuthIdentityState {
    uuid_to_id: HashMap<String, i32>,
    id_to_uuid: HashMap<i32, String>,
    owner: HashMap<i32, NetPeerRef>,
    released: Vec<i32>,
}

/// An auth identity backed by a map the test fills in directly.
#[derive(Default)]
pub struct MapAuthIdentity {
    state: Mutex<MapAuthIdentityState>,
}

impl MapAuthIdentity {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn register(&self, uuid: &str, net_id: i32) {
        let mut s = self.state.lock();
        s.uuid_to_id.insert(uuid.to_lowercase(), net_id);
        s.id_to_uuid.insert(net_id, uuid.to_string());
    }

    pub fn register_owner(&self, uuid: &str, net_id: i32, owner: NetPeerRef) {
        self.register(uuid, net_id);
        self.state.lock().owner.insert(net_id, owner);
    }

    /// Every net id `remove_connection*` actually released, in order.
    pub fn released(&self) -> Vec<i32> {
        self.state.lock().released.clone()
    }
}

impl IAuthIdentity for MapAuthIdentity {
    fn process_connection(&self, _configuration: &Configuration, _connection_request: &Arc<dyn ConnectionRequest>, _data: NetDataReader, _net_peer: &NetPeerRef) {}
    fn de_initialize(&self) {}
    fn remove_connection(&self, net_peer: i32) {
        let mut s = self.state.lock();
        if s.id_to_uuid.remove(&net_peer).is_some() {
            s.owner.remove(&net_peer);
            s.released.push(net_peer);
        }
    }
    fn remove_connection_expected(&self, net_peer: i32, expected: &NetPeerRef) -> bool {
        let mut s = self.state.lock();
        if let Some(owner) = s.owner.get(&net_peer)
            && !basis_network_core::transport::basis_network_shell::peers_equal(owner, expected)
        {
            return false;
        }
        if s.id_to_uuid.remove(&net_peer).is_none() {
            return false;
        }
        s.owner.remove(&net_peer);
        s.released.push(net_peer);
        true
    }
    fn net_id_to_uuid(&self, peer: &NetPeerRef) -> Option<String> {
        self.state.lock().id_to_uuid.get(&peer.id()).cloned()
    }
    fn uuid_to_net_id(&self, uuid: &str) -> Option<i32> {
        self.state.lock().uuid_to_id.get(&uuid.to_lowercase()).copied()
    }
}

/// Snapshots the server statics a lifecycle test mutates and restores them on drop, removing only
/// the peers the test itself added so a leaked entry never bleeds into the next test.
pub struct ServerStaticsScope {
    server: Option<NetManagerRef>,
    configuration: Option<Arc<Configuration>>,
    auth: Option<Arc<dyn IAuth>>,
    identity: Option<Arc<dyn IAuthIdentity>>,
    allow: Option<Arc<BasisAllowList>>,
    ban: Option<Arc<BasisBanList>>,
    high_quality_length: usize,
    baseline_keys: HashSet<i32>,
}

impl ServerStaticsScope {
    pub fn new() -> Self {
        Self {
            server: NetworkServer::server(),
            configuration: NetworkServer::configuration(),
            auth: NetworkServer::auth(),
            identity: NetworkServer::auth_identity(),
            allow: NetworkServer::allow_list(),
            ban: NetworkServer::ban_list(),
            high_quality_length: NetworkServer::high_quality_length(),
            baseline_keys: NetworkServer::authenticated_peers().iter().map(|e| *e.key()).collect(),
        }
    }
}

impl Default for ServerStaticsScope {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ServerStaticsScope {
    fn drop(&mut self) {
        let added: Vec<i32> = NetworkServer::authenticated_peers().iter().map(|e| *e.key()).filter(|k| !self.baseline_keys.contains(k)).collect();
        for id in added {
            NetworkServer::authenticated_peers().remove(&id);
        }
        NetworkServer::set_server(self.server.take());
        match self.configuration.take() {
            Some(c) => NetworkServer::set_configuration((*c).clone()),
            None => NetworkServer::clear_configuration(),
        }
        NetworkServer::set_auth(self.auth.take());
        NetworkServer::set_auth_identity(self.identity.take());
        NetworkServer::set_allow_list(self.allow.take());
        NetworkServer::set_ban_list(self.ban.take());
        NetworkServer::set_high_quality_length(self.high_quality_length);
        NetworkServer::rebuild_peer_snapshot();
    }
}

/// Shared builders for connect payloads, peers and reject-payload parsing.
pub struct LifecycleSupport;

static PEER_ID_COUNTER: AtomicI32 = AtomicI32::new(30_000);

impl LifecycleSupport {
    pub const DEFAULT_IP: &'static str = "203.0.113.9";

    pub fn next_peer_id() -> i32 {
        PEER_ID_COUNTER.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn new_uuid() -> String {
        format!("conn-user-{}", uuid::Uuid::new_v4().simple())
    }

    pub fn peer(id: i32) -> Arc<FakePeer> {
        Self::peer_at(id, Self::DEFAULT_IP)
    }

    pub fn peer_at(id: i32, ip: &str) -> Arc<FakePeer> {
        FakePeer::with_address(id, ip.parse().expect("ip"))
    }

    /// A ReadyMessage that `was_deserialized_correctly` (non-empty avatar-change and sync arrays).
    pub fn make_ready(uuid: &str, display_name: &str) -> ReadyMessage {
        Self::make_ready_on(uuid, display_name, "test-platform")
    }

    pub fn make_ready_on(uuid: &str, display_name: &str, platform: &str) -> ReadyMessage {
        let payload = BasisAvatarBitPacking::convert_to_size(BitQuality::Low);
        ReadyMessage {
            player_meta_data_message: ClientMetaDataMessage { player_uuid: uuid.to_string(), player_display_name: display_name.to_string(), player_platform: platform.to_string() },
            client_avatar_change_message: ClientAvatarChangeMessage { load_mode: 0, byte_array: Some(vec![1]), local_avatar_index: 0, ..Default::default() },
            local_avatar_sync_message: LocalAvatarSyncMessage { data_quality_level: BitQuality::Low as u8, array: Some(vec![0u8; payload]), ..Default::default() },
        }
    }

    /// The exact wire order the real client writes: [version][BytesMessage auth][ReadyMessage].
    pub fn connect_payload(version: u16, auth: Option<&[u8]>, ready: Option<&ReadyMessage>) -> Vec<u8> {
        let mut w = NetDataWriter::with_capacity(64);
        w.put_ushort(version);
        if let Some(auth) = auth {
            BytesMessage.serialize(&mut w, auth).expect("auth bytes");
        }
        if let Some(ready) = ready {
            ready.clone().serialize(&mut w).expect("ready message");
        }
        w.copy_data()
    }

    pub fn request(data: Vec<u8>, accepted: Option<&Arc<FakePeer>>) -> Arc<RecordingConnectionRequest> {
        Self::request_from(data, accepted, Self::DEFAULT_IP)
    }

    pub fn request_from(data: Vec<u8>, accepted: Option<&Arc<FakePeer>>, ip: &str) -> Arc<RecordingConnectionRequest> {
        let ip: IpAddr = ip.parse().expect("ip");
        RecordingConnectionRequest::new(data, SocketAddr::new(ip, 6006), accepted.map(|p| p.as_ref()))
    }

    pub fn reject_reason(payload: &[u8]) -> String {
        NetDataReader::from_slice(payload).get_string().expect("reject reason")
    }

    /// (magic, kind, aux0, aux1, message)
    pub fn reject_structured(payload: &[u8]) -> (u32, u8, u16, u16, String) {
        let mut r = NetDataReader::from_slice(payload);
        (r.get_uint().expect("magic"), r.get_byte().expect("kind"), r.get_ushort().expect("aux0"), r.get_ushort().expect("aux1"), r.get_string().expect("message"))
    }

    /// The reason a peer was disconnected with, read from its first recorded disconnect payload.
    pub fn disconnect_reason(peer: &FakePeer) -> String {
        let data = peer.disconnect_data.lock();
        let first = data.first().expect("the peer was never disconnected with a payload");
        NetDataReader::from_slice(first).get_string().expect("disconnect reason")
    }
}
