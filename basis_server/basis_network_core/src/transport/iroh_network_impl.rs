//! The iroh implementation of the transport abstraction — the counterpart of `LNLNetworkImpl`.
//!
//! # Mapping the Basis channel model onto QUIC
//!
//! LiteNetLib gave every message a channel (0..63) and a delivery method. iroh gives QUIC
//! streams and datagrams. The mapping, chosen so each C# delivery method keeps its guarantees:
//!
//! | DeliveryMethod      | Carrier                                                            |
//! |---------------------|--------------------------------------------------------------------|
//! | ReliableOrdered     | one unidirectional stream per (connection, direction, channel)     |
//! | ReliableSequenced   | same stream as ReliableOrdered (stronger guarantee, never dropped) |
//! | ReliableUnordered   | a fresh unidirectional stream per message (no head-of-line block)  |
//! | Unreliable          | a QUIC datagram `[channel][payload]`                               |
//! | Sequenced           | a QUIC datagram `[channel|0x40][seq:u16][payload]`, old ones dropped |
//!
//! A stream opens with a two-byte header `[kind][channel]` and then carries length-prefixed
//! frames `[len:u32][payload]`, so a channel's ordering is exactly one stream's ordering.
//!
//! # Connection handshake
//!
//! LiteNetLib carried the connect payload (protocol version, password, ready message) in its
//! connect request and the assigned peer id in the accept. Here the client opens a bidirectional
//! *control* stream right after the QUIC handshake and sends `CONNECT [len:u32][payload]`; the
//! server answers `ACCEPTED [peer_id:u16]` or `REJECTED [len:u32][data]`. The control stream then
//! carries pings (for RTT and remote clock delta) and an optional `DISCONNECT [len][data]` so a
//! disconnect reason reaches the other side, as LiteNetLib's disconnect packet did.
//!
//! # Unconnected messages
//!
//! The server-info probe used LiteNetLib's unconnected UDP messages. QUIC has no unconnected
//! traffic, so a probe is a short connection under its own ALPN (`basis-probe/1`): the client
//! writes the query on a uni stream, the server raises `NetworkReceiveUnconnectedEvent`, and the
//! handler's `send_unconnected_message` answers on a uni stream back over that same connection.
//!
//! # Threads
//!
//! Every socket, stream and timer runs on a tokio multi-thread runtime ([`IrohRuntime`]); the
//! worker count is `IrohTransportConfig.tokio_worker_threads` (0 = the core count). Listener
//! events are raised from those workers, which is the `UnsyncedEvents = true` mode the C# server
//! ran LiteNetLib in.

use std::any::Any;
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
use iroh::endpoint::{presets, Connection, IdleTimeout, QuicTransportConfig, RecvStream, SendStream, VarInt};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};
use parking_lot::{Mutex, RwLock};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::BNL;
use crate::configuration::{BasisPopulationScale, BasisTransportConfigStore, Configuration, IrohTransportConfig};
use crate::io::{NetDataReader, NetDataWriter, NetPacketReader};
use crate::protocol::BasisNetworkCommons;

use super::basis_network_shell::*;
use super::basis_network_stack_registry::{BasisNetworkStackRegistry, ServerProbeResult};
use super::connection_target::{ConnectionTarget, ConnectionTargetKeys};
use super::iroh_connection_target_parser::IrohConnectionTargetParser;
use super::lnl_connection_target_parser::LNLConnectionTargetParser;

/// ALPN for Basis client connections.
pub const BASIS_ALPN: &[u8] = b"basis/1";
/// ALPN for the unconnected server-info probe.
pub const BASIS_PROBE_ALPN: &[u8] = b"basis-probe/1";

// Control stream opcodes.
const CTL_CONNECT: u8 = 1;
const CTL_ACCEPTED: u8 = 2;
const CTL_REJECTED: u8 = 3;
const CTL_PING: u8 = 4;
const CTL_PONG: u8 = 5;
const CTL_DISCONNECT: u8 = 6;

// Data stream kinds.
const STREAM_RELIABLE_ORDERED: u8 = 1;
const STREAM_RELIABLE_UNORDERED: u8 = 2;

// Datagram header: low 6 bits channel, bit 6 = sequenced (u16 sequence follows).
const DATAGRAM_SEQUENCED_FLAG: u8 = 0x40;
const DATAGRAM_CHANNEL_MASK: u8 = 0x3F;

/// Largest single frame accepted on a reliable stream; anything over is a protocol violation.
const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// QUIC close codes.
const CLOSE_NORMAL: u32 = 0;
const CLOSE_REJECTED: u32 = 1;
const CLOSE_DISCONNECT: u32 = 2;
const CLOSE_FORCE: u32 = 3;
const CLOSE_PROTOCOL: u32 = 4;

/// The tokio runtime every iroh transport in the process runs on.
pub struct IrohRuntime;

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static RUNTIME_THREADS: AtomicI32 = AtomicI32::new(0);

impl IrohRuntime {
    /// Sets the worker thread count before the runtime is first used. 0 = automatic.
    pub fn configure_worker_threads(threads: i32) {
        RUNTIME_THREADS.store(threads, Ordering::Relaxed);
    }

    pub fn handle() -> tokio::runtime::Handle {
        RUNTIME
            .get_or_init(|| {
                let configured = RUNTIME_THREADS.load(Ordering::Relaxed);
                let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
                let threads = if configured > 0 { configured as usize } else { cores.max(1) };
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(threads)
                    .thread_name("basis-iroh")
                    .enable_all()
                    .build()
                    .expect("tokio runtime")
            })
            .handle()
            .clone()
    }

    /// Runs `fut` on the transport runtime and waits for it from any thread — including one
    /// that is itself inside another runtime, which `Runtime::block_on` would refuse.
    pub fn block_on<T: Send + 'static>(fut: impl std::future::Future<Output = T> + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        Self::handle().spawn(async move {
            let _ = tx.send(fut.await);
        });
        rx.recv().expect("transport runtime task dropped")
    }

    pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        Self::handle().spawn(fut)
    }
}

/// .NET `DateTime.UtcNow.Ticks` (100 ns since 0001-01-01), the unit `remote_time_delta` is in.
fn utc_now_ticks() -> i64 {
    const EPOCH_TICKS: i64 = 621_355_968_000_000_000;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    EPOCH_TICKS + (now.as_nanos() / 100) as i64
}

// ────────────────────────────────────────────────────────────────────────────
//  Peer
// ────────────────────────────────────────────────────────────────────────────

enum Outgoing {
    Reliable { channel: u8, ordered: bool, data: Bytes },
    Disconnect { data: Vec<u8>, code: u32 },
}

struct PeerState {
    id: i32,
    remote_id: AtomicI32,
    identity: u64,
    conn: Connection,
    remote_addr: IpAddr,
    manager: Weak<ManagerInner>,
    is_server_side: bool,
    connected: AtomicBool,
    /// Raised exactly once, by whichever path notices the close first.
    disconnect_raised: AtomicBool,
    tag: RwLock<Option<Arc<dyn Any + Send + Sync>>>,
    rtt_ms: AtomicI32,
    remote_time_delta: AtomicI64,
    last_packet: Mutex<Instant>,
    // Reliable sends, in order, drained by the sender task.
    reliable_queue: Mutex<VecDeque<Outgoing>>,
    // Unreliable sends: voice ahead of bulk, each bounded, oldest dropped when full.
    bulk_queue: Mutex<VecDeque<Bytes>>,
    priority_queue: Mutex<VecDeque<Bytes>>,
    queued_per_channel: [AtomicU32; 64],
    bulk_limit: AtomicU32,
    priority_limit: AtomicU32,
    sequenced_out: [AtomicU32; 64],
    sequenced_in: Mutex<[Option<u16>; 64]>,
    notify: Notify,
    control_tx: tokio::sync::Mutex<Option<SendStream>>,
    ping_sent_at: Mutex<Option<(i64, Instant)>>,
}

/// An iroh-backed peer. Cheap to clone; equality is by connection.
#[derive(Clone)]
pub struct IrohNetPeer {
    state: Arc<PeerState>,
}

impl IrohNetPeer {
    fn new(state: Arc<PeerState>) -> Self {
        Self { state }
    }

    /// The QUIC connection, for callers that need iroh itself (the P2P introducer).
    pub fn connection(&self) -> &Connection {
        &self.state.conn
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.state.conn.remote_id()
    }

    fn manager(&self) -> Option<Arc<ManagerInner>> {
        self.state.manager.upgrade()
    }

    fn enqueue_reliable(&self, channel: u8, ordered: bool, data: &[u8]) {
        self.state
            .reliable_queue
            .lock()
            .push_back(Outgoing::Reliable { channel, ordered, data: Bytes::copy_from_slice(data) });
        self.state.notify.notify_one();
    }

    fn enqueue_unreliable(&self, channel: u8, sequenced: bool, data: &[u8]) {
        let Some(manager) = self.manager() else { return };
        let mut frame = BytesMut::with_capacity(data.len() + 3);
        if sequenced {
            let seq = self.state.sequenced_out[usize::from(channel)].fetch_add(1, Ordering::Relaxed) as u16;
            frame.extend_from_slice(&[channel | DATAGRAM_SEQUENCED_FLAG]);
            frame.extend_from_slice(&seq.to_le_bytes());
        } else {
            frame.extend_from_slice(&[channel]);
        }
        frame.extend_from_slice(data);
        let frame = frame.freeze();

        let priority = manager.priority_channels.get(usize::from(channel)).copied().unwrap_or(false);
        let (queue, limit, dropped) = if priority {
            (&self.state.priority_queue, self.state.priority_limit.load(Ordering::Relaxed), &manager.priority_dropped)
        } else {
            (&self.state.bulk_queue, self.state.bulk_limit.load(Ordering::Relaxed), &manager.unreliable_dropped)
        };
        {
            let mut q = queue.lock();
            while limit > 0 && q.len() >= limit as usize {
                if let Some(old) = q.pop_front() {
                    let old_channel = old[0] & DATAGRAM_CHANNEL_MASK;
                    self.state.queued_per_channel[usize::from(old_channel)].fetch_sub(1, Ordering::Relaxed);
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
            q.push_back(frame);
        }
        self.state.queued_per_channel[usize::from(channel)].fetch_add(1, Ordering::Relaxed);
        self.state.notify.notify_one();
    }
}

impl NetPeer for IrohNetPeer {
    fn disconnect(&self) {
        self.disconnect_with(&[]);
    }

    fn disconnect_with(&self, data: &[u8]) {
        if !self.state.connected.swap(false, Ordering::SeqCst) {
            return;
        }
        self.state.reliable_queue.lock().push_back(Outgoing::Disconnect { data: data.to_vec(), code: CLOSE_DISCONNECT });
        self.state.notify.notify_one();
    }

    fn disconnect_force(&self) {
        self.state.connected.store(false, Ordering::SeqCst);
        self.state.conn.close(VarInt::from_u32(CLOSE_FORCE), b"force");
        self.state.notify.notify_one();
    }

    fn send(&self, data: &[u8], channel_number: u8, delivery_method: DeliveryMethod) {
        if !self.state.connected.load(Ordering::Relaxed) || channel_number >= BasisNetworkCommons::TOTAL_CHANNELS {
            return;
        }
        match delivery_method {
            DeliveryMethod::ReliableOrdered | DeliveryMethod::ReliableSequenced => self.enqueue_reliable(channel_number, true, data),
            DeliveryMethod::ReliableUnordered => self.enqueue_reliable(channel_number, false, data),
            DeliveryMethod::Unreliable => {
                if data.len() > self.mtu() as usize {
                    panic!("Unreliable payload of {} bytes exceeds the {} byte datagram limit; the transport cannot fragment it", data.len(), self.mtu());
                }
                self.enqueue_unreliable(channel_number, false, data)
            }
            DeliveryMethod::Sequenced => {
                if data.len() + 2 > self.mtu() as usize {
                    panic!("Sequenced payload of {} bytes exceeds the {} byte datagram limit; the transport cannot fragment it", data.len(), self.mtu());
                }
                self.enqueue_unreliable(channel_number, true, data)
            }
        }
    }

    fn send_unreliable_raw_merge(&self, data: &[u8], offset: usize, length: usize, channel_number: u8, patch_offset: i32, patch_value: u8) {
        let slice = &data[offset..offset + length];
        if patch_offset >= 0 && (patch_offset as usize) < length {
            let mut patched = slice.to_vec();
            patched[patch_offset as usize] = patch_value;
            self.send(&patched, channel_number, DeliveryMethod::Unreliable);
        } else {
            self.send(slice, channel_number, DeliveryMethod::Unreliable);
        }
    }

    fn get_packets_count_in_queue(&self, channel: u8, delivery_method: DeliveryMethod) -> i32 {
        match delivery_method {
            DeliveryMethod::Unreliable | DeliveryMethod::Sequenced => {
                self.state.queued_per_channel.get(usize::from(channel)).map(|c| c.load(Ordering::Relaxed) as i32).unwrap_or(0)
            }
            _ => self.state.reliable_queue.lock().len() as i32,
        }
    }

    fn id(&self) -> i32 {
        self.state.id
    }

    fn address(&self) -> IpAddr {
        self.state.remote_addr
    }

    fn remote_id(&self) -> i32 {
        self.state.remote_id.load(Ordering::Relaxed)
    }

    fn round_trip_time(&self) -> i32 {
        self.state.rtt_ms.load(Ordering::Relaxed)
    }

    fn time_since_last_packet(&self) -> f32 {
        self.state.last_packet.lock().elapsed().as_secs_f32() * 1000.0
    }

    fn remote_time_delta(&self) -> i64 {
        self.state.remote_time_delta.load(Ordering::Relaxed)
    }

    fn mtu(&self) -> i32 {
        // Header is 1 byte (unreliable) or 3 (sequenced); report the tighter bound.
        self.state.conn.max_datagram_size().map(|m| m.saturating_sub(3) as i32).unwrap_or(BasisNetworkCommons::MAX_UNFRAGMENTED_PAYLOAD)
    }

    fn tag(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.state.tag.read().clone()
    }

    fn set_tag(&self, tag: Option<Arc<dyn Any + Send + Sync>>) {
        *self.state.tag.write() = tag;
    }

    fn identity(&self) -> u64 {
        self.state.identity
    }

    fn is_connected(&self) -> bool {
        self.state.connected.load(Ordering::Relaxed)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// ────────────────────────────────────────────────────────────────────────────
//  Connection request
// ────────────────────────────────────────────────────────────────────────────

struct IrohConnectionRequest {
    manager: Arc<ManagerInner>,
    conn: Connection,
    control_tx: tokio::sync::Mutex<Option<SendStream>>,
    control_rx: tokio::sync::Mutex<Option<RecvStream>>,
    data: Vec<u8>,
    remote: SocketAddr,
    decided: AtomicBool,
}

impl ConnectionRequest for IrohConnectionRequest {
    fn data(&self) -> NetDataReader {
        NetDataReader::from_slice(&self.data)
    }

    fn remote_end_point(&self) -> SocketAddr {
        self.remote
    }

    fn accept(&self) -> NetPeerRef {
        if self.decided.swap(true, Ordering::SeqCst) {
            panic!("ConnectionRequest already accepted or rejected");
        }
        let peer = self.manager.admit(self.conn.clone(), self.remote.ip(), true, 0);
        let tx = self.control_tx.blocking_lock().take();
        let rx = self.control_rx.blocking_lock().take();
        let state = peer.state.clone();
        let manager = self.manager.clone();
        IrohRuntime::spawn(async move {
            if let Some(mut tx) = tx {
                let mut msg = vec![CTL_ACCEPTED];
                msg.extend_from_slice(&(state.id as u16).to_le_bytes());
                if tx.write_all(&msg).await.is_err() {
                    state.conn.close(VarInt::from_u32(CLOSE_PROTOCOL), b"accept write failed");
                    return;
                }
                *state.control_tx.lock().await = Some(tx);
            }
            manager.run_peer(state, rx).await;
        });
        Arc::new(peer)
    }

    fn reject(&self, w: &NetDataWriter) {
        if self.decided.swap(true, Ordering::SeqCst) {
            return;
        }
        let data = w.copy_data();
        let conn = self.conn.clone();
        let tx = self.control_tx.blocking_lock().take();
        IrohRuntime::spawn(async move {
            if let Some(mut tx) = tx {
                let mut msg = vec![CTL_REJECTED];
                msg.extend_from_slice(&(data.len() as u32).to_le_bytes());
                msg.extend_from_slice(&data);
                let _ = tx.write_all(&msg).await;
                let _ = tx.finish();
                // Give the frame a moment to leave before the close races it.
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            conn.close(VarInt::from_u32(CLOSE_REJECTED), b"rejected");
        });
    }
}

// ────────────────────────────────────────────────────────────────────────────
//  Manager
// ────────────────────────────────────────────────────────────────────────────

struct ManagerInner {
    listener: Arc<EventBasedNetListener>,
    endpoint: RwLock<Option<Endpoint>>,
    secret_key: SecretKey,
    transport_config: IrohTransportConfig,
    enable_statistics: bool,
    priority_channels: Vec<bool>,
    peers: DashMap<i32, IrohNetPeer>,
    /// Ids handed back by departed peers, reused lowest-first like LiteNetLib.
    free_ids: Mutex<BTreeSet<i32>>,
    next_id: AtomicI32,
    next_identity: AtomicU64,
    running: AtomicBool,
    accept_task: Mutex<Option<JoinHandle<()>>>,
    packets_sent: AtomicU64,
    packets_received: AtomicU64,
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
    unreliable_dropped: AtomicI64,
    priority_dropped: AtomicI64,
    /// Probe connections awaiting a reply, keyed by the remote address the handler saw.
    probe_replies: DashMap<SocketAddr, Connection>,
    weak_self: RwLock<Weak<ManagerInner>>,
}

/// The iroh-backed [`NetManager`].
pub struct IrohNetManager {
    inner: Arc<ManagerInner>,
}

impl IrohNetManager {
    /// The stack registry's factory: builds a manager over `listener` using the iroh transport
    /// sidecar for its tuning and `configuration` for what the C# manager read from it.
    pub fn create(listener: Arc<EventBasedNetListener>, configuration: &Configuration) -> Option<NetManagerRef> {
        let transport = BasisTransportConfigStore::get::<IrohTransportConfig>(BasisNetworkStackRegistry::IROH_ID);
        Some(Arc::new(Self::new(listener, transport, configuration.enable_statistics, None)))
    }

    /// Builds a manager with an explicit secret key (tests, clients that keep an identity).
    pub fn new(listener: Arc<EventBasedNetListener>, transport: IrohTransportConfig, enable_statistics: bool, secret_key: Option<SecretKey>) -> Self {
        IrohRuntime::configure_worker_threads(transport.tokio_worker_threads);
        let inner = Arc::new(ManagerInner {
            listener,
            endpoint: RwLock::new(None),
            secret_key: secret_key.unwrap_or_else(SecretKey::generate),
            transport_config: transport,
            enable_statistics,
            priority_channels: BasisNetworkCommons::build_priority_unreliable_channel_map(),
            peers: DashMap::new(),
            free_ids: Mutex::new(BTreeSet::new()),
            next_id: AtomicI32::new(0),
            next_identity: AtomicU64::new(1),
            running: AtomicBool::new(false),
            accept_task: Mutex::new(None),
            packets_sent: AtomicU64::new(0),
            packets_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            unreliable_dropped: AtomicI64::new(0),
            priority_dropped: AtomicI64::new(0),
            probe_replies: DashMap::new(),
            weak_self: RwLock::new(Weak::new()),
        });
        *inner.weak_self.write() = Arc::downgrade(&inner);
        Self { inner }
    }

    /// Loads (or creates and saves) the secret key file named by the transport config, so the
    /// server's endpoint id survives restarts.
    pub fn load_or_create_secret_key(config_dir: &std::path::Path, transport: &IrohTransportConfig) -> SecretKey {
        let path = config_dir.join(&transport.secret_key_file);
        if let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(key) = text.trim().parse::<SecretKey>()
        {
            return key;
        }
        let key = SecretKey::generate();
        if let Err(e) = std::fs::create_dir_all(config_dir).and_then(|_| std::fs::write(&path, format!("{}\n", hex_encode(&key.to_bytes())))) {
            BNL::log_warning(format!("Could not persist the iroh secret key to '{}': {e}", path.display()));
        }
        key
    }

    /// This endpoint's id — what clients put in their connection string.
    pub fn endpoint_id(&self) -> EndpointId {
        self.inner.secret_key.public()
    }

    /// The bound endpoint, once started.
    pub fn endpoint(&self) -> Option<Endpoint> {
        self.inner.endpoint.read().clone()
    }

    /// The address to dial this endpoint at: its id plus the sockets it bound.
    pub fn endpoint_addr(&self) -> Option<EndpointAddr> {
        self.endpoint().map(|ep| ep.addr())
    }

    /// Local UDP ports actually bound.
    pub fn bound_sockets(&self) -> Vec<SocketAddr> {
        self.endpoint().map(|ep| ep.bound_sockets()).unwrap_or_default()
    }

    /// `id@host:port` — the connection string a client on the same network can use directly.
    pub fn connection_string(&self) -> String {
        let id = self.endpoint_id().to_z32();
        match self.bound_sockets().into_iter().find(|s| s.is_ipv4()).or_else(|| self.bound_sockets().first().copied()) {
            Some(sock) => format!("{id}@127.0.0.1:{}", sock.port()),
            None => id,
        }
    }

    pub fn peer(&self, id: i32) -> Option<IrohNetPeer> {
        self.inner.peers.get(&id).map(|p| p.clone())
    }

    /// Probes a server for its info line (the counterpart of the unconnected UDP query).
    pub async fn probe(target: ConnectionTarget, timeout_ms: i32) -> ServerProbeResult {
        let mut result = ServerProbeResult::default();
        let addr = match ManagerInner::parse_target(&target) {
            Ok(a) => a,
            Err(e) => {
                result.error = e;
                return result;
            }
        };
        let started = Instant::now();
        let probe = async {
            let ep = Endpoint::builder(presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .bind()
                .await
                .map_err(|e| e.to_string())?;
            let conn = ep.connect(addr.clone(), BASIS_PROBE_ALPN).await.map_err(|e| e.to_string())?;
            let nonce: u16 = rand::random();
            let mut writer = NetDataWriter::new();
            writer.put_uint(BasisNetworkCommons::SERVER_INFO_QUERY_MAGIC);
            writer.put_ushort(BasisNetworkCommons::SERVER_INFO_PROTOCOL_VERSION);
            writer.put_ushort(nonce);
            while writer.length() < BasisNetworkCommons::SERVER_INFO_MIN_REQUEST_BYTES {
                writer.put_byte(0);
            }
            let mut tx = conn.open_uni().await.map_err(|e| e.to_string())?;
            tx.write_all(writer.as_read_only_span()).await.map_err(|e| e.to_string())?;
            tx.finish().map_err(|e| e.to_string())?;
            let mut rx = conn.accept_uni().await.map_err(|e| e.to_string())?;
            let bytes = rx.read_to_end(4096).await.map_err(|e| e.to_string())?;
            conn.close(VarInt::from_u32(CLOSE_NORMAL), b"done");
            ep.close().await;
            Ok::<Vec<u8>, String>(bytes)
        };
        match tokio::time::timeout(Duration::from_millis(timeout_ms.max(1) as u64), probe).await {
            Err(_) => {
                result.timed_out = true;
                result.error = "timed out".into();
            }
            Ok(Err(e)) => result.error = e,
            Ok(Ok(bytes)) => {
                let mut reader = NetDataReader::new(bytes);
                let parsed = (|| -> Result<(), String> {
                    if reader.get_uint().map_err(|e| e.to_string())? != BasisNetworkCommons::SERVER_INFO_RESPONSE_MAGIC {
                        return Err("bad response magic".into());
                    }
                    result.protocol_version = reader.get_ushort().map_err(|e| e.to_string())?;
                    let _nonce = reader.get_ushort().map_err(|e| e.to_string())?;
                    result.online = reader.get_ushort().map_err(|e| e.to_string())?;
                    result.max = reader.get_ushort().map_err(|e| e.to_string())?;
                    result.name = reader.get_string_max(BasisNetworkCommons::SERVER_INFO_NAME_MAX_LENGTH).map_err(|e| e.to_string())?;
                    result.motd = reader.get_string_max(BasisNetworkCommons::SERVER_INFO_MOTD_MAX_LENGTH).map_err(|e| e.to_string())?;
                    Ok(())
                })();
                match parsed {
                    Ok(()) => {
                        result.reachable = true;
                        result.round_trip_ms = started.elapsed().as_millis() as i32;
                        result.endpoint_id = addr.id.to_z32();
                        result.resolved_address = addr.ip_addrs().next().map(|s| s.ip());
                    }
                    Err(e) => result.error = e,
                }
            }
        }
        result
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl ManagerInner {
    fn parse_target(target: &ConnectionTarget) -> Result<EndpointAddr, String> {
        let mut t = target.clone();
        if t.get(ConnectionTargetKeys::ENDPOINT_ID).is_none() {
            use super::connection_target::IConnectionTargetParser;
            IrohConnectionTargetParser.parse(&mut t);
        }
        let id_text = t.get(ConnectionTargetKeys::ENDPOINT_ID).ok_or_else(|| "connection string has no endpoint id".to_string())?;
        let id = Self::parse_endpoint_id(&id_text)?;
        let mut addr = EndpointAddr::new(id);
        if let Some(host) = t.get(ConnectionTargetKeys::ADDRESS) {
            let port = t.get(ConnectionTargetKeys::PORT).and_then(|p| p.parse::<u16>().ok()).unwrap_or(LNLConnectionTargetParser::DEFAULT_PORT);
            if let Ok(ip) = host.parse::<IpAddr>() {
                addr = addr.with_ip_addr(SocketAddr::new(ip, port));
            } else if let Ok(resolved) = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), port)) {
                for s in resolved {
                    addr = addr.with_ip_addr(s);
                }
            }
        }
        if let Some(relay) = t.get(ConnectionTargetKeys::RELAY_URL)
            && let Ok(url) = relay.parse()
        {
            addr = addr.with_relay_url(url);
        }
        Ok(addr)
    }

    fn parse_endpoint_id(text: &str) -> Result<EndpointId, String> {
        let text = text.trim();
        if let Ok(id) = EndpointId::from_z32(text) {
            return Ok(id);
        }
        text.parse::<EndpointId>().map_err(|e| format!("'{text}' is not an endpoint id: {e}"))
    }

    fn allocate_id(&self) -> i32 {
        if let Some(id) = self.free_ids.lock().pop_first() {
            return id;
        }
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn release_id(&self, id: i32) {
        self.free_ids.lock().insert(id);
    }

    fn queue_limits(&self) -> (u32, u32) {
        let peers = self.peers.len() as i32;
        let bulk = BasisPopulationScale::unreliable_queue_per_peer(self.transport_config.max_datagram_queue_per_peer, peers);
        let priority = BasisPopulationScale::priority_queue_per_peer(self.transport_config.max_priority_datagram_queue_per_peer, peers);
        (bulk as u32, priority as u32)
    }

    fn admit(self: &Arc<Self>, conn: Connection, remote: IpAddr, server_side: bool, remote_id: i32) -> IrohNetPeer {
        let id = if server_side { self.allocate_id() } else { 0 };
        let (bulk, priority) = self.queue_limits();
        let state = Arc::new(PeerState {
            id,
            remote_id: AtomicI32::new(remote_id),
            identity: self.next_identity.fetch_add(1, Ordering::Relaxed),
            conn,
            remote_addr: remote,
            manager: Arc::downgrade(self),
            is_server_side: server_side,
            connected: AtomicBool::new(true),
            disconnect_raised: AtomicBool::new(false),
            tag: RwLock::new(None),
            rtt_ms: AtomicI32::new(0),
            remote_time_delta: AtomicI64::new(0),
            last_packet: Mutex::new(Instant::now()),
            reliable_queue: Mutex::new(VecDeque::new()),
            bulk_queue: Mutex::new(VecDeque::new()),
            priority_queue: Mutex::new(VecDeque::new()),
            queued_per_channel: std::array::from_fn(|_| AtomicU32::new(0)),
            bulk_limit: AtomicU32::new(bulk),
            priority_limit: AtomicU32::new(priority),
            sequenced_out: std::array::from_fn(|_| AtomicU32::new(0)),
            sequenced_in: Mutex::new([None; 64]),
            notify: Notify::new(),
            control_tx: tokio::sync::Mutex::new(None),
            ping_sent_at: Mutex::new(None),
        });
        let peer = IrohNetPeer::new(state);
        self.peers.insert(id, peer.clone());
        // Queue bounds follow the population, resolved on every join/leave.
        self.refresh_queue_limits();
        peer
    }

    fn refresh_queue_limits(&self) {
        let (bulk, priority) = self.queue_limits();
        for p in self.peers.iter() {
            p.state.bulk_limit.store(bulk, Ordering::Relaxed);
            p.state.priority_limit.store(priority, Ordering::Relaxed);
        }
    }

    fn record_sent(&self, bytes: usize) {
        if self.enable_statistics {
            self.packets_sent.fetch_add(1, Ordering::Relaxed);
            self.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
        }
    }

    fn record_received(&self, bytes: usize) {
        if self.enable_statistics {
            self.packets_received.fetch_add(1, Ordering::Relaxed);
            self.bytes_received.fetch_add(bytes as u64, Ordering::Relaxed);
        }
    }

    fn build_transport_config(&self) -> QuicTransportConfig {
        let idle = Duration::from_millis(self.transport_config.idle_timeout_ms.max(1000) as u64);
        let keep_alive = if self.transport_config.keep_alive_interval_ms > 0 {
            Duration::from_millis(self.transport_config.keep_alive_interval_ms as u64)
        } else {
            idle / 3
        };
        QuicTransportConfig::builder()
            .max_idle_timeout(IdleTimeout::try_from(idle).ok())
            .keep_alive_interval(keep_alive)
            .max_concurrent_uni_streams(VarInt::from_u32(4096))
            .datagram_receive_buffer_size(Some(4 * 1024 * 1024))
            .datagram_send_buffer_size(4 * 1024 * 1024)
            .build()
    }

    async fn bind(self: Arc<Self>, ipv4: IpAddr, ipv6: IpAddr, port: u16) -> Result<(), String> {
        let relay_mode = match self.transport_config.relay_mode.trim().to_ascii_lowercase().as_str() {
            "disabled" | "none" | "off" => RelayMode::Disabled,
            "custom" => {
                let urls: Vec<iroh::RelayUrl> = self
                    .transport_config
                    .relay_urls_list()
                    .iter()
                    .filter_map(|u| u.parse().ok())
                    .collect();
                if urls.is_empty() { RelayMode::Disabled } else { RelayMode::custom(urls) }
            }
            "staging" => RelayMode::Staging,
            _ => RelayMode::Default,
        };
        let mut builder = Endpoint::builder(presets::Minimal)
            .secret_key(self.secret_key.clone())
            .alpns(vec![BASIS_ALPN.to_vec(), BASIS_PROBE_ALPN.to_vec()])
            .relay_mode(relay_mode)
            .transport_config(self.build_transport_config());
        if let IpAddr::V4(v4) = ipv4 {
            builder = builder.bind_addr(SocketAddr::new(IpAddr::V4(v4), port)).map_err(|e| e.to_string())?;
        }
        if let IpAddr::V6(v6) = ipv6 {
            // Same port on both families when one was asked for; an OS-picked port (0) is
            // picked independently per socket.
            match builder.bind_addr(SocketAddr::new(IpAddr::V6(v6), port)) {
                Ok(b) => builder = b,
                Err(e) => BNL::log_warning(format!("IPv6 bind skipped: {e}")),
            }
        }
        let endpoint = builder.bind().await.map_err(|e| e.to_string())?;
        *self.endpoint.write() = Some(endpoint.clone());
        self.running.store(true, Ordering::SeqCst);
        let me = self.clone();
        let task = IrohRuntime::spawn(async move { me.accept_loop(endpoint).await });
        *self.accept_task.lock() = Some(task);
        Ok(())
    }

    async fn accept_loop(self: Arc<Self>, endpoint: Endpoint) {
        while let Some(incoming) = endpoint.accept().await {
            if !self.running.load(Ordering::Relaxed) {
                break;
            }
            let remote = match incoming.remote_addr() {
                iroh::endpoint::IncomingAddr::Ip(addr) => addr,
                _ => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            };
            let me = self.clone();
            IrohRuntime::spawn(async move {
                let accepting = match incoming.accept() {
                    Ok(a) => a,
                    Err(e) => {
                        BNL::log_warning(format!("[iroh] accept failed from {remote}: {e}"));
                        return;
                    }
                };
                let conn = match accepting.await {
                    Ok(c) => c,
                    Err(e) => {
                        BNL::log_warning(format!("[iroh] handshake failed from {remote}: {e}"));
                        return;
                    }
                };
                if conn.alpn() == BASIS_PROBE_ALPN {
                    me.serve_probe(conn, remote).await;
                } else {
                    me.serve_connect(conn, remote).await;
                }
            });
        }
    }

    async fn serve_probe(self: Arc<Self>, conn: Connection, remote: SocketAddr) {
        let Ok(mut rx) = conn.accept_uni().await else { return };
        let Ok(bytes) = rx.read_to_end(4096).await else { return };
        self.probe_replies.insert(remote, conn.clone());
        self.listener.raise_network_receive_unconnected(remote, NetPacketReader::new(bytes));
        // The handler replies synchronously; whatever is left is a dropped probe.
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Some((_, c)) = self.probe_replies.remove(&remote) {
            c.close(VarInt::from_u32(CLOSE_NORMAL), b"no reply");
        }
    }

    async fn serve_connect(self: Arc<Self>, conn: Connection, remote: SocketAddr) {
        let (tx, mut rx) = match tokio::time::timeout(Duration::from_secs(10), conn.accept_bi()).await {
            Ok(Ok(streams)) => streams,
            _ => {
                conn.close(VarInt::from_u32(CLOSE_PROTOCOL), b"no control stream");
                return;
            }
        };
        let payload = match tokio::time::timeout(Duration::from_secs(10), read_control_frame(&mut rx, CTL_CONNECT)).await {
            Ok(Ok(p)) => p,
            _ => {
                conn.close(VarInt::from_u32(CLOSE_PROTOCOL), b"bad connect");
                return;
            }
        };
        let request = Arc::new(IrohConnectionRequest {
            manager: self.clone(),
            conn: conn.clone(),
            control_tx: tokio::sync::Mutex::new(Some(tx)),
            control_rx: tokio::sync::Mutex::new(Some(rx)),
            data: payload,
            remote,
            decided: AtomicBool::new(false),
        });
        // Raised on a blocking thread: the server's handler is synchronous and may take the
        // tokio thread for longer than a scheduler slot should allow.
        let listener = self.listener.clone();
        let req = request.clone();
        let _ = tokio::task::spawn_blocking(move || listener.raise_connection_request(req)).await;
        if !request.decided.load(Ordering::SeqCst) {
            // No handler decided: LiteNetLib would time the request out.
            request.reject(&NetDataWriter::new());
        }
    }

    /// Runs a peer's I/O until the connection closes: the sender task, the datagram reader, the
    /// uni-stream acceptor and the control stream.
    async fn run_peer(self: Arc<Self>, state: Arc<PeerState>, control_rx: Option<RecvStream>) {
        let peer = IrohNetPeer::new(state.clone());
        if state.is_server_side {
            // The C# raised PeerConnected when the transport accepted; the server itself treats
            // OnNetworkAccepted as the real join, so this stays informational.
            self.listener.raise_peer_connected(Arc::new(peer.clone()));
        }
        let sender = IrohRuntime::spawn(Self::sender_task(self.clone(), state.clone()));
        let datagrams = IrohRuntime::spawn(Self::datagram_task(self.clone(), state.clone()));
        let streams = IrohRuntime::spawn(Self::stream_accept_task(self.clone(), state.clone()));
        let pinger = IrohRuntime::spawn(Self::ping_task(state.clone()));
        let control = IrohRuntime::spawn(Self::control_task(self.clone(), state.clone(), control_rx));

        let error = state.conn.closed().await;
        state.connected.store(false, Ordering::SeqCst);
        state.notify.notify_one();
        sender.abort();
        datagrams.abort();
        streams.abort();
        pinger.abort();
        let disconnect_data = control.await.ok().flatten();

        let reason = match &error {
            iroh::endpoint::ConnectionError::ApplicationClosed(closed) => {
                match u64::from(closed.error_code) as u32 {
                    CLOSE_REJECTED => DisconnectReason::ConnectionRejected,
                    CLOSE_DISCONNECT | CLOSE_NORMAL | CLOSE_FORCE => DisconnectReason::RemoteConnectionClose,
                    _ => DisconnectReason::RemoteConnectionClose,
                }
            }
            iroh::endpoint::ConnectionError::LocallyClosed => DisconnectReason::DisconnectPeerCalled,
            iroh::endpoint::ConnectionError::TimedOut => DisconnectReason::Timeout,
            iroh::endpoint::ConnectionError::Reset => DisconnectReason::RemoteConnectionClose,
            _ => DisconnectReason::ConnectionFailed,
        };
        self.finish_peer(state, reason, disconnect_data);
    }

    fn finish_peer(&self, state: Arc<PeerState>, reason: DisconnectReason, additional: Option<Vec<u8>>) {
        if state.disconnect_raised.swap(true, Ordering::SeqCst) {
            return;
        }
        state.connected.store(false, Ordering::SeqCst);
        let removed = self.peers.remove(&state.id).is_some();
        if state.is_server_side && removed {
            self.release_id(state.id);
        }
        self.refresh_queue_limits();
        let info = DisconnectInfo {
            reason,
            socket_error_code: 0,
            additional_data: NetPacketReader::new(additional.unwrap_or_default()),
        };
        self.listener.raise_peer_disconnected(Arc::new(IrohNetPeer::new(state)), info);
    }

    async fn sender_task(self: Arc<Self>, state: Arc<PeerState>) {
        let mut ordered_streams: HashMap<u8, SendStream> = HashMap::new();
        loop {
            // Voice first, then bulk, then reliable — the priority the C# transport gave voice.
            let next_datagram = state.priority_queue.lock().pop_front().or_else(|| state.bulk_queue.lock().pop_front());
            if let Some(frame) = next_datagram {
                let channel = frame[0] & DATAGRAM_CHANNEL_MASK;
                state.queued_per_channel[usize::from(channel)].fetch_sub(1, Ordering::Relaxed);
                let len = frame.len();
                match state.conn.send_datagram_wait(frame).await {
                    Ok(()) => self.record_sent(len),
                    Err(_) => {
                        if !state.connected.load(Ordering::Relaxed) {
                            return;
                        }
                    }
                }
                continue;
            }
            let next_reliable = state.reliable_queue.lock().pop_front();
            match next_reliable {
                Some(Outgoing::Reliable { channel, ordered, data }) => {
                    let len = data.len();
                    let result = if ordered {
                        Self::send_on_ordered_stream(&state, &mut ordered_streams, channel, data).await
                    } else {
                        Self::send_on_fresh_stream(&state, channel, data).await
                    };
                    if result.is_ok() {
                        self.record_sent(len);
                    } else if !state.connected.load(Ordering::Relaxed) {
                        return;
                    }
                    continue;
                }
                Some(Outgoing::Disconnect { data, code }) => {
                    if let Some(tx) = state.control_tx.lock().await.as_mut() {
                        let mut msg = vec![CTL_DISCONNECT];
                        msg.extend_from_slice(&(data.len() as u32).to_le_bytes());
                        msg.extend_from_slice(&data);
                        let _ = tx.write_all(&msg).await;
                    }
                    for (_, s) in ordered_streams.drain() {
                        let _ = s.finish();
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    state.conn.close(VarInt::from_u32(code), b"disconnect");
                    return;
                }
                None => {}
            }
            if !state.connected.load(Ordering::Relaxed) {
                return;
            }
            state.notify.notified().await;
        }
    }

    async fn send_on_ordered_stream(state: &PeerState, streams: &mut HashMap<u8, SendStream>, channel: u8, data: Bytes) -> Result<(), ()> {
        if !streams.contains_key(&channel) {
            let mut s = state.conn.open_uni().await.map_err(|_| ())?;
            s.write_all(&[STREAM_RELIABLE_ORDERED, channel]).await.map_err(|_| ())?;
            streams.insert(channel, s);
        }
        let s = streams.get_mut(&channel).unwrap();
        let mut frame = BytesMut::with_capacity(4 + data.len());
        frame.extend_from_slice(&(data.len() as u32).to_le_bytes());
        frame.extend_from_slice(&data);
        if s.write_all(&frame).await.is_err() {
            streams.remove(&channel);
            return Err(());
        }
        Ok(())
    }

    async fn send_on_fresh_stream(state: &PeerState, channel: u8, data: Bytes) -> Result<(), ()> {
        let mut s = state.conn.open_uni().await.map_err(|_| ())?;
        let mut frame = BytesMut::with_capacity(6 + data.len());
        frame.extend_from_slice(&[STREAM_RELIABLE_UNORDERED, channel]);
        frame.extend_from_slice(&(data.len() as u32).to_le_bytes());
        frame.extend_from_slice(&data);
        s.write_all(&frame).await.map_err(|_| ())?;
        let _ = s.finish();
        Ok(())
    }

    async fn datagram_task(self: Arc<Self>, state: Arc<PeerState>) {
        let peer: NetPeerRef = Arc::new(IrohNetPeer::new(state.clone()));
        while let Ok(datagram) = state.conn.read_datagram().await {
            if datagram.is_empty() {
                continue;
            }
            *state.last_packet.lock() = Instant::now();
            self.record_received(datagram.len());
            let header = datagram[0];
            let channel = header & DATAGRAM_CHANNEL_MASK;
            if (header & DATAGRAM_SEQUENCED_FLAG) != 0 {
                if datagram.len() < 3 {
                    continue;
                }
                let seq = u16::from_le_bytes([datagram[1], datagram[2]]);
                {
                    let mut last = state.sequenced_in.lock();
                    if let Some(prev) = last[usize::from(channel)]
                        && (seq.wrapping_sub(prev) as i16) <= 0
                    {
                        continue; // older than what we already delivered
                    }
                    last[usize::from(channel)] = Some(seq);
                }
                let reader = NetPacketReader::new(datagram.slice(3..));
                self.listener.raise_network_receive(peer.clone(), reader, channel, DeliveryMethod::Sequenced);
            } else {
                let reader = NetPacketReader::new(datagram.slice(1..));
                self.listener.raise_network_receive(peer.clone(), reader, channel, DeliveryMethod::Unreliable);
            }
        }
    }

    async fn stream_accept_task(self: Arc<Self>, state: Arc<PeerState>) {
        while let Ok(rx) = state.conn.accept_uni().await {
            let me = self.clone();
            let st = state.clone();
            IrohRuntime::spawn(async move { me.stream_reader(st, rx).await });
        }
    }

    async fn stream_reader(self: Arc<Self>, state: Arc<PeerState>, mut rx: RecvStream) {
        let mut header = [0u8; 2];
        if rx.read_exact(&mut header).await.is_err() {
            return;
        }
        let (kind, channel) = (header[0], header[1]);
        let method = match kind {
            STREAM_RELIABLE_ORDERED => DeliveryMethod::ReliableOrdered,
            STREAM_RELIABLE_UNORDERED => DeliveryMethod::ReliableUnordered,
            _ => {
                state.conn.close(VarInt::from_u32(CLOSE_PROTOCOL), b"bad stream kind");
                return;
            }
        };
        let peer: NetPeerRef = Arc::new(IrohNetPeer::new(state.clone()));
        loop {
            let mut len_buf = [0u8; 4];
            if rx.read_exact(&mut len_buf).await.is_err() {
                return; // finished or reset
            }
            let len = u32::from_le_bytes(len_buf) as usize;
            if len > MAX_FRAME_BYTES {
                state.conn.close(VarInt::from_u32(CLOSE_PROTOCOL), b"frame too large");
                return;
            }
            let mut payload = vec![0u8; len];
            if rx.read_exact(&mut payload).await.is_err() {
                return;
            }
            *state.last_packet.lock() = Instant::now();
            self.record_received(len + 4);
            self.listener.raise_network_receive(peer.clone(), NetPacketReader::new(payload), channel, method);
        }
    }

    async fn ping_task(state: Arc<PeerState>) {
        let interval = Duration::from_millis(BasisNetworkCommons::PING_INTERVAL as u64);
        loop {
            tokio::time::sleep(interval).await;
            if !state.connected.load(Ordering::Relaxed) {
                return;
            }
            let now_ticks = utc_now_ticks();
            *state.ping_sent_at.lock() = Some((now_ticks, Instant::now()));
            let mut msg = vec![CTL_PING];
            msg.extend_from_slice(&now_ticks.to_le_bytes());
            let mut guard = state.control_tx.lock().await;
            if let Some(tx) = guard.as_mut()
                && tx.write_all(&msg).await.is_err()
            {
                return;
            }
        }
    }

    /// Reads the control stream until it ends. Returns the DISCONNECT payload if one arrived.
    async fn control_task(self: Arc<Self>, state: Arc<PeerState>, rx: Option<RecvStream>) -> Option<Vec<u8>> {
        let mut rx = rx?;
        loop {
            let mut op = [0u8; 1];
            if rx.read_exact(&mut op).await.is_err() {
                return None;
            }
            match op[0] {
                CTL_PING => {
                    let mut ticks = [0u8; 8];
                    if rx.read_exact(&mut ticks).await.is_err() {
                        return None;
                    }
                    let mut msg = vec![CTL_PONG];
                    msg.extend_from_slice(&ticks);
                    msg.extend_from_slice(&utc_now_ticks().to_le_bytes());
                    if let Some(tx) = state.control_tx.lock().await.as_mut() {
                        let _ = tx.write_all(&msg).await;
                    }
                }
                CTL_PONG => {
                    let mut buf = [0u8; 16];
                    if rx.read_exact(&mut buf).await.is_err() {
                        return None;
                    }
                    let sent_ticks = i64::from_le_bytes(buf[..8].try_into().unwrap());
                    let their_ticks = i64::from_le_bytes(buf[8..].try_into().unwrap());
                    let matches = state.ping_sent_at.lock().map(|(t, _)| t == sent_ticks).unwrap_or(false);
                    if matches {
                        let now_ticks = utc_now_ticks();
                        let rtt_ticks = now_ticks - sent_ticks;
                        state.rtt_ms.store((rtt_ticks / 10_000) as i32, Ordering::Relaxed);
                        // Remote clock at the moment it answered, minus our estimate of our own
                        // clock at that moment.
                        state.remote_time_delta.store(their_ticks - (sent_ticks + rtt_ticks / 2), Ordering::Relaxed);
                    }
                    *state.last_packet.lock() = Instant::now();
                }
                CTL_DISCONNECT => {
                    let mut len_buf = [0u8; 4];
                    if rx.read_exact(&mut len_buf).await.is_err() {
                        return None;
                    }
                    let len = u32::from_le_bytes(len_buf) as usize;
                    if len > MAX_FRAME_BYTES {
                        return None;
                    }
                    let mut data = vec![0u8; len];
                    if rx.read_exact(&mut data).await.is_err() {
                        return Some(Vec::new());
                    }
                    return Some(data);
                }
                CTL_REJECTED => {
                    let mut len_buf = [0u8; 4];
                    if rx.read_exact(&mut len_buf).await.is_err() {
                        return None;
                    }
                    let len = u32::from_le_bytes(len_buf) as usize;
                    let mut data = vec![0u8; len.min(MAX_FRAME_BYTES)];
                    let _ = rx.read_exact(&mut data).await;
                    return Some(data);
                }
                _ => return None,
            }
        }
    }

    /// Client side: dial, send the connect payload, wait for the verdict.
    async fn connect_async(self: Arc<Self>, addr: EndpointAddr, payload: Vec<u8>, shared: Arc<PendingShared>) {
        let Some(endpoint) = self.endpoint.read().clone() else {
            BNL::log_error("[iroh] connect before start");
            return;
        };
        let remote_ip = addr.ip_addrs().next().map(|s| s.ip()).unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let failed = |me: &Arc<Self>, reason: DisconnectReason, data: Option<Vec<u8>>| {
            // No live peer yet: the pending peer the caller holds is the one that "disconnected",
            // so the listener hears the outcome exactly as LiteNetLib reported a failed connect.
            shared.cancelled.store(true, Ordering::SeqCst);
            let peer: NetPeerRef = Arc::new(PendingPeer { remote_ip, shared: shared.clone() });
            me.listener.raise_peer_disconnected(
                peer,
                DisconnectInfo { reason, socket_error_code: 0, additional_data: NetPacketReader::new(data.unwrap_or_default()) },
            );
        };
        let conn = match endpoint.connect(addr, BASIS_ALPN).await {
            Ok(c) => c,
            Err(e) => {
                BNL::log_warning(format!("[iroh] connect failed: {e}"));
                failed(&self, DisconnectReason::ConnectionFailed, None);
                return;
            }
        };
        if shared.cancelled.load(Ordering::SeqCst) {
            conn.close(VarInt::from_u32(CLOSE_NORMAL), b"cancelled");
            failed(&self, DisconnectReason::DisconnectPeerCalled, None);
            return;
        }
        let (bulk, priority) = self.queue_limits();
        let live = Arc::new(PeerState {
            id: 0,
            remote_id: AtomicI32::new(0),
            identity: shared.identity,
            conn: conn.clone(),
            remote_addr: remote_ip,
            manager: Arc::downgrade(&self),
            is_server_side: false,
            connected: AtomicBool::new(true),
            disconnect_raised: AtomicBool::new(false),
            tag: RwLock::new(shared.tag.read().clone()),
            rtt_ms: AtomicI32::new(0),
            remote_time_delta: AtomicI64::new(0),
            last_packet: Mutex::new(Instant::now()),
            reliable_queue: Mutex::new(std::mem::take(&mut *shared.early.lock())),
            bulk_queue: Mutex::new(VecDeque::new()),
            priority_queue: Mutex::new(VecDeque::new()),
            queued_per_channel: std::array::from_fn(|_| AtomicU32::new(0)),
            bulk_limit: AtomicU32::new(bulk),
            priority_limit: AtomicU32::new(priority),
            sequenced_out: std::array::from_fn(|_| AtomicU32::new(0)),
            sequenced_in: Mutex::new([None; 64]),
            notify: Notify::new(),
            control_tx: tokio::sync::Mutex::new(None),
            ping_sent_at: Mutex::new(None),
        });
        *shared.slot.lock() = Some(live.clone());
        self.peers.insert(live.id, IrohNetPeer::new(live.clone()));

        let (mut tx, mut rx) = match conn.open_bi().await {
            Ok(s) => s,
            Err(_) => {
                self.finish_peer(live, DisconnectReason::ConnectionFailed, None);
                return;
            }
        };
        let mut msg = vec![CTL_CONNECT];
        msg.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        msg.extend_from_slice(&payload);
        if tx.write_all(&msg).await.is_err() {
            self.finish_peer(live, DisconnectReason::ConnectionFailed, None);
            return;
        }
        let mut op = [0u8; 1];
        match tokio::time::timeout(Duration::from_secs(15), rx.read_exact(&mut op)).await {
            Ok(Ok(())) => {}
            _ => {
                conn.close(VarInt::from_u32(CLOSE_PROTOCOL), b"no verdict");
                self.finish_peer(live, DisconnectReason::Timeout, None);
                return;
            }
        }
        match op[0] {
            CTL_ACCEPTED => {
                let mut id = [0u8; 2];
                if rx.read_exact(&mut id).await.is_err() {
                    self.finish_peer(live, DisconnectReason::ConnectionFailed, None);
                    return;
                }
                live.remote_id.store(i32::from(u16::from_le_bytes(id)), Ordering::Relaxed);
                *live.control_tx.lock().await = Some(tx);
                self.listener.raise_peer_connected(Arc::new(IrohNetPeer::new(live.clone())));
                self.run_peer(live, Some(rx)).await;
            }
            CTL_REJECTED => {
                let mut len_buf = [0u8; 4];
                let mut data = Vec::new();
                if rx.read_exact(&mut len_buf).await.is_ok() {
                    let len = (u32::from_le_bytes(len_buf) as usize).min(MAX_FRAME_BYTES);
                    data = vec![0u8; len];
                    let _ = rx.read_exact(&mut data).await;
                }
                conn.close(VarInt::from_u32(CLOSE_NORMAL), b"rejected");
                self.finish_peer(live, DisconnectReason::ConnectionRejected, Some(data));
            }
            _ => {
                conn.close(VarInt::from_u32(CLOSE_PROTOCOL), b"bad verdict");
                self.finish_peer(live, DisconnectReason::InvalidProtocol, None);
            }
        }
    }
}

async fn read_control_frame(rx: &mut RecvStream, expected_op: u8) -> Result<Vec<u8>, ()> {
    let mut op = [0u8; 1];
    rx.read_exact(&mut op).await.map_err(|_| ())?;
    if op[0] != expected_op {
        return Err(());
    }
    let mut len_buf = [0u8; 4];
    rx.read_exact(&mut len_buf).await.map_err(|_| ())?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(());
    }
    let mut payload = vec![0u8; len];
    rx.read_exact(&mut payload).await.map_err(|_| ())?;
    Ok(payload)
}

impl NetManager for IrohNetManager {
    fn start(&self, ipv4_address: IpAddr, ipv6_address: IpAddr, set_port: u16) {
        let inner = self.inner.clone();
        if let Err(e) = IrohRuntime::block_on(inner.bind(ipv4_address, ipv6_address, set_port)) {
            BNL::log_error(format!("[iroh] bind failed: {e}"));
        }
    }

    fn stop(&self) {
        self.inner.running.store(false, Ordering::SeqCst);
        let peers: Vec<IrohNetPeer> = self.inner.peers.iter().map(|p| p.clone()).collect();
        for p in &peers {
            p.disconnect_force();
        }
        let endpoint = self.inner.endpoint.write().take();
        if let Some(task) = self.inner.accept_task.lock().take() {
            task.abort();
        }
        if let Some(ep) = endpoint {
            IrohRuntime::block_on(async move { ep.close().await });
        }
        for p in peers {
            self.inner.finish_peer(p.state.clone(), DisconnectReason::DisconnectPeerCalled, None);
        }
        self.inner.peers.clear();
        self.inner.free_ids.lock().clear();
        self.inner.next_id.store(0, Ordering::Relaxed);
    }

    fn connect(&self, target: &str, port: u16, writer: &NetDataWriter) -> Option<NetPeerRef> {
        let raw = if target.contains('@') || port == 0 { target.to_string() } else { format!("{target}:{port}") };
        let mut ct = ConnectionTarget::new(BasisNetworkStackRegistry::IROH_ID, &raw);
        {
            use super::connection_target::IConnectionTargetParser;
            IrohConnectionTargetParser.parse(&mut ct);
        }
        if ct.get(ConnectionTargetKeys::ENDPOINT_ID).is_none() && !target.contains('@') {
            // "host:port" plus a separate endpoint id is what the C# signature could not carry;
            // callers pass "id@host" as the address instead.
            BNL::log_error("[iroh] connect needs an endpoint id: use 'endpointid@host:port'");
            return None;
        }
        let addr = match ManagerInner::parse_target(&ct) {
            Ok(a) => a,
            Err(e) => {
                BNL::log_error(format!("[iroh] {e}"));
                return None;
            }
        };
        let inner = self.inner.clone();
        if self.inner.endpoint.read().is_none() {
            BNL::log_error("[iroh] connect before start");
            return None;
        }
        let remote_ip = addr.ip_addrs().next().map(|s| s.ip()).unwrap_or(IpAddr::V6(Ipv6Addr::UNSPECIFIED));
        let shared = Arc::new(PendingShared {
            slot: Mutex::new(None),
            early: Mutex::new(VecDeque::new()),
            identity: self.inner.next_identity.fetch_add(1, Ordering::Relaxed),
            tag: RwLock::new(None),
            cancelled: AtomicBool::new(false),
        });
        let peer = PendingPeer { remote_ip, shared: shared.clone() };
        let payload = writer.copy_data();
        IrohRuntime::spawn(async move { inner.connect_async(addr, payload, shared).await });
        Some(Arc::new(peer))
    }

    fn send_unconnected_message(&self, writer: &NetDataWriter, remote_end_point: SocketAddr) -> bool {
        let Some((_, conn)) = self.inner.probe_replies.remove(&remote_end_point) else {
            return false;
        };
        let data = writer.copy_data();
        IrohRuntime::spawn(async move {
            if let Ok(mut tx) = conn.open_uni().await {
                let _ = tx.write_all(&data).await;
                let _ = tx.finish();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            conn.close(VarInt::from_u32(CLOSE_NORMAL), b"done");
        });
        true
    }

    fn statistics(&self) -> NetStatistics {
        NetStatistics {
            packets_sent: self.inner.packets_sent.load(Ordering::Relaxed),
            packets_received: self.inner.packets_received.load(Ordering::Relaxed),
            bytes_sent: self.inner.bytes_sent.load(Ordering::Relaxed),
            bytes_received: self.inner.bytes_received.load(Ordering::Relaxed),
            packet_loss: 0,
        }
    }

    fn connected_peers_count(&self) -> i32 {
        self.inner.peers.len() as i32
    }

    fn unreliable_dropped(&self) -> i64 {
        self.inner.unreliable_dropped.load(Ordering::Relaxed)
    }

    fn priority_unreliable_dropped(&self) -> i64 {
        self.inner.priority_dropped.load(Ordering::Relaxed)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// What `connect` shares with the dial task: the live peer once the handshake finishes, and
/// anything the caller sent before it did.
struct PendingShared {
    slot: Mutex<Option<Arc<PeerState>>>,
    early: Mutex<VecDeque<Outgoing>>,
    identity: u64,
    tag: RwLock<Option<Arc<dyn Any + Send + Sync>>>,
    cancelled: AtomicBool,
}

/// The peer handed back by `connect` before the dial completes. It forwards to the live peer
/// once there is one and queues reliable sends until then, so a caller can hold and use the
/// peer `Connect` returned exactly as it did with LiteNetLib.
pub struct PendingPeer {
    remote_ip: IpAddr,
    shared: Arc<PendingShared>,
}

impl PendingPeer {
    fn live(&self) -> Option<IrohNetPeer> {
        self.shared.slot.lock().clone().map(IrohNetPeer::new)
    }

    /// The live iroh peer, once connected.
    pub fn iroh_peer(&self) -> Option<IrohNetPeer> {
        self.live()
    }
}

impl NetPeer for PendingPeer {
    fn disconnect(&self) {
        self.disconnect_with(&[]);
    }

    fn disconnect_with(&self, data: &[u8]) {
        match self.live() {
            Some(p) => p.disconnect_with(data),
            None => self.shared.cancelled.store(true, Ordering::SeqCst),
        }
    }

    fn disconnect_force(&self) {
        match self.live() {
            Some(p) => p.disconnect_force(),
            None => self.shared.cancelled.store(true, Ordering::SeqCst),
        }
    }

    fn send(&self, data: &[u8], channel_number: u8, delivery_method: DeliveryMethod) {
        match self.live() {
            Some(p) => p.send(data, channel_number, delivery_method),
            None => {
                // Only reliable sends survive the wait; an unreliable one would be stale by then.
                if delivery_method.is_reliable() {
                    self.shared.early.lock().push_back(Outgoing::Reliable {
                        channel: channel_number,
                        ordered: delivery_method != DeliveryMethod::ReliableUnordered,
                        data: Bytes::copy_from_slice(data),
                    });
                }
            }
        }
    }

    fn send_unreliable_raw_merge(&self, data: &[u8], offset: usize, length: usize, channel_number: u8, patch_offset: i32, patch_value: u8) {
        if let Some(p) = self.live() {
            p.send_unreliable_raw_merge(data, offset, length, channel_number, patch_offset, patch_value);
        }
    }

    fn get_packets_count_in_queue(&self, channel: u8, delivery_method: DeliveryMethod) -> i32 {
        self.live().map(|p| p.get_packets_count_in_queue(channel, delivery_method)).unwrap_or(0)
    }

    fn id(&self) -> i32 {
        0
    }

    fn address(&self) -> IpAddr {
        self.live().map(|p| p.address()).unwrap_or(self.remote_ip)
    }

    fn remote_id(&self) -> i32 {
        self.live().map(|p| p.remote_id()).unwrap_or(0)
    }

    fn round_trip_time(&self) -> i32 {
        self.live().map(|p| p.round_trip_time()).unwrap_or(0)
    }

    fn time_since_last_packet(&self) -> f32 {
        self.live().map(|p| p.time_since_last_packet()).unwrap_or(0.0)
    }

    fn remote_time_delta(&self) -> i64 {
        self.live().map(|p| p.remote_time_delta()).unwrap_or(0)
    }

    fn mtu(&self) -> i32 {
        self.live().map(|p| p.mtu()).unwrap_or(BasisNetworkCommons::MAX_UNFRAGMENTED_PAYLOAD)
    }

    fn tag(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        match self.live() {
            Some(p) => p.tag(),
            None => self.shared.tag.read().clone(),
        }
    }

    fn set_tag(&self, tag: Option<Arc<dyn Any + Send + Sync>>) {
        *self.shared.tag.write() = tag.clone();
        if let Some(p) = self.live() {
            p.set_tag(tag);
        }
    }

    fn identity(&self) -> u64 {
        self.shared.identity
    }

    fn is_connected(&self) -> bool {
        match self.live() {
            Some(p) => p.is_connected(),
            None => !self.shared.cancelled.load(Ordering::Relaxed),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

