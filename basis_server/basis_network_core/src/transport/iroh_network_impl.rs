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
use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::{Bytes, BytesMut};
use dashmap::DashMap;
use iroh::endpoint::{presets, AckFrequencyConfig, Connection, IdleTimeout, QuicTransportConfig, RecvStream, SendStream, VarInt};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, SecretKey};
use parking_lot::{Mutex, RwLock};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use basis_error::{BasisError, BasisResult, ErrorCode, FaultKind};

use crate::BNL;
use crate::configuration::{BasisPopulationScale, BasisTransportConfigStore, Configuration, IrohTransportConfig};
use crate::io::{NetDataReader, NetDataWriter, NetPacketReader};
use crate::pooling::packet_buffer_pool::PacketBufferPool;
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

/// Largest single frame accepted on a reliable stream or the control stream; anything over is
/// a protocol violation and closes the connection. The largest Basis message (a ready batch)
/// is 32 KiB, so 1 MiB leaves room without letting one peer make the server allocate more
/// than QUIC's own receive window already bounds.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// QUIC close codes.
const CLOSE_NORMAL: u32 = 0;
const CLOSE_REJECTED: u32 = 1;
const CLOSE_DISCONNECT: u32 = 2;
const CLOSE_FORCE: u32 = 3;
const CLOSE_PROTOCOL: u32 = 4;

/// The tokio runtime every iroh transport in the process runs on.
pub struct IrohRuntime;

static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
static RUNTIME_THREADS: AtomicI32 = AtomicI32::new(0);

impl IrohRuntime {
    /// Sets the worker thread count before the runtime is first used. 0 = automatic.
    pub fn configure_worker_threads(threads: i32) {
        RUNTIME_THREADS.store(threads, Ordering::Relaxed);
    }

    /// The runtime handle. Fails only when the runtime could not be built (the OS refused the
    /// worker threads), which every later call reports the same way.
    pub fn handle() -> BasisResult<tokio::runtime::Handle> {
        let runtime = RUNTIME.get_or_init(|| {
            let configured = RUNTIME_THREADS.load(Ordering::Relaxed);
            let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
            let threads = usize::try_from(configured).ok().filter(|t| *t > 0).unwrap_or(cores.max(1));
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(threads)
                .thread_name("basis-iroh")
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
        });
        match runtime {
            Ok(rt) => Ok(rt.handle().clone()),
            Err(e) => Err(BasisError::permanent(ErrorCode::Internal, format!("the transport runtime could not be built: {e}"))),
        }
    }

    /// Runs `fut` on the transport runtime and waits for it from any thread — including one
    /// that is itself inside another runtime, which `Runtime::block_on` would refuse. Do not
    /// call it from a transport worker thread: that would block the very thread the task
    /// needs.
    pub fn block_on<T: Send + 'static>(fut: impl std::future::Future<Output = T> + Send + 'static) -> BasisResult<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        Self::handle()?.spawn(async move {
            let _ = tx.send(fut.await);
        });
        rx.recv()
            .map_err(|_| BasisError::permanent(ErrorCode::Cancelled, "the transport runtime dropped the task before it completed"))
    }

    pub fn spawn<F>(fut: F) -> BasisResult<JoinHandle<F::Output>>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        Ok(Self::handle()?.spawn(fut))
    }

    /// Spawns a task nobody waits on. A runtime failure is logged; there is nothing else a
    /// fire-and-forget caller could do with it.
    pub fn spawn_detached<F>(fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        if let Err(e) = Self::spawn(fut) {
            BNL::log_error(format!("[iroh] could not spawn a transport task: {e}"));
        }
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
    // Bytes of reliable data currently queued (payloads only) and the per-peer budget. A send
    // that would take the queue past the budget is refused; a peer that stays over it past the
    // grace period is disconnected — the difference between a slow client and a memory leak.
    reliable_queued_bytes: AtomicUsize,
    reliable_budget: AtomicUsize,
    /// The last time a reliable frame left the queue for the wire. While the queue is non-empty
    /// and this is not advancing, the peer is not reading (its flow-control window is full and
    /// no acks are arriving), so the watchdog disconnects it once the gap passes the grace period.
    last_reliable_drain: Mutex<Instant>,
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
    /// Set once a datagram exceeded the live MTU, so that warning is logged once per peer.
    warned_too_large: AtomicBool,
    /// Set once this peer's queued frames outgrew the connection's datagram buffer, so that
    /// warning is logged once per peer rather than once per congested pass.
    warned_backlog: AtomicBool,
    /// A locally-decided disconnect reason (the send-queue watchdog), reported in place of the
    /// generic "locally closed" that `conn.closed()` would otherwise yield.
    local_disconnect_reason: Mutex<Option<DisconnectReason>>,
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

    fn enqueue_reliable(&self, channel: u8, ordered: bool, data: &[u8]) -> Result<(), SendError> {
        let budget = self.state.reliable_budget.load(Ordering::Relaxed);
        // The check and the update are one critical section so two concurrent sends cannot both
        // pass a budget that only one of them fits.
        let mut queue = self.state.reliable_queue.lock();
        let queued = self.state.reliable_queued_bytes.load(Ordering::Acquire);
        if budget > 0 && queued.saturating_add(data.len()) > budget {
            // Over budget: refuse this message. The peer is holding more than it is allowed; the
            // watchdog disconnects it if the queue does not drain.
            return Err(SendError::QueueFull { queued, budget });
        }
        queue.push_back(Outgoing::Reliable { channel, ordered, data: Bytes::from(PacketBufferPool::rent_copy(data)) });
        self.state.reliable_queued_bytes.fetch_add(data.len(), Ordering::Release);
        drop(queue);
        self.state.notify.notify_one();
        Ok(())
    }

    fn enqueue_unreliable(&self, channel: u8, sequenced: bool, data: &[u8]) {
        let Some(manager) = self.manager() else { return };
        // One pooled buffer per datagram: the header prefix is stamped in place and the whole
        // frame rides as refcounted `Bytes`, returning to the pool once quinn has sent it.
        let prefix = if sequenced { 3 } else { 1 };
        let mut frame = PacketBufferPool::rent_frame(prefix, data);
        if sequenced {
            let seq = self
                .state
                .sequenced_out
                .get(usize::from(channel))
                .map(|c| c.fetch_add(1, Ordering::Relaxed) as u16)
                .unwrap_or(0);
            if let Some(header) = frame.get_mut(0..3) {
                header[0] = channel | DATAGRAM_SEQUENCED_FLAG;
                header[1..3].copy_from_slice(&seq.to_le_bytes());
            }
        } else if let Some(first) = frame.first_mut() {
            *first = channel;
        }
        let frame = Bytes::from(frame);

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
                    let old_channel = old.first().copied().unwrap_or(0) & DATAGRAM_CHANNEL_MASK;
                    if let Some(counter) = self.state.queued_per_channel.get(usize::from(old_channel)) {
                        counter.fetch_sub(1, Ordering::Relaxed);
                    }
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
            q.push_back(frame);
        }
        if let Some(counter) = self.state.queued_per_channel.get(usize::from(channel)) {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        self.state.notify.notify_one();
    }

    /// Largest unreliable payload this peer can carry right now, header excluded.
    fn datagram_limit(&self, sequenced: bool) -> usize {
        let mtu = usize::try_from(self.mtu()).unwrap_or(0);
        if sequenced { mtu.saturating_sub(2) } else { mtu }
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

    fn send(&self, data: &[u8], channel_number: u8, delivery_method: DeliveryMethod) -> Result<(), SendError> {
        if channel_number >= BasisNetworkCommons::TOTAL_CHANNELS {
            return Err(SendError::BadChannel { channel: channel_number, max: BasisNetworkCommons::TOTAL_CHANNELS });
        }
        if !self.state.connected.load(Ordering::Relaxed) {
            return Ok(()); // LiteNetLib dropped sends to a departed peer silently; so do we.
        }
        match delivery_method {
            DeliveryMethod::ReliableOrdered | DeliveryMethod::ReliableSequenced => return self.enqueue_reliable(channel_number, true, data),
            DeliveryMethod::ReliableUnordered => return self.enqueue_reliable(channel_number, false, data),
            DeliveryMethod::Unreliable => {
                let limit = self.datagram_limit(false);
                if data.len() > limit {
                    return Err(SendError::TooBig { size: data.len(), limit, method: delivery_method });
                }
                self.enqueue_unreliable(channel_number, false, data);
            }
            DeliveryMethod::Sequenced => {
                let limit = self.datagram_limit(true);
                if data.len() > limit {
                    return Err(SendError::TooBig { size: data.len(), limit, method: delivery_method });
                }
                self.enqueue_unreliable(channel_number, true, data);
            }
        }
        Ok(())
    }

    fn send_unreliable_raw_merge(
        &self,
        data: &[u8],
        offset: usize,
        length: usize,
        channel_number: u8,
        patch_offset: i32,
        patch_value: u8,
    ) -> Result<(), SendError> {
        let Some(slice) = offset.checked_add(length).and_then(|end| data.get(offset..end)) else {
            return Err(SendError::BadRange { offset, length, len: data.len() });
        };
        match usize::try_from(patch_offset) {
            Ok(patch) if patch < length => {
                let mut patched = slice.to_vec();
                if let Some(byte) = patched.get_mut(patch) {
                    *byte = patch_value;
                }
                self.send(&patched, channel_number, DeliveryMethod::Unreliable)
            }
            _ => self.send(slice, channel_number, DeliveryMethod::Unreliable),
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

/// Releases one pending-handshake slot when it goes out of scope, however `serve_connect`
/// returns.
struct PendingHandshakeSlot {
    manager: Arc<ManagerInner>,
}

impl Drop for PendingHandshakeSlot {
    fn drop(&mut self) {
        self.manager.pending_handshakes.fetch_sub(1, Ordering::AcqRel);
    }
}

/// How long an undecided connection request stays open before the transport rejects it.
const REQUEST_DECISION_TIMEOUT: Duration = Duration::from_secs(15);

const REQUEST_UNDECIDED: u8 = 0;
const REQUEST_ACCEPTED: u8 = 1;
const REQUEST_REJECTED: u8 = 2;

struct IrohConnectionRequest {
    manager: Arc<ManagerInner>,
    conn: Connection,
    control_tx: Mutex<Option<SendStream>>,
    control_rx: Mutex<Option<RecvStream>>,
    data: Vec<u8>,
    remote: SocketAddr,
    decided: std::sync::atomic::AtomicU8,
    accepted: Mutex<Option<NetPeerRef>>,
}

impl IrohConnectionRequest {
    fn is_decided(&self) -> bool {
        self.decided.load(Ordering::SeqCst) != REQUEST_UNDECIDED
    }
}

impl ConnectionRequest for IrohConnectionRequest {
    fn data(&self) -> NetDataReader {
        NetDataReader::from_slice(&self.data)
    }

    fn remote_end_point(&self) -> SocketAddr {
        self.remote
    }

    fn accept(&self) -> BasisResult<NetPeerRef> {
        if let Err(current) =
            self.decided.compare_exchange(REQUEST_UNDECIDED, REQUEST_ACCEPTED, Ordering::SeqCst, Ordering::SeqCst)
        {
            return match (current, self.accepted.lock().clone()) {
                (REQUEST_ACCEPTED, Some(peer)) => Ok(peer),
                (REQUEST_ACCEPTED, None) => Err(BasisError::permanent(
                    ErrorCode::Conflict,
                    format!("connection request from {} is being accepted on another thread", self.remote),
                )),
                _ => Err(BasisError::permanent(
                    ErrorCode::Conflict,
                    format!("connection request from {} was already rejected", self.remote),
                )),
            };
        }
        let peer = self.manager.admit(self.conn.clone(), self.remote.ip(), true, 0);
        let peer_ref: NetPeerRef = Arc::new(peer.clone());
        *self.accepted.lock() = Some(peer_ref.clone());
        let tx = self.control_tx.lock().take();
        let rx = self.control_rx.lock().take();
        let state = peer.state.clone();
        let manager = self.manager.clone();
        let remote = self.remote;
        if let Err(e) = IrohRuntime::spawn(async move {
            if let Some(mut tx) = tx {
                let mut msg = vec![CTL_ACCEPTED];
                msg.extend_from_slice(&(state.id as u16).to_le_bytes());
                if tx.write_all(&msg).await.is_err() {
                    state.conn.close(VarInt::from_u32(CLOSE_PROTOCOL), b"accept write failed");
                    manager.finish_peer(state, DisconnectReason::ConnectionFailed, None);
                    return;
                }
                *state.control_tx.lock().await = Some(tx);
            }
            manager.run_peer(state, rx).await;
        }) {
            // No runtime to run the peer on: undo the admission so the caller sees the failure
            // rather than a peer that never speaks.
            self.conn.close(VarInt::from_u32(CLOSE_PROTOCOL), b"no runtime");
            self.manager.finish_peer(peer.state.clone(), DisconnectReason::ConnectionFailed, None);
            return Err(e.context(format!("accepting the connection from {remote}")));
        }
        Ok(peer_ref)
    }

    fn reject(&self, w: &NetDataWriter) -> BasisResult<()> {
        if let Err(current) =
            self.decided.compare_exchange(REQUEST_UNDECIDED, REQUEST_REJECTED, Ordering::SeqCst, Ordering::SeqCst)
        {
            return if current == REQUEST_REJECTED {
                Ok(())
            } else {
                Err(BasisError::permanent(
                    ErrorCode::Conflict,
                    format!("connection request from {} was already accepted", self.remote),
                ))
            };
        }
        let data = w.copy_data();
        let conn = self.conn.clone();
        let tx = self.control_tx.lock().take();
        let spawned = IrohRuntime::spawn(async move {
            if let Some(mut tx) = tx {
                let mut msg = vec![CTL_REJECTED];
                msg.extend_from_slice(&(u32::try_from(data.len()).unwrap_or(u32::MAX)).to_le_bytes());
                msg.extend_from_slice(&data);
                let _ = tx.write_all(&msg).await;
                let _ = tx.finish();
                // Give the frame a moment to leave before the close races it.
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            conn.close(VarInt::from_u32(CLOSE_REJECTED), b"rejected");
        });
        if let Err(e) = spawned {
            // The verdict cannot be sent; closing the connection still refuses it.
            self.conn.close(VarInt::from_u32(CLOSE_REJECTED), b"rejected");
            return Err(e.context(format!("rejecting the connection from {}", self.remote)));
        }
        Ok(())
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
    /// Peer ids, reused lowest-first like LiteNetLib. Shared with the LiteNetLib manager when
    /// both stacks serve one world, so a player id is unique across transports.
    ids: Arc<PeerIdAllocator>,
    /// Whether `stop` may reset the allocator: false when it was handed in by a mixed stack.
    owns_ids: bool,
    running: AtomicBool,
    accept_task: Mutex<Option<JoinHandle<()>>>,
    packets_sent: AtomicU64,
    packets_received: AtomicU64,
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
    /// UDP packets and bytes of connections that have closed, so the totals never go backwards.
    retired_udp: [AtomicU64; 4],
    unreliable_dropped: AtomicI64,
    priority_dropped: AtomicI64,
    /// Probe connections awaiting a reply, keyed by the remote address the handler saw.
    probe_replies: DashMap<SocketAddr, Connection>,
    /// Connections between the QUIC handshake and a connect verdict. Each holds a task, two
    /// streams and the connect payload, so it is memory an unauthenticated peer can allocate:
    /// bounded by count, not by the decision timeout alone.
    pending_handshakes: AtomicI32,
    handshake_overflow_logged: AtomicU32,
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
        Self::build(listener, transport, enable_statistics, secret_key, PeerIdAllocator::new(), true)
    }

    /// Builds a manager that draws peer ids from `ids`, an allocator another transport shares.
    pub fn with_id_allocator(
        listener: Arc<EventBasedNetListener>,
        transport: IrohTransportConfig,
        enable_statistics: bool,
        secret_key: Option<SecretKey>,
        ids: Arc<PeerIdAllocator>,
    ) -> Self {
        Self::build(listener, transport, enable_statistics, secret_key, ids, false)
    }

    fn build(
        listener: Arc<EventBasedNetListener>,
        transport: IrohTransportConfig,
        enable_statistics: bool,
        secret_key: Option<SecretKey>,
        ids: Arc<PeerIdAllocator>,
        owns_ids: bool,
    ) -> Self {
        IrohRuntime::configure_worker_threads(transport.tokio_worker_threads);
        let inner = Arc::new(ManagerInner {
            listener,
            endpoint: RwLock::new(None),
            secret_key: secret_key.unwrap_or_else(SecretKey::generate),
            transport_config: transport,
            enable_statistics,
            priority_channels: BasisNetworkCommons::build_priority_unreliable_channel_map(),
            peers: DashMap::new(),
            ids,
            owns_ids,
            running: AtomicBool::new(false),
            accept_task: Mutex::new(None),
            packets_sent: AtomicU64::new(0),
            packets_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            retired_udp: std::array::from_fn(|_| AtomicU64::new(0)),
            unreliable_dropped: AtomicI64::new(0),
            priority_dropped: AtomicI64::new(0),
            probe_replies: DashMap::new(),
            pending_handshakes: AtomicI32::new(0),
            handshake_overflow_logged: AtomicU32::new(0),
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

    /// Application frames handed to the transport (each `send`), as opposed to the UDP packets
    /// they became — see [`NetManager::statistics`]. quinn packs queued datagram frames into
    /// packets itself, which is why the two differ and why no framing above it is needed.
    pub fn frames_sent(&self) -> u64 {
        self.inner.packets_sent.load(Ordering::Relaxed)
    }

    /// Probes a server for its info line (the counterpart of the unconnected UDP query).
    pub async fn probe(target: ConnectionTarget, timeout_ms: i32) -> ServerProbeResult {
        let mut result = ServerProbeResult::default();
        let addr = match ManagerInner::resolve_target(&target).await {
            Ok(a) => a,
            Err(e) => {
                result.error = e.to_string();
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
    /// Parses a connection target into an endpoint address. IP literals are applied here; a
    /// host name is returned separately for [`resolve_target`](Self::resolve_target), because
    /// name resolution blocks and must not run on a transport worker.
    fn parse_target(target: &ConnectionTarget) -> BasisResult<(EndpointAddr, Option<(String, u16)>)> {
        let mut t = target.clone();
        if t.get(ConnectionTargetKeys::ENDPOINT_ID).is_none() {
            use super::connection_target::IConnectionTargetParser;
            IrohConnectionTargetParser.parse(&mut t);
        }
        let id_text = t
            .get(ConnectionTargetKeys::ENDPOINT_ID)
            .ok_or_else(|| BasisError::permanent(ErrorCode::InvalidArgument, "connection string has no endpoint id"))?;
        let id = Self::parse_endpoint_id(&id_text)?;
        let mut addr = EndpointAddr::new(id);
        let mut host_to_resolve = None;
        if let Some(host) = t.get(ConnectionTargetKeys::ADDRESS) {
            let port = t
                .get(ConnectionTargetKeys::PORT)
                .and_then(|p| p.parse::<u16>().ok())
                .unwrap_or(LNLConnectionTargetParser::DEFAULT_PORT);
            match host.parse::<IpAddr>() {
                Ok(ip) => addr = addr.with_ip_addr(SocketAddr::new(ip, port)),
                Err(_) => host_to_resolve = Some((host, port)),
            }
        }
        if let Some(relay) = t.get(ConnectionTargetKeys::RELAY_URL) {
            let url = relay.parse().map_err(|e| {
                BasisError::permanent(ErrorCode::InvalidArgument, format!("'{relay}' is not a relay url: {e}"))
            })?;
            addr = addr.with_relay_url(url);
        }
        Ok((addr, host_to_resolve))
    }

    /// [`parse_target`](Self::parse_target) plus asynchronous name resolution. A name server
    /// that does not answer is a transient fault; a name that does not exist is permanent.
    async fn resolve_target(target: &ConnectionTarget) -> BasisResult<EndpointAddr> {
        let (mut addr, host) = Self::parse_target(target)?;
        if let Some((host, port)) = host {
            let resolved = tokio::net::lookup_host((host.as_str(), port)).await.map_err(|e| {
                let kind = basis_error::io_fault_kind(e.kind());
                BasisError::with_source(kind, ErrorCode::Dns, format!("could not resolve '{host}'"), e)
            })?;
            let mut any = false;
            for socket in resolved {
                addr = addr.with_ip_addr(socket);
                any = true;
            }
            if !any {
                return Err(BasisError::permanent(ErrorCode::Dns, format!("'{host}' has no addresses")));
            }
        }
        Ok(addr)
    }

    fn parse_endpoint_id(text: &str) -> BasisResult<EndpointId> {
        let text = text.trim();
        if let Ok(id) = EndpointId::from_z32(text) {
            return Ok(id);
        }
        text.parse::<EndpointId>()
            .map_err(|e| BasisError::permanent(ErrorCode::InvalidArgument, format!("'{text}' is not an endpoint id: {e}")))
    }

    fn allocate_id(&self) -> i32 {
        self.ids.allocate()
    }

    fn release_id(&self, id: i32) {
        self.ids.release(id);
    }

    fn queue_limits(&self) -> (u32, u32) {
        let peers = self.peers.len() as i32;
        let bulk = BasisPopulationScale::unreliable_queue_per_peer(self.transport_config.max_datagram_queue_per_peer, peers);
        let priority = BasisPopulationScale::priority_queue_per_peer(self.transport_config.max_priority_datagram_queue_per_peer, peers);
        (bulk as u32, priority as u32)
    }

    /// Per-peer reliable byte budget for the current population.
    fn reliable_budget(&self) -> usize {
        let peers = self.peers.len() as i32;
        usize::try_from(BasisPopulationScale::reliable_queue_bytes_per_peer(self.transport_config.max_reliable_queue_bytes_per_peer, peers)).unwrap_or(0)
    }

    fn reliable_grace(&self) -> Duration {
        Duration::from_millis(u64::try_from(self.transport_config.reliable_queue_grace_ms.max(0)).unwrap_or(5000))
    }

    fn admit(self: &Arc<Self>, conn: Connection, remote: IpAddr, server_side: bool, remote_id: i32) -> IrohNetPeer {
        let id = if server_side { self.allocate_id() } else { 0 };
        let (bulk, priority) = self.queue_limits();
        let reliable_budget = self.reliable_budget();
        let state = Arc::new(PeerState {
            id,
            remote_id: AtomicI32::new(remote_id),
            identity: next_peer_identity(),
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
            reliable_queued_bytes: AtomicUsize::new(0),
            reliable_budget: AtomicUsize::new(reliable_budget),
            last_reliable_drain: Mutex::new(Instant::now()),
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
            warned_too_large: AtomicBool::new(false),
            warned_backlog: AtomicBool::new(false),
            local_disconnect_reason: Mutex::new(None),
        });
        let peer = IrohNetPeer::new(state);
        self.peers.insert(id, peer.clone());
        // Queue bounds follow the population, resolved on every join/leave.
        self.refresh_queue_limits();
        peer
    }

    fn refresh_queue_limits(&self) {
        let (bulk, priority) = self.queue_limits();
        let reliable_budget = self.reliable_budget();
        for p in self.peers.iter() {
            p.state.reliable_budget.store(reliable_budget, Ordering::Relaxed);
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
        // Bound what QUIC itself holds per connection, not just what our reliable queue holds:
        // the send window caps unacknowledged data buffered for a peer, the receive window caps
        // data a peer can make us hold before the application reads it. Both are memory a single
        // connection can pin, so they are configurable and default to sane ceilings.
        let send_window = u64::try_from(self.transport_config.send_window_bytes).ok().filter(|v| *v > 0).unwrap_or(8 * 1024 * 1024);
        let receive_window = u32::try_from(self.transport_config.receive_window_bytes).ok().filter(|v| *v > 0).unwrap_or(32 * 1024 * 1024);
        // How often the *peer* acknowledges what we send it, which is the only lever QUIC gives
        // us over a cost this workload feels acutely. Every datagram frame is ack-eliciting, so
        // at the default (acknowledge every second one) a room of 200 peers sends this server
        // roughly 11k ACK-only packets a second — 28 % of all the packets it handles, for
        // traffic that is unreliable and whose loss we do not act on. Both ends of every
        // connection this server talks to are built from this same function (the Rust clients
        // directly, the C# clients through `basis_iroh_ffi`), so configuring it here configures
        // the whole conversation.
        //
        // The threshold is the packet count a peer may hold before it must acknowledge; the
        // delay is the time it may hold them. QUIC clamps the effective delay to at most the
        // greater of the path RTT and 25 ms, so 25 ms is the ceiling on a LAN and the timer,
        // not the threshold, is what usually fires: about 40 acknowledgements per second per
        // peer instead of one per two packets. Loss detection for the reliable streams pays up
        // to that same 25 ms, which is inside this server's tick budget.
        let mut ack_frequency = AckFrequencyConfig::default();
        ack_frequency
            .ack_eliciting_threshold(VarInt::from_u32(10))
            .max_ack_delay(Some(Duration::from_millis(25)));
        QuicTransportConfig::builder()
            .max_idle_timeout(IdleTimeout::try_from(idle).ok())
            .ack_frequency_config(Some(ack_frequency))
            .keep_alive_interval(keep_alive)
            .max_concurrent_uni_streams(VarInt::from_u32(4096))
            .datagram_receive_buffer_size(Some(4 * 1024 * 1024))
            // Where an unreliable backlog now lives. The sender task pushes frames without
            // waiting for buffer space (see `sender_task`), so this buffer — not our per-peer
            // queue — is what a stalled path fills, and QUIC drops its oldest frames to keep the
            // newest. 4 MiB of MTU-sized frames is ~2800 of them: half a minute of one peer's
            // traffic, which is stale state nobody wants delivered and 800 MB across a full
            // room. 256 KiB is ~180 frames, a second or two — enough to ride out a brief stall,
            // little enough that recovery is not a replay of the distant past.
            .datagram_send_buffer_size(256 * 1024)
            .send_window(send_window)
            .receive_window(VarInt::from_u32(receive_window))
            .build()
    }

    async fn bind(self: Arc<Self>, ipv4: IpAddr, ipv6: IpAddr, port: u16) -> BasisResult<()> {
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
        let bad_addr = |addr: SocketAddr, e: iroh::endpoint::InvalidSocketAddr| {
            BasisError::with_source(FaultKind::Permanent, ErrorCode::Config, format!("cannot bind {addr}"), e)
        };
        let build = |with_v6: bool| -> BasisResult<iroh::endpoint::Builder> {
            let mut builder = Endpoint::builder(presets::Minimal)
                .secret_key(self.secret_key.clone())
                .alpns(vec![BASIS_ALPN.to_vec(), BASIS_PROBE_ALPN.to_vec()])
                .relay_mode(relay_mode.clone())
                .transport_config(self.build_transport_config());
            if let IpAddr::V4(v4) = ipv4 {
                let addr = SocketAddr::new(IpAddr::V4(v4), port);
                builder = builder.bind_addr(addr).map_err(|e| bad_addr(addr, e))?;
            }
            if with_v6 && let IpAddr::V6(v6) = ipv6 {
                // Same port on both families when one was asked for; an OS-picked port (0) is
                // picked independently per socket.
                let addr = SocketAddr::new(IpAddr::V6(v6), port);
                builder = builder.bind_addr(addr).map_err(|e| bad_addr(addr, e))?;
            }
            Ok(builder)
        };
        let bind_error = |e: iroh::endpoint::BindError| {
            // A port still in TIME_WAIT is worth retrying; a bad address or config is not.
            let kind = basis_error::fault_kind_from_chain(&e).unwrap_or(FaultKind::Permanent);
            BasisError::with_source(kind, ErrorCode::Transport, format!("binding the iroh endpoint on port {port} failed"), e)
        };
        let endpoint = match build(true)?.bind().await {
            Ok(endpoint) => endpoint,
            Err(e) if matches!(ipv6, IpAddr::V6(_)) => {
                // A host without IPv6 must still come up: retry on IPv4 alone, as the C# server
                // did when its second socket failed to bind.
                BNL::log_warning(format!("[iroh] bind with IPv6 failed ({e}); retrying IPv4 only"));
                build(false)?.bind().await.map_err(bind_error)?
            }
            Err(e) => return Err(bind_error(e)),
        };
        *self.endpoint.write() = Some(endpoint.clone());
        self.running.store(true, Ordering::SeqCst);
        let me = self.clone();
        let task = IrohRuntime::spawn(async move { me.accept_loop(endpoint).await })?;
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
            IrohRuntime::spawn_detached(async move {
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

    /// Probe connections awaiting a reply at once. A probe is unauthenticated and cheap to
    /// send, so the map that lets the handler answer one is bounded by count rather than by the
    /// half-second the entry would otherwise live for.
    const MAX_PROBE_REPLIES: usize = 1024;

    async fn serve_probe(self: Arc<Self>, conn: Connection, remote: SocketAddr) {
        if self.probe_replies.len() >= Self::MAX_PROBE_REPLIES {
            // Already answering as many probes as we will hold state for; this one is refused
            // outright rather than queued. The server-info query is idempotent and retried.
            conn.close(VarInt::from_u32(CLOSE_NORMAL), b"probe capacity");
            return;
        }
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
        // A hard cap on connections between the QUIC handshake and a verdict. Without it the
        // only thing limiting them is how long each waits, which is a rate, not a bound: a peer
        // opening connections faster than they time out grows this without limit.
        let limit = if self.transport_config.max_pending_handshakes > 0 { self.transport_config.max_pending_handshakes } else { 1024 };
        if self.pending_handshakes.fetch_add(1, Ordering::AcqRel) >= limit {
            self.pending_handshakes.fetch_sub(1, Ordering::AcqRel);
            if self.handshake_overflow_logged.fetch_add(1, Ordering::Relaxed).is_multiple_of(1000) {
                BNL::log_warning(format!(
                    "[iroh] {limit} connections are already awaiting a connect verdict (the cap); closing the one from {remote}."
                ));
            }
            conn.close(VarInt::from_u32(CLOSE_PROTOCOL), b"handshake capacity");
            return;
        }
        // Released however this returns, so a panic or an early return cannot leak a slot.
        let _slot = PendingHandshakeSlot { manager: self.clone() };
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
            control_tx: Mutex::new(Some(tx)),
            control_rx: Mutex::new(Some(rx)),
            data: payload,
            remote,
            decided: std::sync::atomic::AtomicU8::new(REQUEST_UNDECIDED),
            accepted: Mutex::new(None),
        });
        // Raised on a blocking thread: the server's handler is synchronous and may take the
        // tokio thread for longer than a scheduler slot should allow.
        let listener = self.listener.clone();
        let req = request.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || listener.raise_connection_request(req)).await {
            BNL::log_error(format!("[iroh] the connection request handler for {remote} did not complete: {e}"));
        }
        // A handler may hold the request and decide later — a client that hands connection
        // requests to another thread (the FFI event queue, a UI) does exactly that. LiteNetLib
        // kept an undecided request until its timeout; so does this.
        let deadline = tokio::time::Instant::now() + REQUEST_DECISION_TIMEOUT;
        while !request.is_decided() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if !request.is_decided() {
            // No handler decided in time: LiteNetLib would time the request out.
            if let Err(e) = request.reject(&NetDataWriter::new()) {
                BNL::log_warning(format!("[iroh] could not reject the undecided connection from {remote}: {e}"));
            }
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
        let tasks = (|| -> BasisResult<_> {
            Ok((
                IrohRuntime::spawn(Self::sender_task(self.clone(), state.clone()))?,
                IrohRuntime::spawn(Self::datagram_task(self.clone(), state.clone()))?,
                IrohRuntime::spawn(Self::stream_accept_task(self.clone(), state.clone()))?,
                IrohRuntime::spawn(Self::ping_task(state.clone()))?,
                IrohRuntime::spawn(Self::control_task(self.clone(), state.clone(), control_rx))?,
            ))
        })();
        let (sender, datagrams, streams, pinger, control) = match tasks {
            Ok(tasks) => tasks,
            Err(e) => {
                BNL::log_error(format!("[iroh] peer {} cannot be served: {e}", state.id));
                state.conn.close(VarInt::from_u32(CLOSE_PROTOCOL), b"no runtime");
                self.finish_peer(state, DisconnectReason::ConnectionFailed, None);
                return;
            }
        };

        let error = state.conn.closed().await;
        state.connected.store(false, Ordering::SeqCst);
        state.notify.notify_one();
        sender.abort();
        datagrams.abort();
        streams.abort();
        pinger.abort();
        let disconnect_data = control.await.ok().flatten();

        let reason = state.local_disconnect_reason.lock().take().unwrap_or_else(|| match &error {
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
        });
        self.finish_peer(state, reason, disconnect_data);
    }

    /// What the connection actually put on the wire: UDP packets and bytes each way.
    fn udp_stats(conn: &Connection) -> [u64; 4] {
        let stats = conn.stats();
        [stats.udp_tx.datagrams, stats.udp_tx.bytes, stats.udp_rx.datagrams, stats.udp_rx.bytes]
    }

    fn finish_peer(&self, state: Arc<PeerState>, reason: DisconnectReason, additional: Option<Vec<u8>>) {
        if state.disconnect_raised.swap(true, Ordering::SeqCst) {
            return;
        }
        state.connected.store(false, Ordering::SeqCst);
        for (total, value) in self.retired_udp.iter().zip(Self::udp_stats(&state.conn)) {
            total.fetch_add(value, Ordering::Relaxed);
        }
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

    /// Moves queued frames into `batch` under one lock acquisition, decrementing the per-channel
    /// depth as each leaves the queue, up to `DATAGRAM_BATCH` in total across both queues.
    fn drain_into(state: &PeerState, queue: &Mutex<VecDeque<Bytes>>, batch: &mut Vec<Bytes>) {
        if batch.len() >= Self::DATAGRAM_BATCH {
            return;
        }
        let mut queue = queue.lock();
        while batch.len() < Self::DATAGRAM_BATCH {
            let Some(frame) = queue.pop_front() else { break };
            let channel = frame.first().copied().unwrap_or(0) & DATAGRAM_CHANNEL_MASK;
            if let Some(counter) = state.queued_per_channel.get(usize::from(channel)) {
                counter.fetch_sub(1, Ordering::Relaxed);
            }
            batch.push(frame);
        }
    }

    /// Datagram frames one wake-up may push before the loop looks at newly arrived voice and at
    /// the reliable queue again. A tick queues one frame per peer, so a steady-state pass moves
    /// one or two; the bound only matters when a backlog has built, where it keeps a bulk burst
    /// from starving everything else on this connection.
    const DATAGRAM_BATCH: usize = 64;

    /// A backlog this deep is where the connection's own datagram buffer is worth a look; below
    /// it the buffer is empty by construction and the probe would cost a connection lock per
    /// frame, which is exactly what draining in batches exists to avoid.
    const BACKLOG_PROBE_AT: usize = 8;

    async fn sender_task(self: Arc<Self>, state: Arc<PeerState>) {
        let mut ordered_streams: HashMap<u8, SendStream> = HashMap::new();
        // Reused across wake-ups, so the steady state neither allocates nor grows it.
        let mut batch: Vec<Bytes> = Vec::with_capacity(Self::DATAGRAM_BATCH);
        loop {
            // Voice first, then bulk, then reliable — the priority the C# transport gave voice —
            // but drained under one lock acquisition per queue rather than one per frame, and
            // sent without awaiting: an unreliable frame that cannot go now is dropped, never
            // waited on. `send_datagram` drops the oldest queued datagrams to make room, which
            // is the policy our own queues already use and the one LiteNetLib's unreliable path
            // has always had. `send_datagram_wait` did the opposite — it held this task until
            // buffer space appeared, prioritising stale frames over fresh ones and costing a
            // future poll round trip per frame.
            Self::drain_into(&state, &state.priority_queue, &mut batch);
            Self::drain_into(&state, &state.bulk_queue, &mut batch);
            if !batch.is_empty() {
                if batch.len() >= Self::BACKLOG_PROBE_AT {
                    let wanted: usize = batch.iter().map(|frame| frame.len()).sum();
                    if state.conn.datagram_send_buffer_space() < wanted && !state.warned_backlog.swap(true, Ordering::Relaxed) {
                        BNL::log_warning(format!(
                            "[iroh] peer {}: the connection's datagram buffer cannot hold a {} frame backlog; QUIC is dropping the oldest frames to keep the newest. Counted by QUIC, not by the per-peer drop counters.",
                            state.id,
                            batch.len()
                        ));
                    }
                }
                for frame in batch.drain(..) {
                    let len = frame.len();
                    match state.conn.send_datagram(frame) {
                        Ok(()) => self.record_sent(len),
                        Err(iroh::endpoint::SendDatagramError::TooLarge) => {
                            // The path MTU shrank under a frame that fitted when it was queued. It
                            // is unreliable traffic: drop it, count it, say so once.
                            self.unreliable_dropped.fetch_add(1, Ordering::Relaxed);
                            if !state.warned_too_large.swap(true, Ordering::Relaxed) {
                                BNL::log_warning(format!(
                                    "[iroh] peer {}: a {len} byte datagram no longer fits the path MTU; dropping such frames",
                                    state.id
                                ));
                            }
                        }
                        Err(iroh::endpoint::SendDatagramError::ConnectionLost(_)) => return,
                        Err(e) => {
                            // Datagrams disabled or unsupported: the peer cannot speak this protocol.
                            BNL::log_error(format!("[iroh] peer {}: datagrams unavailable ({e}); closing", state.id));
                            state.conn.close(VarInt::from_u32(CLOSE_PROTOCOL), b"datagrams unavailable");
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
                    // The bytes leave the queue for the wire now: stamp the drain so the watchdog
                    // knows the peer is still accepting reliable data. Saturating so a miscount
                    // can never wrap the counter.
                    state.reliable_queued_bytes.fetch_update(Ordering::AcqRel, Ordering::Acquire, |q| Some(q.saturating_sub(len))).ok();
                    *state.last_reliable_drain.lock() = Instant::now();
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
                        let data: Vec<u8> = data.into_iter().take(MAX_FRAME_BYTES).collect();
                        msg.extend_from_slice(&(u32::try_from(data.len()).unwrap_or(u32::MAX)).to_le_bytes());
                        msg.extend_from_slice(&data);
                        let _ = tx.write_all(&msg).await;
                    }
                    for (_, mut s) in ordered_streams.drain() {
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
        let s = match streams.entry(channel) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let mut s = state.conn.open_uni().await.map_err(|_| ())?;
                s.write_all(&[STREAM_RELIABLE_ORDERED, channel]).await.map_err(|_| ())?;
                entry.insert(s)
            }
        };
        let len = u32::try_from(data.len()).map_err(|_| ())?;
        let mut frame = BytesMut::with_capacity(4 + data.len());
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&data);
        if s.write_all(&frame).await.is_err() {
            streams.remove(&channel);
            return Err(());
        }
        Ok(())
    }

    async fn send_on_fresh_stream(state: &PeerState, channel: u8, data: Bytes) -> Result<(), ()> {
        let mut s = state.conn.open_uni().await.map_err(|_| ())?;
        let len = u32::try_from(data.len()).map_err(|_| ())?;
        let mut frame = BytesMut::with_capacity(6 + data.len());
        frame.extend_from_slice(&[STREAM_RELIABLE_UNORDERED, channel]);
        frame.extend_from_slice(&len.to_le_bytes());
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
            let Some(&header) = datagram.first() else { continue };
            let channel = header & DATAGRAM_CHANNEL_MASK;
            if (header & DATAGRAM_SEQUENCED_FLAG) != 0 {
                let (Some(&lo), Some(&hi)) = (datagram.get(1), datagram.get(2)) else {
                    continue;
                };
                let seq = u16::from_le_bytes([lo, hi]);
                {
                    let mut last = state.sequenced_in.lock();
                    let Some(slot) = last.get_mut(usize::from(channel)) else { continue };
                    if let Some(prev) = *slot
                        && (seq.wrapping_sub(prev) as i16) <= 0
                    {
                        continue; // older than what we already delivered
                    }
                    *slot = Some(seq);
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
            IrohRuntime::spawn_detached(async move { me.stream_reader(st, rx).await });
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
            // Pooled and handed to the reader as shared `Bytes`, recycling when the handler is
            // done. Zeroed on rent — a bounded memset the stream read then overwrites — so the
            // pool never exposes an API that could return another connection's stale bytes.
            let mut payload = PacketBufferPool::rent_zeroed(len);
            if rx.read_exact(&mut payload).await.is_err() {
                return;
            }
            *state.last_packet.lock() = Instant::now();
            self.record_received(len + 4);
            self.listener.raise_network_receive(peer.clone(), NetPacketReader::new(payload), channel, method);
        }
    }

    async fn ping_task(state: Arc<PeerState>) {
        // Half the ping interval, so the send-queue watchdog reacts within ~grace + 750 ms
        // rather than only on ping boundaries.
        let interval = Duration::from_millis((BasisNetworkCommons::PING_INTERVAL as u64 / 2).max(1));
        let grace = Duration::from_millis(u64::try_from(BasisNetworkCommons::PING_INTERVAL).unwrap_or(1500));
        let grace = state.manager.upgrade().map(|m| m.reliable_grace()).unwrap_or(grace);
        let mut since_last_ping = Duration::ZERO;
        loop {
            tokio::time::sleep(interval).await;
            if !state.connected.load(Ordering::Relaxed) {
                return;
            }
            // Watchdog: a peer whose reliable queue has been over budget for the whole grace
            // period is not reading. Close it with a reason the server can act on, rather than
            // keep buffering messages it will never receive.
            let queued = state.reliable_queued_bytes.load(Ordering::Relaxed);
            if queued > 0 && state.last_reliable_drain.lock().elapsed() >= grace {
                BNL::log_warning(format!(
                    "[iroh] peer {}: {queued} bytes of reliable data have not left the send queue for over {}s; disconnecting a client that is not reading.",
                    state.id,
                    grace.as_secs()
                ));
                *state.local_disconnect_reason.lock() = Some(DisconnectReason::SendQueueOverBudget);
                state.connected.store(false, Ordering::SeqCst);
                state.conn.close(VarInt::from_u32(CLOSE_FORCE), b"send queue over budget");
                state.notify.notify_one();
                return;
            }
            // The control-stream ping only needs the full interval; run the watchdog twice as often.
            since_last_ping += interval;
            if since_last_ping < Duration::from_millis(BasisNetworkCommons::PING_INTERVAL as u64) {
                continue;
            }
            since_last_ping = Duration::ZERO;
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
                    let mut sent = [0u8; 8];
                    let mut theirs = [0u8; 8];
                    if rx.read_exact(&mut sent).await.is_err() || rx.read_exact(&mut theirs).await.is_err() {
                        return None;
                    }
                    let sent_ticks = i64::from_le_bytes(sent);
                    let their_ticks = i64::from_le_bytes(theirs);
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
    async fn connect_async(self: Arc<Self>, target: ConnectionTarget, payload: Vec<u8>, shared: Arc<PendingShared>) {
        let mut remote_ip = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
        let failed = |me: &Arc<Self>, remote_ip: IpAddr, reason: DisconnectReason, data: Option<Vec<u8>>| {
            // No live peer yet: the pending peer the caller holds is the one that "disconnected",
            // so the listener hears the outcome exactly as LiteNetLib reported a failed connect.
            shared.cancelled.store(true, Ordering::SeqCst);
            let peer: NetPeerRef = Arc::new(PendingPeer { remote_ip, shared: shared.clone() });
            me.listener.raise_peer_disconnected(
                peer,
                DisconnectInfo { reason, socket_error_code: 0, additional_data: NetPacketReader::new(data.unwrap_or_default()) },
            );
        };
        let Some(endpoint) = self.endpoint.read().clone() else {
            BNL::log_error("[iroh] connect before start");
            failed(&self, remote_ip, DisconnectReason::ConnectionFailed, None);
            return;
        };
        let addr = match Self::resolve_target(&target).await {
            Ok(addr) => addr,
            Err(e) => {
                BNL::log_warning(format!("[iroh] connect target could not be resolved: {e}"));
                failed(&self, remote_ip, DisconnectReason::UnknownHost, None);
                return;
            }
        };
        remote_ip = addr.ip_addrs().next().map(|s| s.ip()).unwrap_or(remote_ip);
        if payload.len() > MAX_FRAME_BYTES {
            BNL::log_error(format!("[iroh] connect payload of {} bytes exceeds {MAX_FRAME_BYTES}", payload.len()));
            failed(&self, remote_ip, DisconnectReason::ConnectionFailed, None);
            return;
        }
        let conn = match endpoint.connect(addr, BASIS_ALPN).await {
            Ok(c) => c,
            Err(e) => {
                BNL::log_warning(format!("[iroh] connect failed: {e}"));
                failed(&self, remote_ip, DisconnectReason::ConnectionFailed, None);
                return;
            }
        };
        if shared.cancelled.load(Ordering::SeqCst) {
            conn.close(VarInt::from_u32(CLOSE_NORMAL), b"cancelled");
            failed(&self, remote_ip, DisconnectReason::DisconnectPeerCalled, None);
            return;
        }
        let (bulk, priority) = self.queue_limits();
        // Bytes of the early reliable sends already sitting in the queue, so the sender's
        // per-frame decrement stays balanced and the counter never underflows.
        let early_reliable_bytes: usize = shared.early.lock().iter().map(|o| if let Outgoing::Reliable { data, .. } = o { data.len() } else { 0 }).sum();
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
            reliable_queued_bytes: AtomicUsize::new(early_reliable_bytes),
            // A client dialling out is not a target for a queue-flood; give it the ceiling and
            // no watchdog pressure (the server is the one that must bound a peer that stalls).
            reliable_budget: AtomicUsize::new(0),
            last_reliable_drain: Mutex::new(Instant::now()),
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
            warned_too_large: AtomicBool::new(false),
            warned_backlog: AtomicBool::new(false),
            local_disconnect_reason: Mutex::new(None),
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
        msg.extend_from_slice(&(u32::try_from(payload.len()).unwrap_or(u32::MAX)).to_le_bytes());
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
    fn start(&self, ipv4_address: IpAddr, ipv6_address: IpAddr, set_port: u16) -> BasisResult<()> {
        if self.inner.running.load(Ordering::SeqCst) {
            return Err(BasisError::permanent(ErrorCode::Conflict, "the iroh transport is already started"));
        }
        let inner = self.inner.clone();
        IrohRuntime::block_on(inner.bind(ipv4_address, ipv6_address, set_port))
            .and_then(|r| r)
            .map_err(|e| e.context(format!("starting the iroh transport on port {set_port}")))
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
        if let Some(ep) = endpoint
            && let Err(e) = IrohRuntime::block_on(async move { ep.close().await })
        {
            BNL::log_warning(format!("[iroh] endpoint close did not complete: {e}"));
        }
        for p in peers {
            self.inner.finish_peer(p.state.clone(), DisconnectReason::DisconnectPeerCalled, None);
        }
        self.inner.peers.clear();
        if self.inner.owns_ids {
            self.inner.ids.reset();
        }
    }

    fn connect(&self, target: &str, port: u16, writer: &NetDataWriter) -> BasisResult<NetPeerRef> {
        let raw = if target.contains('@') || port == 0 { target.to_string() } else { format!("{target}:{port}") };
        let mut ct = ConnectionTarget::new(BasisNetworkStackRegistry::IROH_ID, &raw);
        {
            use super::connection_target::IConnectionTargetParser;
            IrohConnectionTargetParser.parse(&mut ct);
        }
        if ct.get(ConnectionTargetKeys::ENDPOINT_ID).is_none() && !target.contains('@') {
            // "host:port" plus a separate endpoint id is what the C# signature could not carry;
            // callers pass "id@host" as the address instead.
            return Err(BasisError::permanent(
                ErrorCode::InvalidArgument,
                format!("'{target}' has no endpoint id: use 'endpointid@host:port'"),
            ));
        }
        // Validate now so a bad target fails at the call rather than inside the dial task; the
        // host name, if any, is resolved on the runtime.
        let (addr, _) = ManagerInner::parse_target(&ct).map_err(|e| e.context(format!("parsing connect target '{target}'")))?;
        if self.inner.endpoint.read().is_none() {
            return Err(BasisError::permanent(ErrorCode::Conflict, "connect before the iroh transport was started"));
        }
        let inner = self.inner.clone();
        let remote_ip = addr.ip_addrs().next().map(|s| s.ip()).unwrap_or(IpAddr::V6(Ipv6Addr::UNSPECIFIED));
        let shared = Arc::new(PendingShared {
            slot: Mutex::new(None),
            early: Mutex::new(VecDeque::new()),
            identity: next_peer_identity(),
            tag: RwLock::new(None),
            cancelled: AtomicBool::new(false),
        });
        let peer = PendingPeer { remote_ip, shared: shared.clone() };
        let payload = writer.copy_data();
        IrohRuntime::spawn(async move { inner.connect_async(ct, payload, shared).await })
            .map_err(|e| e.context(format!("dialing '{target}'")))?;
        Ok(Arc::new(peer))
    }

    fn send_unconnected_message(&self, writer: &NetDataWriter, remote_end_point: SocketAddr) -> bool {
        let Some((_, conn)) = self.inner.probe_replies.remove(&remote_end_point) else {
            return false;
        };
        let data = writer.copy_data();
        IrohRuntime::spawn_detached(async move {
            if let Ok(mut tx) = conn.open_uni().await {
                let _ = tx.write_all(&data).await;
                let _ = tx.finish();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            conn.close(VarInt::from_u32(CLOSE_NORMAL), b"done");
        });
        true
    }

    /// UDP packets and bytes on the wire, not application frames: what the C# statistics
    /// counted, and what a capacity figure has to be built from. Live connections are summed
    /// on every call; closed ones were folded into the retired totals as they left.
    fn statistics(&self) -> NetStatistics {
        let mut totals: [u64; 4] = std::array::from_fn(|i| self.inner.retired_udp[i].load(Ordering::Relaxed));
        for peer in self.inner.peers.iter() {
            for (total, value) in totals.iter_mut().zip(ManagerInner::udp_stats(&peer.state.conn)) {
                *total += value;
            }
        }
        NetStatistics {
            packets_sent: totals[0],
            bytes_sent: totals[1],
            packets_received: totals[2],
            bytes_received: totals[3],
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

    fn send(&self, data: &[u8], channel_number: u8, delivery_method: DeliveryMethod) -> Result<(), SendError> {
        if channel_number >= BasisNetworkCommons::TOTAL_CHANNELS {
            return Err(SendError::BadChannel { channel: channel_number, max: BasisNetworkCommons::TOTAL_CHANNELS });
        }
        match self.live() {
            Some(p) => p.send(data, channel_number, delivery_method),
            None => {
                // Only reliable sends survive the wait; an unreliable one would be stale by then.
                if delivery_method.is_reliable() {
                    self.shared.early.lock().push_back(Outgoing::Reliable {
                        channel: channel_number,
                        ordered: delivery_method != DeliveryMethod::ReliableUnordered,
                        data: Bytes::from(PacketBufferPool::rent_copy(data)),
                    });
                }
                Ok(())
            }
        }
    }

    fn send_unreliable_raw_merge(
        &self,
        data: &[u8],
        offset: usize,
        length: usize,
        channel_number: u8,
        patch_offset: i32,
        patch_value: u8,
    ) -> Result<(), SendError> {
        match self.live() {
            Some(p) => p.send_unreliable_raw_merge(data, offset, length, channel_number, patch_offset, patch_value),
            None => Ok(()),
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

