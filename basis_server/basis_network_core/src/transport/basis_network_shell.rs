use std::any::Any;
use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

use basis_error::{BasisError, BasisResult, ErrorCode};
use parking_lot::{Mutex, RwLock};

use crate::BNL;
use crate::io::{NetDataReader, NetDataWriter, NetPacketReader};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum DisconnectReason {
    ConnectionFailed,
    Timeout,
    HostUnreachable,
    NetworkUnreachable,
    RemoteConnectionClose,
    DisconnectPeerCalled,
    ConnectionRejected,
    InvalidProtocol,
    UnknownHost,
    Reconnect,
    PeerToPeerConnection,
    PeerNotFound,
    /// The peer stopped reading: its reliable send queue stayed over budget for the grace
    /// period, so the server closed the connection rather than keep buffering for it.
    SendQueueOverBudget,
}

#[derive(Clone, Debug)]
pub struct DisconnectInfo {
    pub reason: DisconnectReason,
    /// The C# `SocketError` code; 0 when the disconnect was not a socket failure.
    pub socket_error_code: i32,
    /// The bytes the other side attached (a reject reason, a structured reject payload).
    pub additional_data: NetPacketReader,
}

/// Sending method type. Values are LiteNetLib's, which is what the C# enum carried.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[repr(u8)]
pub enum DeliveryMethod {
    /// Unreliable. Packets can be dropped, can be duplicated, can arrive without order.
    Unreliable = 4,
    /// Reliable. Packets won't be dropped, won't be duplicated, can arrive without order.
    ReliableUnordered = 0,
    /// Unreliable. Packets can be dropped, won't be duplicated, will arrive in order.
    Sequenced = 1,
    /// Reliable and ordered. Packets won't be dropped, won't be duplicated, will arrive in order.
    ReliableOrdered = 2,
    /// Reliable only last packet. Packets can be dropped (except the last one), won't be
    /// duplicated, will arrive in order. Cannot be fragmented.
    ReliableSequenced = 3,
}

impl DeliveryMethod {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            4 => Some(Self::Unreliable),
            0 => Some(Self::ReliableUnordered),
            1 => Some(Self::Sequenced),
            2 => Some(Self::ReliableOrdered),
            3 => Some(Self::ReliableSequenced),
            _ => None,
        }
    }

    pub fn is_reliable(self) -> bool {
        matches!(self, Self::ReliableOrdered | Self::ReliableUnordered | Self::ReliableSequenced)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetLogLevel {
    Warning,
    Error,
    Trace,
    Info,
}

pub trait INetLogger: Send + Sync {
    fn write_net(&self, level: NetLogLevel, message: &str);
}

static NET_LOGGER: RwLock<Option<Arc<dyn INetLogger>>> = RwLock::new(None);

/// The transport's own log sink (LiteNetLib's `NetDebug`).
pub struct NetDebug;

impl NetDebug {
    pub fn set_logger(logger: Option<Arc<dyn INetLogger>>) {
        *NET_LOGGER.write() = logger;
    }

    pub fn logger() -> Option<Arc<dyn INetLogger>> {
        NET_LOGGER.read().clone()
    }

    pub fn write(level: NetLogLevel, message: &str) {
        if let Some(l) = Self::logger() {
            l.write_net(level, message);
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetStatistics {
    pub packets_sent: u64,
    pub packets_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub packet_loss: u64,
}

/// A pending inbound connection: what the client sent to connect, where from, and the two
/// verdicts. Decided by exactly one of `accept` / `reject`.
pub trait ConnectionRequest: Send + Sync {
    /// The connect payload (protocol version, auth bytes, ready message).
    fn data(&self) -> NetDataReader;
    fn remote_end_point(&self) -> SocketAddr;
    /// Admits the connection and returns its peer. Accepting twice returns the same peer;
    /// accepting after `reject` is a [`Conflict`](ErrorCode::Conflict) error.
    fn accept(&self) -> BasisResult<NetPeerRef>;
    /// Refuses the connection, sending `w` as the reject payload. Rejecting twice is harmless;
    /// rejecting after `accept` is a [`Conflict`](ErrorCode::Conflict) error.
    fn reject(&self, w: &NetDataWriter) -> BasisResult<()>;
}

/// Why a send was refused before it reached the wire. `TooBig`, `BadChannel` and `BadRange`
/// are caller errors — the C# transport threw `TooBigPacketException` / `ArgumentException`
/// for the same cases — so none of those is worth retrying with the same arguments.
/// `QueueFull` is the peer's condition, not the caller's: the message is dropped, and the
/// transport disconnects the peer if it stays over budget past the grace period.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    #[error("payload of {size} bytes exceeds the {limit} byte limit for {method:?}; the transport cannot fragment it")]
    TooBig { size: usize, limit: usize, method: DeliveryMethod },
    #[error("channel {channel} is outside 0..{max}")]
    BadChannel { channel: u8, max: u8 },
    #[error("range {offset}..{} is outside a buffer of {len} byte(s)", offset.saturating_add(*length))]
    BadRange { offset: usize, length: usize, len: usize },
    #[error("the peer's reliable send queue holds {queued} bytes against a budget of {budget}; the client is not reading")]
    QueueFull { queued: usize, budget: usize },
}

impl From<SendError> for BasisError {
    #[track_caller]
    fn from(err: SendError) -> Self {
        // A full queue clears itself as the peer reads (or the peer is dropped): transient.
        let kind = if matches!(err, SendError::QueueFull { .. }) { basis_error::FaultKind::Transient } else { basis_error::FaultKind::Permanent };
        BasisError::wrap(kind, ErrorCode::Transport, err)
    }
}

/// Hands out the small integer ids peers are known by (the C# `NetManager.GetNextPeerId`):
/// lowest free id first, so ids stay dense and fit the `ushort` the wire protocol carries.
///
/// One allocator can be shared by several transports. A server that listens on iroh and on
/// LiteNetLib at once must never give two players the same id, and every subsystem above the
/// transport keys players by it, so the mixed stack hands both managers the same allocator.
#[derive(Default)]
pub struct PeerIdAllocator {
    free_ids: Mutex<BTreeSet<i32>>,
    next_id: AtomicI32,
}

impl PeerIdAllocator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The lowest id that no live peer holds.
    pub fn allocate(&self) -> i32 {
        if let Some(id) = self.free_ids.lock().pop_first() {
            return id;
        }
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Returns `id` to the pool. Releasing an id that was never handed out, or twice, is
    /// harmless: the set dedups it and the allocator only ever reissues each id once per hold.
    pub fn release(&self, id: i32) {
        self.free_ids.lock().insert(id);
    }

    /// Forgets every id: the transport that owned them has stopped and its peers are gone.
    pub fn reset(&self) {
        self.free_ids.lock().clear();
        self.next_id.store(0, Ordering::Relaxed);
    }

    /// Ids handed out and not yet released.
    pub fn live_count(&self) -> usize {
        let next = usize::try_from(self.next_id.load(Ordering::Relaxed)).unwrap_or(0);
        next.saturating_sub(self.free_ids.lock().len())
    }
}

static NEXT_PEER_IDENTITY: AtomicU64 = AtomicU64::new(1);

/// A process-wide identity for a new connection. Every transport draws from the same counter,
/// so [`peers_equal`] can compare peers from different stacks without a collision: two live
/// connections never share an identity, whichever transport carries them.
pub fn next_peer_identity() -> u64 {
    NEXT_PEER_IDENTITY.fetch_add(1, Ordering::Relaxed)
}

/// One connected peer. `Arc<dyn NetPeer>` is the currency everything above the transport passes
/// around; equality and hashing are by transport identity (the C# `LNLNetPeer.Equals`).
pub trait NetPeer: Send + Sync {
    fn disconnect(&self);
    fn disconnect_with(&self, data: &[u8]);
    fn disconnect_force(&self);
    /// Queues `data` for delivery. A send to a peer that has already gone is silently dropped,
    /// as LiteNetLib did; a payload the transport cannot carry is a [`SendError`].
    fn send(&self, data: &[u8], channel_number: u8, delivery_method: DeliveryMethod) -> Result<(), SendError>;
    fn send_writer(&self, data: &NetDataWriter, channel_number: u8, delivery_method: DeliveryMethod) -> Result<(), SendError> {
        self.send(data.as_read_only_span(), channel_number, delivery_method)
    }
    /// Unreliable send of `data[offset..offset+length]`, optionally patching one byte first.
    /// LiteNetLib merged such sends into shared datagrams; QUIC coalesces datagram frames itself.
    fn send_unreliable_raw_merge(
        &self,
        data: &[u8],
        offset: usize,
        length: usize,
        channel_number: u8,
        patch_offset: i32,
        patch_value: u8,
    ) -> Result<(), SendError>;
    fn get_packets_count_in_queue(&self, channel: u8, delivery_method: DeliveryMethod) -> i32;
    fn id(&self) -> i32;
    fn address(&self) -> IpAddr;
    fn remote_id(&self) -> i32;
    fn round_trip_time(&self) -> i32;
    fn ping(&self) -> i32 {
        self.round_trip_time() / 2
    }
    fn time_since_last_packet(&self) -> f32;
    /// Remote clock minus local clock, in 100 ns ticks (the C# `DateTime` tick).
    fn remote_time_delta(&self) -> i64;
    /// Maximum UDP payload (no fragmentation) negotiated for this peer.
    fn mtu(&self) -> i32;
    /// The C# `object Tag`: the server stores its authenticated marker here.
    fn tag(&self) -> Option<Arc<dyn Any + Send + Sync>>;
    fn set_tag(&self, tag: Option<Arc<dyn Any + Send + Sync>>);
    /// Stable identity for equality and hashing across wrapper instances.
    fn identity(&self) -> u64;
    /// Whether the transport still holds the connection open.
    fn is_connected(&self) -> bool;
    /// Whether this peer's transport can hold a direct link to another player, so the server
    /// may offload their traffic to a peer-to-peer connection. A legacy LiteNetLib client
    /// cannot: everything to and from it stays relayed by the server, and the P2P broker
    /// declines any session that names it.
    fn direct_link_capable(&self) -> bool {
        true
    }
    fn as_any(&self) -> &dyn Any;
}

pub type NetPeerRef = Arc<dyn NetPeer>;

/// The C# `Equals`/`GetHashCode` on peers compared the underlying transport peer; two wrappers
/// of the same connection are the same peer.
pub fn peers_equal(a: &NetPeerRef, b: &NetPeerRef) -> bool {
    a.identity() == b.identity()
}

/// The transport endpoint: binds, connects, hands out peers, and raises events on its listener.
pub trait NetManager: Send + Sync {
    /// Binds and starts accepting. A bind failure is returned, classified transient when the
    /// port is merely still in use and permanent for a bad address or configuration.
    fn start(&self, ipv4_address: IpAddr, ipv6_address: IpAddr, set_port: u16) -> BasisResult<()>;
    fn start_default(&self) -> BasisResult<()> {
        self.start(IpAddr::from([0, 0, 0, 0]), IpAddr::from([0u16; 8]), 0)
    }
    fn start_port(&self, set_port: u16) -> BasisResult<()> {
        self.start(IpAddr::from([0, 0, 0, 0]), IpAddr::from([0u16; 8]), set_port)
    }
    /// Manual mode is a LiteNetLib feature; a transport that lacks it answers
    /// [`Unsupported`](ErrorCode::Unsupported), where the C# threw `NotSupportedException`.
    fn start_manual(&self, _ipv4: IpAddr, _ipv6: IpAddr, _set_port: u16) -> BasisResult<()> {
        Err(BasisError::permanent(ErrorCode::Unsupported, "This transport does not support manual mode."))
    }
    fn poll_events(&self) -> BasisResult<()> {
        Err(BasisError::permanent(ErrorCode::Unsupported, "This transport does not support manual mode."))
    }
    fn manual_update(&self, _elapsed_milliseconds: f32) -> BasisResult<()> {
        Err(BasisError::permanent(ErrorCode::Unsupported, "This transport does not support manual mode."))
    }
    fn stop(&self);
    /// Connects to `target` (an address:port for the LiteNetLib parser, an endpoint id / ticket
    /// for iroh) presenting `writer` as the connect payload. Returns the outgoing peer, whose
    /// `PeerConnectedEvent`/`PeerDisconnectedEvent` reports the outcome. Fails at once for a
    /// target that cannot be parsed or a transport that has not been started.
    fn connect(&self, target: &str, port: u16, writer: &NetDataWriter) -> BasisResult<NetPeerRef>;
    fn send_unconnected_message(&self, writer: &NetDataWriter, remote_end_point: SocketAddr) -> bool;
    fn statistics(&self) -> NetStatistics;
    fn connected_peers_count(&self) -> i32;
    /// Unreliable packets dropped because a peer's send queue was over budget. Zero on a healthy
    /// instance; a rising number is the signal that the instance is past what it can deliver.
    fn unreliable_dropped(&self) -> i64 {
        0
    }
    /// Voice packets dropped because a peer's priority send queue was over budget.
    fn priority_unreliable_dropped(&self) -> i64 {
        0
    }
    fn as_any(&self) -> &dyn Any;
}

pub type NetManagerRef = Arc<dyn NetManager>;

/// A subscription handle; unsubscribing needs it because closures have no identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SubscriptionId(u64);

static NEXT_SUBSCRIPTION: AtomicU64 = AtomicU64::new(1);

/// One C# `event`: a list of handlers invoked in subscription order.
pub struct NetEvent<F: ?Sized> {
    handlers: Mutex<Vec<(SubscriptionId, Arc<F>)>>,
}

impl<F: ?Sized> Default for NetEvent<F> {
    fn default() -> Self {
        Self { handlers: Mutex::new(Vec::new()) }
    }
}

impl<F: ?Sized> NetEvent<F> {
    pub fn subscribe(&self, handler: Arc<F>) -> SubscriptionId {
        let id = SubscriptionId(NEXT_SUBSCRIPTION.fetch_add(1, Ordering::Relaxed));
        self.handlers.lock().push((id, handler));
        id
    }

    pub fn unsubscribe(&self, id: SubscriptionId) {
        self.handlers.lock().retain(|(h, _)| *h != id);
    }

    pub fn clear(&self) {
        self.handlers.lock().clear();
    }

    pub fn is_empty(&self) -> bool {
        self.handlers.lock().is_empty()
    }

    /// Snapshot of the handlers, so a handler may (un)subscribe while being invoked.
    pub fn snapshot(&self) -> Vec<Arc<F>> {
        self.handlers.lock().iter().map(|(_, h)| h.clone()).collect()
    }
}

pub type OnConnectionRequest = dyn Fn(Arc<dyn ConnectionRequest>) + Send + Sync;
pub type OnPeerDisconnected = dyn Fn(NetPeerRef, DisconnectInfo) + Send + Sync;
pub type OnNetworkReceive = dyn Fn(NetPeerRef, NetPacketReader, u8, DeliveryMethod) + Send + Sync;
pub type OnNetworkError = dyn Fn(SocketAddr, i32) + Send + Sync;
pub type OnPeerConnected = dyn Fn(NetPeerRef) + Send + Sync;
pub type OnNetworkReceiveUnconnected = dyn Fn(SocketAddr, NetPacketReader) + Send + Sync;

/// The listener a transport raises its events on — the C# `EventBasedNetListener`, with each
/// C# `event` a [`NetEvent`] field so subscribers read `listener.network_receive_event.subscribe(..)`.
#[derive(Default)]
pub struct EventBasedNetListener {
    pub connection_request_event: NetEvent<OnConnectionRequest>,
    pub peer_disconnected_event: NetEvent<OnPeerDisconnected>,
    pub network_receive_event: NetEvent<OnNetworkReceive>,
    pub network_error_event: NetEvent<OnNetworkError>,
    pub peer_connected_event: NetEvent<OnPeerConnected>,
    pub network_receive_unconnected_event: NetEvent<OnNetworkReceiveUnconnected>,
}

/// Runs one handler, containing a panic in it. A handler that unwinds would otherwise take
/// the transport task that raised the event down with it — the peer's reader would stop and
/// the peer would hang — so the panic is logged as the bug it is and the transport carries on.
fn invoke_handler(event: &'static str, handler: impl FnOnce()) {
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(handler)) {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_string());
        BNL::log_error(format!(
            "[Transport] a {event} handler panicked: {message}. The transport keeps running; this is a bug in the handler."
        ));
    }
}

impl EventBasedNetListener {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn raise_connection_request(&self, request: Arc<dyn ConnectionRequest>) {
        for h in self.connection_request_event.snapshot() {
            invoke_handler("ConnectionRequest", || h(request.clone()));
        }
    }

    pub fn raise_peer_disconnected(&self, peer: NetPeerRef, disconnect_info: DisconnectInfo) {
        for h in self.peer_disconnected_event.snapshot() {
            invoke_handler("PeerDisconnected", || h(peer.clone(), disconnect_info.clone()));
        }
    }

    pub fn raise_network_receive(&self, peer: NetPeerRef, reader: NetPacketReader, channel: u8, delivery_method: DeliveryMethod) {
        for h in self.network_receive_event.snapshot() {
            invoke_handler("NetworkReceive", || h(peer.clone(), reader.clone(), channel, delivery_method));
        }
    }

    pub fn raise_peer_connected(&self, peer: NetPeerRef) {
        for h in self.peer_connected_event.snapshot() {
            invoke_handler("PeerConnected", || h(peer.clone()));
        }
    }

    pub fn raise_network_error(&self, end_point: SocketAddr, socket_error: i32) {
        for h in self.network_error_event.snapshot() {
            invoke_handler("NetworkError", || h(end_point, socket_error));
        }
    }

    pub fn raise_network_receive_unconnected(&self, remote_end_point: SocketAddr, reader: NetPacketReader) {
        for h in self.network_receive_unconnected_event.snapshot() {
            invoke_handler("NetworkReceiveUnconnected", || h(remote_end_point, reader.clone()));
        }
    }
}
