//! Port of `LiteNetLib/NetPeer.cs`: one connection's state machine — handshake, ping/RTT, MTU
//! discovery, the reliable/sequenced channels, fragmentation and the merged-datagram sender.
//!
//! The C# class ran under a handful of locks (the channels, the fragments, the shutdown state)
//! with the logic thread owning the timers and the merge buffer. The same split is kept here:
//! `logic` is only ever taken by [`update`](LnlPeer::update) (the logic thread), the channels
//! and fragments have their own locks, and nothing raises a listener event while holding any of
//! them — a handler is free to send on the very channel that delivered to it.

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

use parking_lot::{Mutex, RwLock};

use crate::transport::basis_network_shell::{DeliveryMethod, DisconnectReason, NetDebug, NetLogLevel, SendError};

use super::compact_merge::CompactMerge;
use super::internal_packets::{NetConnectAcceptPacket, NetConnectRequestPacket};
use super::net_constants::NetConstants;
use super::net_manager::ManagerInner;
use super::net_packet::{NetPacket, PacketProperty};
use super::net_utils::{TICKS_PER_MILLISECOND, relative_sequence_number, socket_address_bytes, utc_now_ticks};
use super::reliable_channel::ReliableChannel;
use super::sequenced_channel::SequencedChannel;

/// Peer connection state (the C# `ConnectionState` flags).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum ConnectionState {
    Outgoing = 1 << 1,
    Connected = 1 << 2,
    ShutdownRequested = 1 << 3,
    Disconnected = 1 << 4,
    EndPointChange = 1 << 5,
}

impl ConnectionState {
    fn from_byte(b: u8) -> Self {
        match b {
            x if x == Self::Outgoing as u8 => Self::Outgoing,
            x if x == Self::Connected as u8 => Self::Connected,
            x if x == Self::ShutdownRequested as u8 => Self::ShutdownRequested,
            x if x == Self::EndPointChange as u8 => Self::EndPointChange,
            _ => Self::Disconnected,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ConnectRequestResult {
    None,
    /// When peer connecting.
    P2PLose,
    /// When peer was connected.
    Reconnection,
    /// When peer was disconnected.
    NewConnection,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum DisconnectResult {
    None,
    Reject,
    Disconnect,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ShutdownResult {
    None,
    Success,
    WasConnected,
}

/// What `update` decided while it held the logic lock, acted on after releasing it.
enum UpdateAction {
    Nothing,
    Timeout,
    ConnectFailed,
}

pub(super) enum Channel {
    Reliable(ReliableChannel),
    Sequenced(SequencedChannel),
}

impl Channel {
    fn create(idx: usize, connection_number: u8) -> Self {
        let id = idx as u8;
        match DeliveryMethod::from_byte((idx % NetConstants::CHANNEL_TYPE_COUNT) as u8) {
            Some(DeliveryMethod::ReliableUnordered) => Self::Reliable(ReliableChannel::new(false, id, connection_number)),
            Some(DeliveryMethod::Sequenced) => Self::Sequenced(SequencedChannel::new(false, id, connection_number)),
            Some(DeliveryMethod::ReliableSequenced) => Self::Sequenced(SequencedChannel::new(true, id, connection_number)),
            _ => Self::Reliable(ReliableChannel::new(true, id, connection_number)),
        }
    }

    fn add_to_queue(&mut self, packet: NetPacket) {
        match self {
            Self::Reliable(c) => c.add_to_queue(packet),
            Self::Sequenced(c) => c.add_to_queue(packet),
        }
    }

    fn packets_in_queue(&self) -> usize {
        match self {
            Self::Reliable(c) => c.packets_in_queue(),
            Self::Sequenced(c) => c.packets_in_queue(),
        }
    }

    fn send_next_packets(&mut self, now: i64, resend_delay_ms: f64, send: &mut dyn FnMut(&NetPacket)) -> bool {
        match self {
            Self::Reliable(c) => c.send_next_packets(now, resend_delay_ms, send),
            Self::Sequenced(c) => c.send_next_packets(now, resend_delay_ms, send),
        }
    }

    fn process_packet(&mut self, packet: NetPacket, deliver: &mut Vec<(DeliveryMethod, NetPacket)>) -> super::reliable_channel::ChannelOutcome {
        match self {
            Self::Reliable(c) => c.process_packet(packet, deliver),
            Self::Sequenced(c) => c.process_packet(packet, deliver),
        }
    }
}

struct RttState {
    rtt: i32,
    avg_rtt: i32,
    rtt_count: i32,
    resend_delay: f64,
    ping_timer: Option<Instant>,
    /// Sequence of the last ping sent; a pong answers it.
    ping_sequence: u16,
    /// Sequence of the last pong sent, so an older ping is not answered twice.
    pong_sequence: u16,
}

struct MtuState {
    mtu_idx: usize,
    finish_mtu: bool,
    check_timer: f32,
    check_attempts: i32,
}

struct IncomingFragments {
    fragments: HashMap<u16, NetPacket>,
    total_fragments: u16,
    channel_id: u8,
    received_count: usize,
    total_size: usize,
    /// Peer clock (ms) when a fragment last arrived for this set.
    last_touched_ms: f64,
}

struct FragmentState {
    holded: HashMap<u16, IncomingFragments>,
    peer_clock_ms: f64,
    next_sweep_ms: f64,
}

/// Everything only the logic thread touches.
struct LogicState {
    merge_data: Vec<u8>,
    merge_pos: usize,
    merge_count: usize,
    /// Framing of the entries currently held in `merge_data`.
    merge_compact: bool,
    /// Time the current partial merge buffer has been waiting.
    merge_held_ms: f32,
    ping_send_timer: f32,
    rtt_reset_timer: f32,
    connect_attempts: i32,
    connect_timer: f32,
    shutdown_timer: f32,
    shutdown_packet: Option<NetPacket>,
    connect_request_packet: Option<NetPacket>,
    connect_accept_packet: Option<NetPacket>,
}

#[derive(Default)]
pub(super) struct PeerStatistics {
    pub packets_sent: AtomicI64,
    pub bytes_sent: AtomicI64,
    pub packets_received: AtomicI64,
    pub bytes_received: AtomicI64,
    pub packet_loss: AtomicI64,
}

/// The peer (the C# `NetPeer`). Held in an `Arc` by the manager's tables; the public
/// [`LnlNetPeer`](super::LnlNetPeer) is a thin handle over it.
pub struct LnlPeer {
    pub(super) id: i32,
    pub(super) identity: u64,
    manager: Weak<ManagerInner>,
    remote: RwLock<SocketAddr>,
    cached_socket_addr: RwLock<Vec<u8>>,
    remote_id: AtomicI32,
    state: AtomicU8,
    connect_time: AtomicI64,
    connect_num: AtomicU8,
    time_since_last_packet: AtomicU32,
    remote_delta: AtomicI64,
    rtt: Mutex<RttState>,
    mtu: Mutex<MtuState>,
    mtu_value: AtomicUsize,
    channels: Vec<Mutex<Option<Channel>>>,
    channel_send_queue: Mutex<VecDeque<usize>>,
    channel_queued: Vec<AtomicBool>,
    unreliable: Mutex<VecDeque<NetPacket>>,
    unreliable_count: AtomicI32,
    priority_unreliable: Mutex<VecDeque<NetPacket>>,
    priority_unreliable_count: AtomicI32,
    fragments: Mutex<FragmentState>,
    fragment_id: AtomicU32,
    logic: Mutex<LogicState>,
    shutdown_lock: Mutex<()>,
    tag: RwLock<Option<Arc<dyn Any + Send + Sync>>>,
    pub(super) statistics: PeerStatistics,
}

const MTU_CHECK_DELAY: f32 = 1000.0;
const MAX_MTU_CHECK_ATTEMPTS: i32 = 4;
const SHUTDOWN_DELAY: f32 = 300.0;
/// Reassembly sets nothing has touched for this long belong to a sender that stopped mid-message.
const FRAGMENT_STALE_MS: f64 = 30_000.0;
const FRAGMENT_SWEEP_INTERVAL_MS: f64 = 5_000.0;

impl LnlPeer {
    fn base(manager: &Arc<ManagerInner>, remote: SocketAddr, id: i32) -> Self {
        let channels_count = usize::from(manager.settings().channels_count) * NetConstants::CHANNEL_TYPE_COUNT;
        let mut merge_data = vec![0u8; NetConstants::HEADER_SIZE + NetConstants::MAX_PACKET_SIZE];
        merge_data[0] = PacketProperty::Merged as u8;
        let peer = Self {
            id,
            identity: crate::transport::basis_network_shell::next_peer_identity(),
            manager: Arc::downgrade(manager),
            remote: RwLock::new(remote),
            cached_socket_addr: RwLock::new(socket_address_bytes(remote)),
            remote_id: AtomicI32::new(0),
            state: AtomicU8::new(ConnectionState::Connected as u8),
            connect_time: AtomicI64::new(0),
            connect_num: AtomicU8::new(0),
            time_since_last_packet: AtomicU32::new(0f32.to_bits()),
            remote_delta: AtomicI64::new(0),
            rtt: Mutex::new(RttState { rtt: 0, avg_rtt: 0, rtt_count: 0, resend_delay: 27.0, ping_timer: None, ping_sequence: 1, pong_sequence: 0 }),
            mtu: Mutex::new(MtuState { mtu_idx: 0, finish_mtu: false, check_timer: 0.0, check_attempts: 0 }),
            mtu_value: AtomicUsize::new(NetConstants::INITIAL_MTU),
            channels: (0..channels_count).map(|_| Mutex::new(None)).collect(),
            channel_send_queue: Mutex::new(VecDeque::new()),
            channel_queued: (0..channels_count).map(|_| AtomicBool::new(false)).collect(),
            unreliable: Mutex::new(VecDeque::new()),
            unreliable_count: AtomicI32::new(0),
            priority_unreliable: Mutex::new(VecDeque::new()),
            priority_unreliable_count: AtomicI32::new(0),
            fragments: Mutex::new(FragmentState { holded: HashMap::new(), peer_clock_ms: 0.0, next_sweep_ms: FRAGMENT_SWEEP_INTERVAL_MS }),
            fragment_id: AtomicU32::new(0),
            logic: Mutex::new(LogicState {
                merge_data,
                merge_pos: 0,
                merge_count: 0,
                merge_compact: false,
                merge_held_ms: 0.0,
                ping_send_timer: 0.0,
                rtt_reset_timer: 0.0,
                connect_attempts: 0,
                connect_timer: 0.0,
                shutdown_timer: 0.0,
                shutdown_packet: None,
                connect_request_packet: None,
                connect_accept_packet: None,
            }),
            shutdown_lock: Mutex::new(()),
            tag: RwLock::new(None),
            statistics: PeerStatistics::default(),
        };
        peer.reset_mtu();
        peer
    }

    /// Incoming connection constructor: a peer that exists to be rejected, or an address that
    /// is not yet a connection.
    pub(super) fn new_incoming(manager: &Arc<ManagerInner>, remote: SocketAddr, id: i32) -> Arc<Self> {
        Arc::new(Self::base(manager, remote, id))
    }

    /// "Connect to" constructor: sends the connect request and keeps resending it from `update`.
    pub(super) fn new_outgoing(manager: &Arc<ManagerInner>, remote: SocketAddr, id: i32, connect_num: u8, connect_data: &[u8]) -> Arc<Self> {
        let peer = Self::base(manager, remote, id);
        let connect_time = utc_now_ticks();
        peer.connect_time.store(connect_time, Ordering::SeqCst);
        peer.state.store(ConnectionState::Outgoing as u8, Ordering::SeqCst);
        peer.connect_num.store(connect_num, Ordering::SeqCst);
        let mut request = NetConnectRequestPacket::make(connect_data, &socket_address_bytes(remote), connect_time, id);
        request.set_connection_number(connect_num);
        manager.send_raw(request.raw(), remote);
        peer.logic.lock().connect_request_packet = Some(request);
        Arc::new(peer)
    }

    /// "Accept" incoming constructor: the connection is up from this side's point of view as
    /// soon as the accept packet is on its way.
    pub(super) fn new_accepted(manager: &Arc<ManagerInner>, request: &NetConnectRequestPacket, remote: SocketAddr, id: i32) -> Arc<Self> {
        let peer = Self::base(manager, remote, id);
        peer.connect_time.store(request.connection_time, Ordering::SeqCst);
        peer.connect_num.store(request.connection_number, Ordering::SeqCst);
        peer.remote_id.store(request.peer_id, Ordering::SeqCst);
        let accept = NetConnectAcceptPacket::make(request.connection_time, request.connection_number, id);
        peer.state.store(ConnectionState::Connected as u8, Ordering::SeqCst);
        manager.send_raw(accept.raw(), remote);
        peer.logic.lock().connect_accept_packet = Some(accept);
        Arc::new(peer)
    }

    /// Refuses a connection: the shutdown packet carrying `data` is what the other side reads
    /// as its reject reason, and it is resent until acknowledged.
    pub(super) fn reject(&self, request: &NetConnectRequestPacket, data: &[u8]) {
        self.connect_time.store(request.connection_time, Ordering::SeqCst);
        self.connect_num.store(request.connection_number, Ordering::SeqCst);
        self.shutdown(data, false);
    }

    // ── Plain accessors ───────────────────────────────────────────────────

    pub(super) fn state(&self) -> ConnectionState {
        ConnectionState::from_byte(self.state.load(Ordering::SeqCst))
    }

    fn set_state(&self, state: ConnectionState) {
        self.state.store(state as u8, Ordering::SeqCst);
    }

    pub(super) fn remote(&self) -> SocketAddr {
        *self.remote.read()
    }

    pub(super) fn connect_time(&self) -> i64 {
        self.connect_time.load(Ordering::SeqCst)
    }

    pub(super) fn connection_num(&self) -> u8 {
        self.connect_num.load(Ordering::SeqCst)
    }

    pub(super) fn remote_id(&self) -> i32 {
        self.remote_id.load(Ordering::Relaxed)
    }

    pub(super) fn round_trip_time(&self) -> i32 {
        self.rtt.lock().avg_rtt
    }

    pub(super) fn time_since_last_packet(&self) -> f32 {
        f32::from_bits(self.time_since_last_packet.load(Ordering::Relaxed))
    }

    fn reset_time_since_last_packet(&self) {
        self.time_since_last_packet.store(0f32.to_bits(), Ordering::Relaxed);
    }

    pub(super) fn remote_time_delta(&self) -> i64 {
        self.remote_delta.load(Ordering::Relaxed)
    }

    pub(super) fn mtu(&self) -> usize {
        self.mtu_value.load(Ordering::Relaxed)
    }

    pub(super) fn tag(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.tag.read().clone()
    }

    pub(super) fn set_tag(&self, tag: Option<Arc<dyn Any + Send + Sync>>) {
        *self.tag.write() = tag;
    }

    fn manager(&self) -> Option<Arc<ManagerInner>> {
        self.manager.upgrade()
    }

    pub(super) fn manager_upgrade(&self) -> Option<Arc<ManagerInner>> {
        self.manager.upgrade()
    }

    pub(super) fn manager_settings_compact(&self) -> bool {
        self.manager().is_some_and(|m| m.settings().compact_merge_enabled)
    }

    fn send_raw(&self, data: &[u8]) -> usize {
        match self.manager() {
            Some(manager) => manager.send_raw(data, self.remote()),
            None => 0,
        }
    }

    /// Sends a packet built for this connection, stamping its connection number first.
    fn send_raw_packet(&self, packet: &mut NetPacket) -> usize {
        packet.set_connection_number(self.connection_num());
        self.send_raw(packet.raw())
    }

    // ── Address change ────────────────────────────────────────────────────

    pub(super) fn initiate_end_point_change(&self) {
        self.reset_mtu();
        self.set_state(ConnectionState::EndPointChange);
    }

    pub(super) fn finish_end_point_change(&self, new_end_point: SocketAddr) {
        if self.state() != ConnectionState::EndPointChange {
            return;
        }
        self.set_state(ConnectionState::Connected);
        *self.remote.write() = new_end_point;
        *self.cached_socket_addr.write() = socket_address_bytes(new_end_point);
    }

    // ── MTU ──────────────────────────────────────────────────────────────

    pub(super) fn reset_mtu(&self) {
        let Some(manager) = self.manager() else { return };
        let settings = manager.settings();
        let mut mtu = self.mtu.lock();
        // finish if discovery disabled
        mtu.finish_mtu = !settings.mtu_discovery;
        if settings.mtu_override > 0 {
            self.mtu_value.store(settings.mtu_override, Ordering::Relaxed);
            mtu.finish_mtu = true;
        } else {
            mtu.mtu_idx = 0;
            self.mtu_value.store(NetConstants::POSSIBLE_MTU[0], Ordering::Relaxed);
        }
    }

    fn process_mtu_packet(&self, mut packet: NetPacket) {
        // header + int
        if packet.size() < NetConstants::POSSIBLE_MTU[0] {
            return;
        }
        // first stage check (mtu check and mtu ok)
        let received_mtu = packet.read_i32(1);
        let end_mtu_check = packet.read_i32(packet.size() - 4);
        let size = i32::try_from(packet.size()).unwrap_or(i32::MAX);
        if received_mtu != size || received_mtu != end_mtu_check || received_mtu > NetConstants::MAX_PACKET_SIZE as i32 {
            NetDebug::write(NetLogLevel::Error, &format!("[MTU] Broken packet. RMTU {received_mtu}, EMTU {end_mtu_check}, PSIZE {size}"));
            return;
        }
        if packet.property() == Some(PacketProperty::MtuCheck) {
            self.mtu.lock().check_attempts = 0;
            packet.set_property(PacketProperty::MtuOk);
            self.send_raw_packet(&mut packet);
            return;
        }
        // MtuOk
        let received = usize::try_from(received_mtu).unwrap_or(0);
        let mut mtu = self.mtu.lock();
        if received > self.mtu() && !mtu.finish_mtu {
            // invalid packet
            if mtu.mtu_idx + 1 < NetConstants::POSSIBLE_MTU.len() && received == NetConstants::POSSIBLE_MTU[mtu.mtu_idx + 1] {
                mtu.mtu_idx += 1;
                self.mtu_value.store(NetConstants::POSSIBLE_MTU[mtu.mtu_idx], Ordering::Relaxed);
                // if maxed - finish.
                if mtu.mtu_idx == NetConstants::POSSIBLE_MTU.len() - 1 {
                    mtu.finish_mtu = true;
                }
            }
        }
    }

    fn update_mtu_logic(&self, delta_time: f32) {
        let mut mtu = self.mtu.lock();
        if mtu.finish_mtu {
            return;
        }
        mtu.check_timer += delta_time;
        if mtu.check_timer < MTU_CHECK_DELAY {
            return;
        }
        mtu.check_timer = 0.0;
        mtu.check_attempts += 1;
        if mtu.check_attempts >= MAX_MTU_CHECK_ATTEMPTS {
            mtu.finish_mtu = true;
            return;
        }
        if mtu.mtu_idx + 1 >= NetConstants::POSSIBLE_MTU.len() {
            return;
        }
        // Send increased packet
        let new_mtu = NetConstants::POSSIBLE_MTU[mtu.mtu_idx + 1];
        let mut probe = NetPacket::with_size(new_mtu);
        probe.set_property(PacketProperty::MtuCheck);
        let new_mtu_i32 = new_mtu as i32;
        probe.write_i32(1, new_mtu_i32); // place into start
        probe.write_i32(new_mtu - 4, new_mtu_i32); // and end of packet
        // Must check result for MTU fix
        if self.send_raw_packet(&mut probe) == 0 {
            mtu.finish_mtu = true;
        }
    }

    // ── Connection handshake ──────────────────────────────────────────────

    pub(super) fn process_connect_accept(&self, packet: &NetConnectAcceptPacket) -> bool {
        if self.state() != ConnectionState::Outgoing {
            return false;
        }
        // check connection id
        if packet.connection_time != self.connect_time() {
            return false;
        }
        // check connect num
        self.connect_num.store(packet.connection_number, Ordering::SeqCst);
        self.remote_id.store(packet.peer_id, Ordering::SeqCst);
        self.reset_time_since_last_packet();
        self.set_state(ConnectionState::Connected);
        true
    }

    pub(super) fn process_connect_request(&self, request: &NetConnectRequestPacket) -> ConnectRequestResult {
        match self.state() {
            // P2P case
            ConnectionState::Outgoing => {
                // fast check
                if request.connection_time < self.connect_time() {
                    return ConnectRequestResult::P2PLose;
                }
                // slow rare case check
                if request.connection_time == self.connect_time() {
                    let local = self.cached_socket_addr.read();
                    for i in (0..local.len()).rev() {
                        let rb = local[i];
                        let lb = request.target_address.get(i).copied().unwrap_or(0);
                        if rb == lb {
                            continue;
                        }
                        if rb < lb {
                            return ConnectRequestResult::P2PLose;
                        }
                    }
                }
            }
            ConnectionState::Connected => {
                // Old connect request
                if request.connection_time == self.connect_time() {
                    // just reply accept
                    let accept = self.logic.lock().connect_accept_packet.clone();
                    if let Some(accept) = accept {
                        self.send_raw(accept.raw());
                    }
                } else if request.connection_time > self.connect_time() {
                    // New connect request
                    return ConnectRequestResult::Reconnection;
                }
            }
            ConnectionState::Disconnected | ConnectionState::ShutdownRequested => {
                if request.connection_time >= self.connect_time() {
                    return ConnectRequestResult::NewConnection;
                }
            }
            ConnectionState::EndPointChange => {}
        }
        ConnectRequestResult::None
    }

    pub(super) fn process_disconnect(&self, packet: &NetPacket) -> DisconnectResult {
        let state = self.state();
        if (state == ConnectionState::Connected || state == ConnectionState::Outgoing)
            && packet.size() >= 9
            && packet.read_i64(1) == self.connect_time()
            && packet.connection_number() == self.connection_num()
        {
            return if state == ConnectionState::Connected { DisconnectResult::Disconnect } else { DisconnectResult::Reject };
        }
        DisconnectResult::None
    }

    pub(super) fn shutdown(&self, data: &[u8], force: bool) -> ShutdownResult {
        let _guard = self.shutdown_lock.lock();
        let state = self.state();
        // trying to shutdown already disconnected
        if state == ConnectionState::Disconnected || state == ConnectionState::ShutdownRequested {
            return ShutdownResult::None;
        }
        let result = if state == ConnectionState::Connected { ShutdownResult::WasConnected } else { ShutdownResult::Success };

        // don't send anything
        if force {
            self.set_state(ConnectionState::Disconnected);
            return result;
        }

        // reset time for reconnect protection
        self.reset_time_since_last_packet();

        // send shutdown packet
        let mut shutdown_packet = NetPacket::with_property(PacketProperty::Disconnect, data.len());
        shutdown_packet.set_connection_number(self.connection_num());
        shutdown_packet.write_i64(1, self.connect_time());
        if shutdown_packet.size() >= self.mtu() {
            // Drop additional data
            NetDebug::write(NetLogLevel::Error, "[Peer] Disconnect additional data size more than MTU - 8!");
            shutdown_packet.truncate(PacketProperty::Disconnect.header_size());
        } else if !data.is_empty() {
            shutdown_packet.raw_mut()[9..].copy_from_slice(data);
        }
        self.set_state(ConnectionState::ShutdownRequested);
        self.send_raw(shutdown_packet.raw());
        self.logic.lock().shutdown_packet = Some(shutdown_packet);
        result
    }

    // ── Sending ───────────────────────────────────────────────────────────

    fn channels_count(&self) -> usize {
        self.channels.len() / NetConstants::CHANNEL_TYPE_COUNT
    }

    fn with_channel<R>(&self, idx: usize, create: bool, f: impl FnOnce(&mut Channel) -> R) -> Option<R> {
        let slot = self.channels.get(idx)?;
        let mut guard = slot.lock();
        if guard.is_none() {
            if !create {
                return None;
            }
            *guard = Some(Channel::create(idx, self.connection_num()));
        }
        guard.as_mut().map(f)
    }

    fn add_to_reliable_channel_send_queue(&self, idx: usize) {
        if let Some(flag) = self.channel_queued.get(idx)
            && flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok()
        {
            self.channel_send_queue.lock().push_back(idx);
        }
    }

    fn queue_on_channel(&self, idx: usize, packet: NetPacket) {
        self.with_channel(idx, true, |c| c.add_to_queue(packet));
        self.add_to_reliable_channel_send_queue(idx);
    }

    pub(super) fn get_packets_count_in_queue(&self, channel_number: u8, delivery_method: DeliveryMethod) -> i32 {
        if delivery_method == DeliveryMethod::Unreliable {
            return 0;
        }
        let idx = usize::from(channel_number) * NetConstants::CHANNEL_TYPE_COUNT + usize::from(delivery_method as u8);
        self.with_channel(idx, false, |c| c.packets_in_queue() as i32).unwrap_or(0)
    }

    pub(super) fn send_internal(&self, data: &[u8], channel_number: u8, delivery_method: DeliveryMethod) -> Result<(), SendError> {
        if usize::from(channel_number) >= self.channels_count() {
            return Err(SendError::BadChannel { channel: channel_number, max: self.channels_count() as u8 });
        }
        if self.state() != ConnectionState::Connected {
            return Ok(()); // LiteNetLib dropped sends to a departed peer silently.
        }

        // Select channel
        let (property, channel_idx) = if delivery_method == DeliveryMethod::Unreliable {
            (PacketProperty::Unreliable, None)
        } else {
            (PacketProperty::Channeled, Some(usize::from(channel_number) * NetConstants::CHANNEL_TYPE_COUNT + usize::from(delivery_method as u8)))
        };

        // Check fragmentation
        let header_size = property.header_size();
        let mtu = self.mtu();
        let length = data.len();
        if length + header_size > mtu {
            // if cannot be fragmented
            let Some(channel_idx) = channel_idx.filter(|_| delivery_method == DeliveryMethod::ReliableOrdered || delivery_method == DeliveryMethod::ReliableUnordered) else {
                return Err(SendError::TooBig { size: length, limit: mtu.saturating_sub(header_size), method: delivery_method });
            };
            let packet_full_size = mtu - header_size;
            let packet_data_size = packet_full_size.saturating_sub(NetConstants::FRAGMENT_HEADER_SIZE).max(1);
            let total_packets = length.div_ceil(packet_data_size);
            let Ok(total_packets_u16) = u16::try_from(total_packets) else {
                return Err(SendError::TooBig { size: length, limit: packet_data_size * usize::from(u16::MAX), method: delivery_method });
            };
            let current_fragment_id = self.fragment_id.fetch_add(1, Ordering::Relaxed).wrapping_add(1) as u16;
            let connection_num = self.connection_num();
            for (part_idx, chunk) in data.chunks(packet_data_size).enumerate() {
                let mut p = NetPacket::with_size(header_size + chunk.len() + NetConstants::FRAGMENT_HEADER_SIZE);
                p.set_property(property);
                p.set_connection_number(connection_num);
                p.set_fragment_id(current_fragment_id);
                p.set_fragment_part(part_idx as u16);
                p.set_fragments_total(total_packets_u16);
                p.mark_fragmented();
                p.raw_mut()[NetConstants::FRAGMENTED_HEADER_TOTAL_SIZE..].copy_from_slice(chunk);
                self.queue_on_channel(channel_idx, p);
            }
            return Ok(());
        }

        // Else just send
        let mut packet = NetPacket::with_size(header_size + length);
        packet.set_property(property);
        packet.set_connection_number(self.connection_num());
        packet.raw_mut()[header_size..].copy_from_slice(data);
        match channel_idx {
            None => {
                packet.raw_mut()[1] = channel_number;
                self.enqueue_unreliable(packet);
            }
            Some(idx) => self.queue_on_channel(idx, packet),
        }
        Ok(())
    }

    /// Builds an unreliable packet from raw user bytes and enqueues it, optionally patching one
    /// byte (a per-receiver field such as the interval) after the copy.
    pub(super) fn send_unreliable_raw_merge(&self, data: &[u8], channel_number: u8, patch_offset: i32, patch_value: u8) -> Result<(), SendError> {
        if usize::from(channel_number) >= self.channels_count() {
            return Err(SendError::BadChannel { channel: channel_number, max: self.channels_count() as u8 });
        }
        if self.state() != ConnectionState::Connected {
            return Ok(());
        }
        let header_size = NetConstants::UNRELIABLE_HEADER_SIZE;
        let mut packet = NetPacket::with_size(header_size + data.len());
        packet.set_property(PacketProperty::Unreliable);
        packet.set_connection_number(self.connection_num());
        packet.raw_mut()[1] = channel_number;
        packet.raw_mut()[header_size..].copy_from_slice(data);
        if let Ok(patch) = usize::try_from(patch_offset)
            && patch < data.len()
        {
            packet.raw_mut()[header_size + patch] = patch_value;
        }
        self.enqueue_unreliable(packet);
        Ok(())
    }

    fn is_priority_unreliable(&self, packet: &NetPacket, manager: &ManagerInner) -> bool {
        let channel = usize::from(packet.raw().get(1).copied().unwrap_or(0));
        manager.priority_channels().get(channel).copied().unwrap_or(false)
    }

    fn enqueue_unreliable(&self, packet: NetPacket) {
        let Some(manager) = self.manager() else { return };
        if self.is_priority_unreliable(&packet, &manager) {
            self.enqueue_priority_unreliable(packet, &manager);
            return;
        }
        let limit = manager.effective_unreliable_queue_per_peer();
        let mut queue = self.unreliable.lock();
        queue.push_back(packet);
        let depth = self.unreliable_count.fetch_add(1, Ordering::AcqRel) + 1;
        if limit <= 0 || depth <= limit {
            return;
        }
        // Over budget: the producer is outrunning the send loop. Drop from the front until we
        // are back inside it. Oldest-first, because these are position updates: the newest
        // frame supersedes everything queued behind it.
        while self.unreliable_count.load(Ordering::Acquire) > limit && queue.pop_front().is_some() {
            self.unreliable_count.fetch_sub(1, Ordering::AcqRel);
            manager.note_unreliable_dropped();
        }
    }

    fn enqueue_priority_unreliable(&self, packet: NetPacket, manager: &ManagerInner) {
        let limit = manager.effective_priority_unreliable_queue_per_peer();
        let mut queue = self.priority_unreliable.lock();
        queue.push_back(packet);
        let depth = self.priority_unreliable_count.fetch_add(1, Ordering::AcqRel) + 1;
        if limit <= 0 || depth <= limit {
            return;
        }
        // Nothing supersedes a voice packet: this is "the receiver is far enough behind that
        // the head of this queue is already unplayable". Counted separately.
        while self.priority_unreliable_count.load(Ordering::Acquire) > limit && queue.pop_front().is_some() {
            self.priority_unreliable_count.fetch_sub(1, Ordering::AcqRel);
            manager.note_priority_unreliable_dropped();
        }
    }

    /// Returns everything still queued; called once when the peer leaves the manager.
    pub(super) fn recycle_queued_packets(&self) {
        self.unreliable.lock().clear();
        self.unreliable_count.store(0, Ordering::Release);
        self.priority_unreliable.lock().clear();
        self.priority_unreliable_count.store(0, Ordering::Release);
        self.fragments.lock().holded.clear();
    }

    // ── Merging ───────────────────────────────────────────────────────────

    fn record_sent(&self, manager: &ManagerInner, bytes: usize) {
        if manager.enable_statistics() {
            self.statistics.packets_sent.fetch_add(1, Ordering::Relaxed);
            self.statistics.bytes_sent.fetch_add(bytes as i64, Ordering::Relaxed);
        }
    }

    fn send_merged(&self, logic: &mut LogicState, manager: &ManagerInner) {
        if logic.merge_count == 0 {
            return;
        }
        let bytes_sent;
        if logic.merge_count > 1 {
            bytes_sent = self.send_raw(&logic.merge_data[..NetConstants::HEADER_SIZE + logic.merge_pos]);
        } else if logic.merge_compact {
            let mut entry_pos = NetConstants::HEADER_SIZE;
            let Some(entry) = CompactMerge::try_read_entry(&logic.merge_data, NetConstants::HEADER_SIZE + logic.merge_pos, &mut entry_pos) else {
                NetDebug::write(
                    NetLogLevel::Error,
                    &format!("[CompactMerged] Internal single-entry decode failure: count={}, pos={}. Dropping merge buffer.", logic.merge_count, logic.merge_pos),
                );
                logic.merge_pos = 0;
                logic.merge_count = 0;
                logic.merge_held_ms = 0.0;
                return;
            };
            if entry.is_raw_packet {
                bytes_sent = self.send_raw(&logic.merge_data[entry_pos..entry_pos + entry.payload_length]);
            } else {
                // Rewrite the two bytes immediately before the payload as a normal Unreliable
                // header/channel. This avoids moving the payload for either header length.
                let send_offset = entry_pos - NetConstants::UNRELIABLE_HEADER_SIZE;
                logic.merge_data[send_offset] = PacketProperty::Unreliable as u8 | (self.connection_num() << 5);
                logic.merge_data[send_offset + 1] = entry.channel;
                bytes_sent = self.send_raw(&logic.merge_data[send_offset..send_offset + NetConstants::UNRELIABLE_HEADER_SIZE + entry.payload_length]);
            }
        } else {
            // Legacy Merged stores the complete inner packet after a 16-bit length.
            let start = NetConstants::HEADER_SIZE + 2;
            bytes_sent = self.send_raw(&logic.merge_data[start..start + logic.merge_pos - 2]);
        }
        if bytes_sent > 0 {
            self.record_sent(manager, bytes_sent);
        }
        logic.merge_pos = 0;
        logic.merge_count = 0;
        logic.merge_held_ms = 0.0;
    }

    /// Queues one packet into the merge buffer, or sends it straight away when it will not fit.
    fn send_user_data(&self, logic: &mut LogicState, manager: &ManagerInner, packet: &NetPacket) {
        const SIZE_THRESHOLD: usize = 20;
        let connection_num = self.connection_num();
        let mut header0 = packet.raw()[0];
        header0 = (header0 & 0x9F) | (connection_num << 5);

        let is_unreliable = packet.property() == Some(PacketProperty::Unreliable);
        let is_raw_transport = matches!(packet.property(), Some(PacketProperty::Ack) | Some(PacketProperty::Channeled));
        let payload_size = if is_unreliable { packet.size().saturating_sub(NetConstants::UNRELIABLE_HEADER_SIZE) } else { 0 };
        let compact_enabled = manager.settings().compact_merge_enabled;
        let mtu = self.mtu();

        // With compact framing enabled, an out-of-range unreliable channel cannot use the
        // six-bit channel field. Unreliable traffic is unordered, so bypass the accumulator
        // without flushing an otherwise useful pending CompactMerged datagram.
        if is_unreliable && compact_enabled && !CompactMerge::can_carry_channel(packet.raw()[1]) {
            let sent = self.send_raw_stamped(packet, header0);
            self.record_sent(manager, sent);
            return;
        }

        let compact = compact_enabled && ((is_unreliable && !packet.is_fragmented()) || is_raw_transport);
        let compact_payload_size = if is_raw_transport { packet.size() } else { payload_size };
        let entry_size = if compact { CompactMerge::entry_size(compact_payload_size) } else { packet.size() + 2 };
        let merged_packet_size = NetConstants::HEADER_SIZE + entry_size;

        if merged_packet_size + SIZE_THRESHOLD >= mtu {
            // Channeled transport has ordering semantics. If an earlier entry is held in the
            // accumulator, send it before a later Channeled packet that must bypass merging.
            // Unreliable traffic is unordered and intentionally does not evict the accumulator.
            if !is_unreliable && logic.merge_count > 0 {
                self.send_merged(logic, manager);
            }
            let sent = self.send_raw_stamped(packet, header0);
            self.record_sent(manager, sent);
            return;
        }

        if logic.merge_count > 0 && compact != logic.merge_compact {
            self.send_merged(logic, manager);
        }
        if NetConstants::HEADER_SIZE + logic.merge_pos + entry_size > mtu {
            self.send_merged(logic, manager);
        }
        if logic.merge_count == 0 {
            logic.merge_compact = compact;
            logic.merge_data[0] = (if compact { PacketProperty::CompactMerged } else { PacketProperty::Merged }) as u8 | (connection_num << 5);
        }

        let entry_offset = NetConstants::HEADER_SIZE + logic.merge_pos;
        if compact {
            if is_raw_transport {
                let written = CompactMerge::write_raw_entry(&mut logic.merge_data, entry_offset, packet.raw());
                // The raw copy carries the packet's own first byte; stamp the connection number.
                let overhead = CompactMerge::entry_overhead(packet.size());
                logic.merge_data[entry_offset + overhead] = header0;
                logic.merge_pos += written;
            } else {
                logic.merge_pos += CompactMerge::write_unreliable_entry(&mut logic.merge_data, entry_offset, packet.raw()[1], &packet.raw()[NetConstants::UNRELIABLE_HEADER_SIZE..]);
            }
        } else {
            let size = u16::try_from(packet.size()).unwrap_or(u16::MAX).to_le_bytes();
            logic.merge_data[entry_offset] = size[0];
            logic.merge_data[entry_offset + 1] = size[1];
            logic.merge_data[entry_offset + 2..entry_offset + 2 + packet.size()].copy_from_slice(packet.raw());
            logic.merge_data[entry_offset + 2] = header0;
            logic.merge_pos += packet.size() + 2;
        }
        logic.merge_count += 1;
    }

    /// Sends a packet with its first byte replaced by `header0` (the connection-stamped one).
    fn send_raw_stamped(&self, packet: &NetPacket, header0: u8) -> usize {
        if packet.raw()[0] == header0 {
            return self.send_raw(packet.raw());
        }
        let mut copy = packet.raw().to_vec();
        copy[0] = header0;
        self.send_raw(&copy)
    }

    // ── Receiving ─────────────────────────────────────────────────────────

    fn update_round_trip_time(&self, rtt: &mut RttState, round_trip_time: i32) {
        rtt.rtt += round_trip_time;
        rtt.rtt_count += 1;
        rtt.avg_rtt = rtt.rtt / rtt.rtt_count.max(1);
        rtt.resend_delay = 25.0 + f64::from(rtt.avg_rtt) * 2.1;
    }

    /// Processes one inbound packet addressed to this connection (the C# `ProcessPacket`).
    pub(super) fn process_packet(self: &Arc<Self>, packet: NetPacket) {
        let state = self.state();
        // not initialized
        if state == ConnectionState::Outgoing || state == ConnectionState::Disconnected {
            return;
        }
        if packet.property() == Some(PacketProperty::ShutdownOk) {
            if state == ConnectionState::ShutdownRequested {
                self.set_state(ConnectionState::Disconnected);
            }
            return;
        }
        if packet.connection_number() != self.connection_num() {
            return; // Old packet
        }
        self.reset_time_since_last_packet();
        let Some(manager) = self.manager() else { return };

        match packet.property() {
            Some(PacketProperty::Merged) => {
                let mut pos = NetConstants::HEADER_SIZE;
                let raw = packet.raw();
                while pos < raw.len() {
                    if pos + 2 > raw.len() {
                        break;
                    }
                    let size = usize::from(u16::from_le_bytes([raw[pos], raw[pos + 1]]));
                    if size == 0 {
                        break;
                    }
                    pos += 2;
                    if raw.len() - pos < size {
                        break;
                    }
                    let merged = NetPacket::from_bytes(raw[pos..pos + size].to_vec());
                    if !merged.verify() {
                        break;
                    }
                    pos += size;
                    self.process_packet(merged);
                }
            }
            Some(PacketProperty::CompactMerged) => {
                let mut compact_pos = NetConstants::HEADER_SIZE;
                let raw = packet.raw();
                while compact_pos < raw.len() {
                    let Some(entry) = CompactMerge::try_read_entry(raw, raw.len(), &mut compact_pos) else {
                        break;
                    };
                    let payload_offset = compact_pos;
                    compact_pos += entry.payload_length;
                    if entry.is_raw_packet {
                        let inner = NetPacket::from_bytes(raw[payload_offset..payload_offset + entry.payload_length].to_vec());
                        if !inner.verify() {
                            break;
                        }
                        self.process_packet(inner);
                        continue;
                    }
                    let mut unreliable = NetPacket::with_size(NetConstants::UNRELIABLE_HEADER_SIZE + entry.payload_length);
                    unreliable.set_property(PacketProperty::Unreliable);
                    unreliable.set_connection_number(self.connection_num());
                    unreliable.raw_mut()[1] = entry.channel;
                    unreliable.raw_mut()[NetConstants::UNRELIABLE_HEADER_SIZE..].copy_from_slice(&raw[payload_offset..payload_offset + entry.payload_length]);
                    manager.create_receive_event(unreliable, DeliveryMethod::Unreliable, entry.channel, NetConstants::UNRELIABLE_HEADER_SIZE, self);
                }
            }
            // If we get ping, send pong
            Some(PacketProperty::Ping) => {
                let mut rtt = self.rtt.lock();
                if relative_sequence_number(i32::from(packet.sequence()), i32::from(rtt.pong_sequence)) > 0 {
                    let mut pong = NetPacket::with_property(PacketProperty::Pong, 0);
                    pong.write_i64(3, utc_now_ticks());
                    pong.set_sequence(packet.sequence());
                    rtt.pong_sequence = packet.sequence();
                    drop(rtt);
                    self.send_raw_packet(&mut pong);
                }
            }
            // If we get pong, calculate ping time and rtt
            Some(PacketProperty::Pong) => {
                let mut rtt = self.rtt.lock();
                if packet.sequence() == rtt.ping_sequence
                    && let Some(started) = rtt.ping_timer.take()
                {
                    let elapsed_ms = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
                    let delta = packet.read_i64(3) + (i64::from(elapsed_ms) * TICKS_PER_MILLISECOND) / 2 - utc_now_ticks();
                    self.remote_delta.store(delta, Ordering::Relaxed);
                    self.update_round_trip_time(&mut rtt, elapsed_ms);
                }
            }
            Some(PacketProperty::Ack) | Some(PacketProperty::Channeled) => {
                let idx = usize::from(packet.channel_id());
                if idx >= self.channels.len() {
                    return;
                }
                let is_ack = packet.property() == Some(PacketProperty::Ack);
                let mut deliver: Vec<(DeliveryMethod, NetPacket)> = Vec::new();
                let outcome = self.with_channel(idx, !is_ack, |c| c.process_packet(packet, &mut deliver));
                let Some(outcome) = outcome else { return };
                if outcome.request_send {
                    self.add_to_reliable_channel_send_queue(idx);
                }
                if outcome.packet_loss > 0 && manager.enable_statistics() {
                    self.statistics.packet_loss.fetch_add(i64::from(outcome.packet_loss), Ordering::Relaxed);
                    manager.add_packet_loss(i64::from(outcome.packet_loss));
                }
                let sequenced = matches!(idx % NetConstants::CHANNEL_TYPE_COUNT, 1 | 3);
                for (method, delivered) in deliver {
                    if sequenced {
                        manager.create_receive_event(delivered, method, (idx / NetConstants::CHANNEL_TYPE_COUNT) as u8, NetConstants::CHANNELED_HEADER_SIZE, self);
                    } else {
                        self.add_reliable_packet(&manager, method, delivered);
                    }
                }
            }
            // Simple packet without acks
            Some(PacketProperty::Unreliable) => {
                let channel = packet.raw()[1];
                manager.create_receive_event(packet, DeliveryMethod::Unreliable, channel, NetConstants::UNRELIABLE_HEADER_SIZE, self);
            }
            Some(PacketProperty::MtuCheck) | Some(PacketProperty::MtuOk) => self.process_mtu_packet(packet),
            other => NetDebug::write(NetLogLevel::Error, &format!("Error! Unexpected packet type: {other:?}")),
        }
    }

    /// A reliable packet the channel released: whole, or one fragment of a larger message.
    fn add_reliable_packet(self: &Arc<Self>, manager: &Arc<ManagerInner>, method: DeliveryMethod, p: NetPacket) {
        if !p.is_fragmented() {
            let channel = (usize::from(p.channel_id()) / NetConstants::CHANNEL_TYPE_COUNT) as u8;
            manager.create_receive_event(p, method, channel, NetConstants::CHANNELED_HEADER_SIZE, self);
            return;
        }
        let total = p.fragments_total();
        if total == 0 || total > manager.settings().max_fragments_count {
            NetDebug::write(NetLogLevel::Error, &format!("Invalid FragmentsTotal: {total}"));
            return;
        }
        if p.fragment_part() >= total {
            NetDebug::write(NetLogLevel::Error, &format!("FragmentPart {} >= FragmentsTotal {total}", p.fragment_part()));
            return;
        }
        let packet_frag_id = p.fragment_id();
        let packet_channel_id = p.channel_id();
        let complete = {
            let mut fragments = self.fragments.lock();
            let clock = fragments.peer_clock_ms;
            let cap = NetConstants::MAX_FRAGMENTS_IN_WINDOW * usize::from(manager.settings().channels_count) * NetConstants::FRAGMENTED_CHANNELS_COUNT;
            let holded_len = fragments.holded.len();
            let incoming = match fragments.holded.entry(packet_frag_id) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    if holded_len >= cap {
                        return;
                    }
                    e.insert(IncomingFragments {
                        fragments: HashMap::new(),
                        total_fragments: total,
                        channel_id: packet_channel_id,
                        received_count: 0,
                        total_size: 0,
                        last_touched_ms: clock,
                    })
                }
            };
            if total != incoming.total_fragments || packet_channel_id != incoming.channel_id {
                NetDebug::write(NetLogLevel::Error, "Fragment metadata mismatch");
                return;
            }
            if incoming.fragments.contains_key(&p.fragment_part()) {
                return;
            }
            incoming.received_count += 1;
            incoming.total_size += p.size().saturating_sub(NetConstants::FRAGMENTED_HEADER_TOTAL_SIZE);
            incoming.last_touched_ms = clock;
            incoming.fragments.insert(p.fragment_part(), p);
            if incoming.received_count != usize::from(incoming.total_fragments) {
                return;
            }
            // All fragments received — take the set out of the table while under the lock.
            fragments.holded.remove(&packet_frag_id)
        };
        let Some(mut complete) = complete else { return };
        // Outside the lock: the actual data copy.
        let mut resulting = Vec::with_capacity(complete.total_size);
        for i in 0..complete.total_fragments {
            let Some(fragment) = complete.fragments.remove(&i) else {
                NetDebug::write(NetLogLevel::Error, &format!("Fragment {i} missing during reassembly"));
                return;
            };
            resulting.extend_from_slice(&fragment.raw()[NetConstants::FRAGMENTED_HEADER_TOTAL_SIZE.min(fragment.size())..]);
        }
        let channel = (usize::from(packet_channel_id) / NetConstants::CHANNEL_TYPE_COUNT) as u8;
        manager.create_receive_event(NetPacket::from_bytes(resulting), method, channel, 0, self);
    }

    fn sweep_stale_fragments(&self) {
        let mut fragments = self.fragments.lock();
        let clock = fragments.peer_clock_ms;
        fragments.holded.retain(|_, set| clock - set.last_touched_ms <= FRAGMENT_STALE_MS);
    }

    // ── Update (the logic thread) ─────────────────────────────────────────

    /// One logic pass: timeouts, the handshake retries, ping, MTU discovery, the channel sends
    /// and the unreliable drains, then the merged datagram.
    pub(super) fn update(self: &Arc<Self>, delta_time: f32) {
        let Some(manager) = self.manager() else { return };
        let settings = manager.settings();

        let previous = f32::from_bits(self.time_since_last_packet.load(Ordering::Relaxed));
        // The receive path resets this to zero; a plain store here could overwrite that reset,
        // so add with a CAS loop the way the C# did.
        let mut current = previous;
        loop {
            let updated = current + delta_time;
            match self.time_since_last_packet.compare_exchange(current.to_bits(), updated.to_bits(), Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => break,
                Err(actual) => current = f32::from_bits(actual),
            }
        }

        {
            let mut fragments = self.fragments.lock();
            fragments.peer_clock_ms += f64::from(delta_time);
            if fragments.peer_clock_ms >= fragments.next_sweep_ms {
                fragments.next_sweep_ms = fragments.peer_clock_ms + FRAGMENT_SWEEP_INTERVAL_MS;
                drop(fragments);
                self.sweep_stale_fragments();
            }
        }

        let action = {
            let mut logic = self.logic.lock();
            match self.state() {
                ConnectionState::Connected => {
                    if self.time_since_last_packet() > settings.disconnect_timeout_ms {
                        UpdateAction::Timeout
                    } else {
                        UpdateAction::Nothing
                    }
                }
                ConnectionState::ShutdownRequested => {
                    if self.time_since_last_packet() > settings.disconnect_timeout_ms {
                        self.set_state(ConnectionState::Disconnected);
                    } else {
                        logic.shutdown_timer += delta_time;
                        if logic.shutdown_timer >= SHUTDOWN_DELAY {
                            logic.shutdown_timer = 0.0;
                            if let Some(shutdown) = &logic.shutdown_packet {
                                self.send_raw(shutdown.raw());
                            }
                        }
                    }
                    return;
                }
                ConnectionState::Outgoing => {
                    logic.connect_timer += delta_time;
                    if logic.connect_timer > settings.reconnect_delay_ms {
                        logic.connect_timer = 0.0;
                        logic.connect_attempts += 1;
                        if logic.connect_attempts > settings.max_connect_attempts {
                            UpdateAction::ConnectFailed
                        } else {
                            // else send connect again
                            if let Some(request) = &logic.connect_request_packet {
                                self.send_raw(request.raw());
                            }
                            return;
                        }
                    } else {
                        return;
                    }
                }
                ConnectionState::Disconnected | ConnectionState::EndPointChange => return,
            }
        };
        match action {
            UpdateAction::Timeout => {
                manager.disconnect_peer_force(self, DisconnectReason::Timeout, 0, None);
                return;
            }
            UpdateAction::ConnectFailed => {
                manager.disconnect_peer_force(self, DisconnectReason::ConnectionFailed, 0, None);
                return;
            }
            UpdateAction::Nothing => {}
        }

        let mut logic = self.logic.lock();

        // Send ping
        logic.ping_send_timer += delta_time;
        if logic.ping_send_timer >= settings.ping_interval_ms {
            logic.ping_send_timer = 0.0;
            let mut rtt = self.rtt.lock();
            rtt.ping_sequence = rtt.ping_sequence.wrapping_add(1) % NetConstants::MAX_SEQUENCE;
            // ping timeout
            if let Some(started) = rtt.ping_timer.take() {
                let elapsed = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
                self.update_round_trip_time(&mut rtt, elapsed);
            }
            rtt.ping_timer = Some(Instant::now());
            let mut ping = NetPacket::with_property(PacketProperty::Ping, 0);
            ping.set_sequence(rtt.ping_sequence);
            drop(rtt);
            self.send_raw_packet(&mut ping);
        }

        // RTT - round trip time
        logic.rtt_reset_timer += delta_time;
        if logic.rtt_reset_timer >= settings.ping_interval_ms * 3.0 {
            logic.rtt_reset_timer = 0.0;
            let mut rtt = self.rtt.lock();
            rtt.rtt = rtt.avg_rtt;
            rtt.rtt_count = 1;
        }

        self.update_mtu_logic(delta_time);

        // Pending send
        let now = utc_now_ticks();
        let resend_delay = self.rtt.lock().resend_delay;
        let mut count = self.channel_send_queue.lock().len();
        while count > 0 {
            count -= 1;
            let Some(idx) = self.channel_send_queue.lock().pop_front() else {
                break;
            };
            let has_more = self
                .with_channel(idx, false, |c| c.send_next_packets(now, resend_delay, &mut |p| self.send_user_data(&mut logic, &manager, p)))
                .unwrap_or(false);
            if has_more {
                // still has something to send, re-add it to the send queue
                self.channel_send_queue.lock().push_back(idx);
            } else if let Some(flag) = self.channel_queued.get(idx) {
                flag.store(false, Ordering::Release);
                // A packet queued between the send and the flag reset would otherwise wait for
                // the next enqueue; re-arm rather than strand it.
                if self.with_channel(idx, false, |c| c.packets_in_queue() > 0).unwrap_or(false) {
                    self.add_to_reliable_channel_send_queue(idx);
                }
            }
        }

        // Priority first, so latency-critical traffic lands in the earliest datagrams this pass
        // emits instead of behind however much bulk state the producer queued since the last one.
        loop {
            let next = self.priority_unreliable.lock().pop_front();
            let Some(packet) = next else { break };
            self.priority_unreliable_count.fetch_sub(1, Ordering::AcqRel);
            self.send_user_data(&mut logic, &manager, &packet);
        }
        loop {
            let next = self.unreliable.lock().pop_front();
            let Some(packet) = next else { break };
            self.unreliable_count.fetch_sub(1, Ordering::AcqRel);
            self.send_user_data(&mut logic, &manager, &packet);
        }

        // Hold a partly-filled buffer briefly so consecutive passes coalesce into one datagram
        // instead of each pass emitting its own half-empty one.
        let hold = settings.merge_hold_ms;
        if hold <= 0.0 {
            self.send_merged(&mut logic, &manager);
        } else if logic.merge_count > 0 {
            logic.merge_held_ms += delta_time;
            if logic.merge_held_ms >= hold {
                self.send_merged(&mut logic, &manager);
            }
        }
    }
}
