//! Port of `LiteNetLib/NetManager.cs` (with `NetManager.Socket.cs`): the sockets, the receive
//! path, the logic thread, peer admission and the [`NetManager`] surface.
//!
//! # Threads
//!
//! Receives run as tasks on the shared transport runtime ([`IrohRuntime`]), one per socket:
//! every datagram is parsed and dispatched to its peer on the task that read it, and listener
//! events are raised right there — the `UnsyncedEvents = true` mode the C# server ran in. The
//! logic pass (acks, resends, pings, timeouts, the merged datagrams) runs on a dedicated OS
//! thread every `update_time_ms`, over a rayon pool sized by [`BasisCpuBudget::peer_update_cap`]
//! once the population is large enough to be worth splitting, exactly as `UpdateLogic` did.
//!
//! # Sockets
//!
//! One IPv4 and one IPv6 socket on the same port (never dual-stack, so a v6-less host still
//! binds), 32 MB kernel buffers, and — on Linux — `MultiSocketCount` extra `SO_REUSEPORT`
//! sockets so the kernel spreads inbound flows over several receive tasks.

use std::any::Any;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU16, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use basis_error::{BasisError, BasisResult, ErrorCode, FaultKind};
use dashmap::DashMap;
use parking_lot::{Condvar, Mutex, RwLock};
use tokio::net::UdpSocket;

use crate::BNL;
use crate::configuration::{BasisPopulationScale, BasisTransportConfigStore, Configuration, LNLTransportConfig};
use crate::io::{NetDataReader, NetDataWriter, NetPacketReader};
use crate::protocol::basis_cpu_budget::BasisCpuBudget;
use crate::protocol::BasisNetworkCommons;
use crate::transport::basis_network_shell::*;
use crate::transport::basis_network_stack_registry::{BasisNetworkStackRegistry, ServerProbeResult};
use crate::transport::connection_target::{ConnectionTarget, ConnectionTargetKeys, IConnectionTargetParser};
use crate::transport::iroh_network_impl::IrohRuntime;
use crate::transport::lnl_connection_target_parser::LNLConnectionTargetParser;

use super::connection_request::{ConnectionRequestResult, LnlConnectionRequest};
use super::internal_packets::{NetConnectAcceptPacket, NetConnectRequestPacket};
use super::net_constants::NetConstants;
use super::net_packet::{NetPacket, PacketProperty};
use super::net_peer::{ConnectRequestResult, ConnectionState, DisconnectResult, LnlPeer, ShutdownResult};

/// Everything the C# `NetManager` read from its public fields, resolved once at creation.
pub struct LnlSettings {
    pub channels_count: u8,
    pub update_time_ms: u64,
    pub ping_interval_ms: f32,
    pub disconnect_timeout_ms: f32,
    pub reconnect_delay_ms: f32,
    pub max_connect_attempts: i32,
    pub mtu_override: usize,
    pub mtu_discovery: bool,
    pub merge_hold_ms: f32,
    pub compact_merge_enabled: bool,
    pub allow_peer_address_change: bool,
    pub ipv6_enabled: bool,
    pub unconnected_messages_enabled: bool,
    pub enable_statistics: bool,
    pub max_unreliable_queue_per_peer: i32,
    pub max_priority_unreliable_queue_per_peer: i32,
    pub max_fragments_count: u16,
    pub multi_socket_count: usize,
    pub reuse_address: bool,
    pub peer_update_parallelism: usize,
    pub peers_per_update_worker: usize,
}

impl LnlSettings {
    /// The C# `LNLNetManager` constructor's mapping from the transport sidecar.
    pub fn from_config(lnl: &LNLTransportConfig, enable_statistics: bool) -> Self {
        Self {
            channels_count: BasisNetworkCommons::TOTAL_CHANNELS,
            update_time_ms: u64::try_from(BasisNetworkCommons::NETWORK_INTERVAL_POLL).unwrap_or(2).max(1),
            ping_interval_ms: lnl.ping_interval.max(1) as f32,
            disconnect_timeout_ms: lnl.disconnect_timeout.max(1) as f32,
            reconnect_delay_ms: lnl.reconnect_delay.max(1) as f32,
            max_connect_attempts: lnl.max_connect_attempts.max(1),
            mtu_override: usize::try_from(lnl.mtu_override).unwrap_or(0),
            mtu_discovery: lnl.mtu_discovery,
            merge_hold_ms: lnl.merge_hold_ms,
            compact_merge_enabled: lnl.compact_merged,
            allow_peer_address_change: lnl.allow_peer_address_change,
            ipv6_enabled: lnl.i_pv6_enabled,
            unconnected_messages_enabled: true,
            enable_statistics,
            max_unreliable_queue_per_peer: lnl.max_unreliable_queue_per_peer,
            max_priority_unreliable_queue_per_peer: lnl.max_priority_unreliable_queue_per_peer,
            max_fragments_count: u16::MAX,
            multi_socket_count: usize::try_from(lnl.multi_socket_count).unwrap_or(1).max(1),
            reuse_address: lnl.reuse_addresss,
            peer_update_parallelism: usize::try_from(lnl.peer_update_parallelism).unwrap_or(0),
            peers_per_update_worker: usize::try_from(lnl.peer_update_peers_per_worker).ok().filter(|v| *v > 0).unwrap_or(128),
        }
    }
}

/// Serial below this: `Parallel.ForEach` overhead is not worth paying for a handful of peers.
const PARALLEL_PEER_THRESHOLD: usize = 8;
const RECEIVE_BUFFER_BYTES: usize = 2048;

struct Sockets {
    v4: Vec<Arc<UdpSocket>>,
    v6: Vec<Arc<UdpSocket>>,
}

pub(super) struct ManagerInner {
    listener: Arc<EventBasedNetListener>,
    settings: LnlSettings,
    priority_channels: Vec<bool>,
    ids: Arc<PeerIdAllocator>,
    owns_ids: bool,
    sockets: RwLock<Option<Sockets>>,
    local_port: AtomicU16,
    peers_by_addr: DashMap<SocketAddr, Arc<LnlPeer>>,
    peers_by_id: DashMap<i32, Arc<LnlPeer>>,
    requests: Mutex<HashMap<SocketAddr, Arc<LnlConnectionRequest>>>,
    connected_peers_count: AtomicI32,
    running: AtomicBool,
    logic_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    logic_wake: (Mutex<bool>, Condvar),
    receive_tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    peer_pool: Mutex<Option<rayon::ThreadPool>>,
    peers_to_remove: Mutex<Vec<Arc<LnlPeer>>>,
    packets_sent: AtomicI64,
    packets_received: AtomicI64,
    bytes_sent: AtomicI64,
    bytes_received: AtomicI64,
    packet_loss: AtomicI64,
    unreliable_dropped: AtomicI64,
    priority_dropped: AtomicI64,
    effective_unreliable_queue: AtomicI32,
    effective_priority_queue: AtomicI32,
    send_failures_logged: AtomicU32,
    weak_self: RwLock<Weak<ManagerInner>>,
}

impl ManagerInner {
    pub(super) fn settings(&self) -> &LnlSettings {
        &self.settings
    }

    pub(super) fn priority_channels(&self) -> &[bool] {
        &self.priority_channels
    }

    pub(super) fn enable_statistics(&self) -> bool {
        self.settings.enable_statistics
    }

    pub(super) fn effective_unreliable_queue_per_peer(&self) -> i32 {
        self.effective_unreliable_queue.load(Ordering::Relaxed)
    }

    pub(super) fn effective_priority_unreliable_queue_per_peer(&self) -> i32 {
        self.effective_priority_queue.load(Ordering::Relaxed)
    }

    pub(super) fn note_unreliable_dropped(&self) {
        self.unreliable_dropped.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn note_priority_unreliable_dropped(&self) {
        self.priority_dropped.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn add_packet_loss(&self, count: i64) {
        self.packet_loss.fetch_add(count, Ordering::Relaxed);
    }

    /// Queue bounds follow the population, resolved on every join/leave.
    fn recompute_queue_caps(&self) {
        let peers = self.connected_peers_count.load(Ordering::Relaxed);
        self.effective_unreliable_queue
            .store(BasisPopulationScale::unreliable_queue_per_peer(self.settings.max_unreliable_queue_per_peer, peers), Ordering::Relaxed);
        self.effective_priority_queue
            .store(BasisPopulationScale::priority_queue_per_peer(self.settings.max_priority_unreliable_queue_per_peer, peers), Ordering::Relaxed);
    }

    // ── Raw sends ─────────────────────────────────────────────────────────

    fn pick_socket(&self, remote: SocketAddr) -> Option<Arc<UdpSocket>> {
        let sockets = self.sockets.read();
        let sockets = sockets.as_ref()?;
        let family = if remote.is_ipv6() { &sockets.v6 } else { &sockets.v4 };
        if family.is_empty() {
            return None;
        }
        // Any socket of the group may send (they share the port); spread flows by address so
        // a large population does not serialise on one descriptor.
        let index = if family.len() == 1 { 0 } else { (remote.port() as usize ^ hash_ip(remote.ip())) % family.len() };
        family.get(index).cloned()
    }

    /// Writes one datagram. Returns the bytes sent, or 0 when it could not go — a full socket
    /// buffer drops the datagram exactly as LiteNetLib's `NoBufferSpaceAvailable` did, and
    /// unreliable delivery is what that means.
    pub(super) fn send_raw(&self, data: &[u8], remote: SocketAddr) -> usize {
        if !self.running.load(Ordering::Relaxed) {
            return 0;
        }
        let Some(socket) = self.pick_socket(remote) else {
            return 0;
        };
        match socket.try_send_to(data, remote) {
            Ok(sent) => {
                if self.settings.enable_statistics {
                    self.packets_sent.fetch_add(1, Ordering::Relaxed);
                    self.bytes_sent.fetch_add(sent as i64, Ordering::Relaxed);
                }
                sent
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => 0,
            Err(e) => {
                // A refused destination or an unreachable host: say so a few times, then stop —
                // under load the same peer could produce thousands of these a second.
                if self.send_failures_logged.fetch_add(1, Ordering::Relaxed) < 8 {
                    NetDebug::write(NetLogLevel::Error, &format!("[NM] send to {remote} failed: {e}"));
                }
                0
            }
        }
    }

    fn send_property(&self, property: PacketProperty, remote: SocketAddr) {
        let packet = NetPacket::with_property(property, 0);
        self.send_raw(packet.raw(), remote);
    }

    // ── Events ────────────────────────────────────────────────────────────

    pub(super) fn create_receive_event(&self, packet: NetPacket, method: DeliveryMethod, channel: u8, header_size: usize, peer: &Arc<LnlPeer>) {
        let size = packet.size();
        let reader = NetPacketReader::with_offset(packet.into_bytes(), header_size, size);
        self.listener.raise_network_receive(Arc::new(super::LnlNetPeer::new(peer.clone())), reader, channel, method);
    }

    pub(super) fn disconnect_peer_force(&self, peer: &Arc<LnlPeer>, reason: DisconnectReason, socket_error: i32, event_data: Option<NetPacket>) {
        self.disconnect_peer(peer, reason, socket_error, true, &[], event_data);
    }

    pub(super) fn disconnect_peer(&self, peer: &Arc<LnlPeer>, reason: DisconnectReason, socket_error: i32, force: bool, data: &[u8], event_data: Option<NetPacket>) {
        let shutdown_result = peer.shutdown(data, force);
        if shutdown_result == ShutdownResult::None {
            return;
        }
        if shutdown_result == ShutdownResult::WasConnected {
            self.connected_peers_count.fetch_sub(1, Ordering::AcqRel);
            self.recompute_queue_caps();
        }
        let additional_data = match event_data {
            Some(packet) => {
                let (header, size) = (packet.header_size(), packet.size());
                NetPacketReader::with_offset(packet.into_bytes(), header, size)
            }
            None => NetPacketReader::new(Vec::new()),
        };
        self.listener.raise_peer_disconnected(
            Arc::new(super::LnlNetPeer::new(peer.clone())),
            DisconnectInfo { reason, socket_error_code: socket_error, additional_data },
        );
    }

    fn raise_peer_connected(&self, peer: &Arc<LnlPeer>) {
        self.connected_peers_count.fetch_add(1, Ordering::AcqRel);
        self.recompute_queue_caps();
        self.listener.raise_peer_connected(Arc::new(super::LnlNetPeer::new(peer.clone())));
    }

    // ── Peer table ────────────────────────────────────────────────────────

    fn add_peer(&self, peer: Arc<LnlPeer>) {
        self.peers_by_addr.insert(peer.remote(), peer.clone());
        self.peers_by_id.insert(peer.id, peer);
    }

    fn remove_peer(&self, peer: &Arc<LnlPeer>) {
        self.peers_by_addr.remove_if(&peer.remote(), |_, held| Arc::ptr_eq(held, peer));
        if self.peers_by_id.remove_if(&peer.id, |_, held| Arc::ptr_eq(held, peer)).is_some() {
            self.ids.release(peer.id);
        }
        peer.recycle_queued_packets();
    }

    fn try_get_peer(&self, remote: SocketAddr) -> Option<Arc<LnlPeer>> {
        self.peers_by_addr.get(&remote).map(|p| p.value().clone())
    }

    // ── Receive path ──────────────────────────────────────────────────────

    /// The C# `HandleMessageReceived`: one datagram, from one address.
    fn on_message_received(self: &Arc<Self>, data: &[u8], remote: SocketAddr) {
        if data.is_empty() {
            return;
        }
        if self.settings.enable_statistics {
            self.packets_received.fetch_add(1, Ordering::Relaxed);
            self.bytes_received.fetch_add(data.len() as i64, Ordering::Relaxed);
        }
        let packet = NetPacket::from_bytes(data.to_vec());
        if !packet.verify() {
            NetDebug::write(NetLogLevel::Error, "[NM] DataReceived: bad!");
            return;
        }
        match packet.property() {
            // special case connect request
            Some(PacketProperty::ConnectRequest) => {
                if NetConnectRequestPacket::get_protocol_id(&packet) != NetConstants::PROTOCOL_ID {
                    self.send_property(PacketProperty::InvalidProtocol, remote);
                    return;
                }
            }
            // unconnected messages
            Some(PacketProperty::Broadcast) => return,
            Some(PacketProperty::UnconnectedMessage) => {
                if self.settings.unconnected_messages_enabled {
                    let size = packet.size();
                    let reader = NetPacketReader::with_offset(packet.into_bytes(), NetConstants::HEADER_SIZE, size);
                    self.listener.raise_network_receive_unconnected(remote, reader);
                }
                return;
            }
            Some(PacketProperty::NatMessage) => return, // NAT punching is not offered to legacy clients
            _ => {}
        }

        // Check normal packets
        let peer = self.try_get_peer(remote);
        if let Some(peer) = &peer
            && self.settings.enable_statistics
        {
            peer.statistics.packets_received.fetch_add(1, Ordering::Relaxed);
            peer.statistics.bytes_received.fetch_add(data.len() as i64, Ordering::Relaxed);
        }

        match packet.property() {
            Some(PacketProperty::ConnectRequest) => {
                if let Some(request) = NetConnectRequestPacket::from_data(&packet) {
                    self.process_connect_request(remote, peer, request);
                }
            }
            Some(PacketProperty::PeerNotFound) => {
                if let Some(peer) = peer {
                    // local
                    if peer.state() != ConnectionState::Connected {
                        return;
                    }
                    if packet.size() == 1 {
                        // first reply: send NetworkChanged packet
                        peer.reset_mtu();
                        let changed = NetConnectAcceptPacket::make_network_changed(peer.connect_time(), peer.connection_num(), peer.remote_id());
                        self.send_raw(changed.raw(), remote);
                    } else if packet.size() == 2 && packet.raw()[1] == 1 {
                        // second reply
                        self.disconnect_peer_force(&peer, DisconnectReason::PeerNotFound, 0, None);
                    }
                } else if packet.size() > 1 {
                    // remote: check if this is an old peer that changed address
                    let mut is_old_peer = false;
                    if self.settings.allow_peer_address_change
                        && let Some(remote_data) = NetConnectAcceptPacket::from_data(&packet)
                        && remote_data.peer_network_changed
                        && let Some(known) = self.peers_by_id.get(&remote_data.peer_id).map(|p| p.value().clone())
                        && known.connect_time() == remote_data.connection_time
                        && known.connection_num() == remote_data.connection_number
                    {
                        if known.state() == ConnectionState::Connected {
                            known.initiate_end_point_change();
                            let previous = known.remote();
                            self.peers_by_addr.remove_if(&previous, |_, held| Arc::ptr_eq(held, &known));
                            known.finish_end_point_change(remote);
                            self.peers_by_addr.insert(remote, known.clone());
                            BNL::log(format!("[NM] peer {} moved from {previous} to {remote}", known.id));
                        }
                        is_old_peer = true;
                    }
                    // else peer really not found
                    if !is_old_peer {
                        let mut second = NetPacket::with_property(PacketProperty::PeerNotFound, 1);
                        second.raw_mut()[1] = 1;
                        self.send_raw(second.raw(), remote);
                    }
                }
            }
            Some(PacketProperty::InvalidProtocol) => {
                if let Some(peer) = peer
                    && peer.state() == ConnectionState::Outgoing
                {
                    self.disconnect_peer_force(&peer, DisconnectReason::InvalidProtocol, 0, None);
                }
            }
            Some(PacketProperty::Disconnect) => {
                if let Some(peer) = peer {
                    match peer.process_disconnect(&packet) {
                        DisconnectResult::None => return,
                        DisconnectResult::Disconnect => self.disconnect_peer_force(&peer, DisconnectReason::RemoteConnectionClose, 0, Some(packet)),
                        DisconnectResult::Reject => self.disconnect_peer_force(&peer, DisconnectReason::ConnectionRejected, 0, Some(packet)),
                    }
                }
                // Send shutdown
                self.send_property(PacketProperty::ShutdownOk, remote);
            }
            Some(PacketProperty::ConnectAccept) => {
                let Some(peer) = peer else { return };
                if let Some(accept) = NetConnectAcceptPacket::from_data(&packet)
                    && peer.process_connect_accept(&accept)
                {
                    self.raise_peer_connected(&peer);
                }
            }
            _ => match peer {
                Some(peer) => peer.process_packet(packet),
                None => self.send_property(PacketProperty::PeerNotFound, remote),
            },
        }
    }

    fn process_connect_request(self: &Arc<Self>, remote: SocketAddr, peer: Option<Arc<LnlPeer>>, mut request: NetConnectRequestPacket) {
        // if we have peer
        if let Some(peer) = peer {
            let result = peer.process_connect_request(&request);
            match result {
                ConnectRequestResult::Reconnection => {
                    self.disconnect_peer_force(&peer, DisconnectReason::Reconnect, 0, None);
                    self.remove_peer(&peer);
                }
                ConnectRequestResult::NewConnection => self.remove_peer(&peer),
                ConnectRequestResult::P2PLose => {
                    self.disconnect_peer_force(&peer, DisconnectReason::PeerToPeerConnection, 0, None);
                    self.remove_peer(&peer);
                }
                ConnectRequestResult::None => return,
            }
            // Set next connection number
            if result != ConnectRequestResult::P2PLose {
                request.connection_number = (peer.connection_num() + 1) % NetConstants::MAX_CONNECTION_NUMBER;
            }
        }

        let req = {
            let mut requests = self.requests.lock();
            if let Some(existing) = requests.get(&remote) {
                existing.update_request(request);
                return;
            }
            let req = LnlConnectionRequest::new(self, remote, request);
            requests.insert(remote, req.clone());
            req
        };
        // The server's handler verifies the connect payload before deciding, which can take a
        // moment; it runs off the receive task so a slow decision never stalls every other
        // peer's datagrams. The request stays pending (and the client keeps resending) until
        // it is decided.
        let listener = self.listener.clone();
        let raise = move || listener.raise_connection_request(req);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(raise);
            }
            Err(_) => raise(),
        }
    }

    /// The C# `OnConnectionSolved`: the handler's verdict becomes a peer (accepted or, for a
    /// reject, one that exists only to deliver the refusal) and the request is retired.
    pub(super) fn on_connection_solved(self: &Arc<Self>, request: &LnlConnectionRequest, result: ConnectionRequestResult, reject_data: &[u8]) -> Option<Arc<LnlPeer>> {
        let remote = request.remote_end_point();
        let internal = request.internal_packet();
        if result == ConnectionRequestResult::RejectForce {
            if !reject_data.is_empty() {
                let mut shutdown = NetPacket::with_property(PacketProperty::Disconnect, reject_data.len());
                shutdown.set_connection_number(internal.connection_number);
                shutdown.write_i64(1, internal.connection_time);
                if shutdown.size() >= NetConstants::POSSIBLE_MTU[0] {
                    NetDebug::write(NetLogLevel::Error, "[Peer] Disconnect additional data size more than MTU!");
                    shutdown.truncate(PacketProperty::Disconnect.header_size());
                } else {
                    shutdown.raw_mut()[9..].copy_from_slice(reject_data);
                }
                self.send_raw(shutdown.raw(), remote);
            }
            self.requests.lock().remove(&remote);
            return None;
        }
        let (new_peer, raise_connected) = {
            let mut requests = self.requests.lock();
            let admitted = if let Some(existing) = self.try_get_peer(remote) {
                // already have peer
                (existing, false)
            } else if result == ConnectionRequestResult::Reject {
                let peer = LnlPeer::new_incoming(self, remote, self.ids.allocate());
                peer.reject(&internal, reject_data);
                self.add_peer(peer.clone());
                (peer, false)
            } else {
                let peer = LnlPeer::new_accepted(self, &internal, remote, self.ids.allocate());
                self.add_peer(peer.clone());
                (peer, true)
            };
            requests.remove(&remote);
            admitted
        };
        if raise_connected {
            self.raise_peer_connected(&new_peer);
        }
        Some(new_peer)
    }

    // ── Sockets and threads ───────────────────────────────────────────────

    fn bind_socket(&self, addr: SocketAddr, reuse_port: bool) -> std::io::Result<UdpSocket> {
        use socket2::{Domain, Protocol, Socket, Type};
        let domain = if addr.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
        let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
        // Best effort: Linux clamps these to rmem_max/wmem_max and reports success anyway.
        let _ = socket.set_recv_buffer_size(NetConstants::SOCKET_BUFFER_SIZE);
        let _ = socket.set_send_buffer_size(NetConstants::SOCKET_BUFFER_SIZE);
        warn_if_socket_buffers_were_clamped(&socket);
        if addr.is_ipv6() {
            socket.set_only_v6(true)?;
        }
        let want_reuse = self.settings.reuse_address || reuse_port;
        socket.set_reuse_address(want_reuse)?;
        #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
        if reuse_port {
            socket.set_reuse_port(true)?;
        }
        socket.set_nonblocking(true)?;
        socket.bind(&addr.into())?;
        let std_socket: std::net::UdpSocket = socket.into();
        UdpSocket::from_std(std_socket)
    }

    fn start(self: &Arc<Self>, ipv4: IpAddr, ipv6: IpAddr, port: u16) -> BasisResult<()> {
        if self.running.load(Ordering::SeqCst) {
            return Err(BasisError::permanent(ErrorCode::Conflict, "the LiteNetLib transport is already started"));
        }
        let handle = IrohRuntime::handle()?;
        // `UdpSocket::from_std` registers with the runtime the current thread belongs to.
        let _enter = handle.enter();

        let bind_error = |what: &str, addr: SocketAddr, e: std::io::Error| {
            let kind = basis_error::io_fault_kind(e.kind());
            BasisError::with_source(kind, ErrorCode::Transport, format!("binding the LiteNetLib {what} socket on {addr} failed"), e)
        };

        // SO_REUSEPORT multi-socket ingress is Linux-only, and it must be decided before the
        // primary binds because every member of the group has to carry the option.
        let mut extra = self.settings.multi_socket_count.saturating_sub(1);
        let reuse_port = extra > 0 && cfg!(target_os = "linux");
        if extra > 0 && !reuse_port {
            BNL::log_warning(format!("[NM] MultiSocketCount={} requested but SO_REUSEPORT is Linux-only; falling back to a single socket", self.settings.multi_socket_count));
            extra = 0;
        }

        let v4_addr = SocketAddr::new(ipv4, port);
        let primary = self.bind_socket(v4_addr, reuse_port).map_err(|e| bind_error("IPv4", v4_addr, e))?;
        let local_port = primary.local_addr().map(|a| a.port()).unwrap_or(port);
        self.local_port.store(local_port, Ordering::SeqCst);
        let mut v4 = vec![Arc::new(primary)];
        let mut v6 = Vec::new();

        // Check IPv6 support: one port for two sockets.
        if self.settings.ipv6_enabled && ipv6.is_ipv6() {
            let v6_addr = SocketAddr::new(ipv6, local_port);
            match self.bind_socket(v6_addr, reuse_port) {
                Ok(socket) => v6.push(Arc::new(socket)),
                Err(e) => BNL::log_warning(format!("[NM] IPv6 bind on {v6_addr} failed ({e}); continuing on IPv4 only")),
            }
        }

        for _ in 0..extra {
            match self.bind_socket(SocketAddr::new(ipv4, local_port), true) {
                Ok(socket) => v4.push(Arc::new(socket)),
                Err(e) => {
                    BNL::log_warning(format!("[NM] extra SO_REUSEPORT socket could not bind ({e}); running with {} socket(s)", v4.len()));
                    break;
                }
            }
            if let Some(first_v6) = v6.first().cloned()
                && let Ok(local_v6) = first_v6.local_addr()
                && let Ok(socket) = self.bind_socket(local_v6, true)
            {
                v6.push(Arc::new(socket));
            }
        }

        let all: Vec<Arc<UdpSocket>> = v4.iter().chain(v6.iter()).cloned().collect();
        *self.sockets.write() = Some(Sockets { v4, v6 });
        self.running.store(true, Ordering::SeqCst);
        self.recompute_queue_caps();

        let mut tasks = self.receive_tasks.lock();
        for socket in all {
            let me = self.clone();
            tasks.push(handle.spawn(async move { me.receive_loop(socket).await }));
        }
        drop(tasks);

        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        let cap = usize::try_from(BasisCpuBudget::peer_update_cap()).ok().filter(|c| *c > 0).unwrap_or(cores.max(4) * 3 / 4).min(cores).max(1);
        let workers = if self.settings.peer_update_parallelism > 0 { self.settings.peer_update_parallelism.min(cores) } else { cap };
        match rayon::ThreadPoolBuilder::new().num_threads(workers.max(1)).thread_name(|i| format!("basis-lnl-peers-{i}")).build() {
            Ok(pool) => *self.peer_pool.lock() = Some(pool),
            Err(e) => BNL::log_warning(format!("[NM] peer update pool could not be built ({e}); updating peers serially")),
        }

        let me = self.clone();
        let thread = std::thread::Builder::new()
            .name(format!("basis-lnl-logic({local_port})"))
            .spawn(move || me.logic_loop())
            .map_err(|e| BasisError::with_source(FaultKind::Transient, ErrorCode::Internal, "the LiteNetLib logic thread could not be started", e))?;
        *self.logic_thread.lock() = Some(thread);
        Ok(())
    }

    async fn receive_loop(self: Arc<Self>, socket: Arc<UdpSocket>) {
        let mut buffer = vec![0u8; RECEIVE_BUFFER_BYTES];
        loop {
            match socket.recv_from(&mut buffer).await {
                Ok((n, remote)) => self.on_message_received(&buffer[..n], remote),
                Err(e) => {
                    if !self.running.load(Ordering::Relaxed) {
                        return;
                    }
                    // ICMP port-unreachable surfaces here on some platforms; it is a peer that
                    // went away, not a socket that broke.
                    if !matches!(e.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::Interrupted) {
                        NetDebug::write(NetLogLevel::Error, &format!("[NM] SocketReceiveThread error: {e}"));
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
            }
        }
    }

    /// The C# `UpdateLogic`: one pass over every peer, every `update_time_ms`.
    fn logic_loop(self: Arc<Self>) {
        let update_time = Duration::from_millis(self.settings.update_time_ms);
        let mut last = Instant::now();
        let mut snapshot: Vec<Arc<LnlPeer>> = Vec::with_capacity(64);
        while self.running.load(Ordering::Relaxed) {
            let started = Instant::now();
            let elapsed = (started - last).as_secs_f32() * 1000.0;
            let elapsed = if elapsed <= 0.0 { 0.001 } else { elapsed };
            last = started;

            snapshot.clear();
            snapshot.extend(self.peers_by_id.iter().map(|e| e.value().clone()));
            let timeout = self.settings.disconnect_timeout_ms;
            let update = |peer: &Arc<LnlPeer>| {
                if peer.state() == ConnectionState::Disconnected && peer.time_since_last_packet() > timeout {
                    self.peers_to_remove.lock().push(peer.clone());
                } else {
                    peer.update(elapsed);
                }
            };
            let pool = self.peer_pool.lock();
            match pool.as_ref().filter(|_| snapshot.len() > PARALLEL_PEER_THRESHOLD) {
                Some(pool) => {
                    use rayon::prelude::*;
                    let chunk = (snapshot.len() / pool.current_num_threads().max(1)).clamp(1, self.settings.peers_per_update_worker);
                    pool.install(|| snapshot.par_chunks(chunk).for_each(|peers| peers.iter().for_each(update)));
                }
                None => snapshot.iter().for_each(update),
            }
            drop(pool);

            let to_remove = std::mem::take(&mut *self.peers_to_remove.lock());
            for peer in &to_remove {
                self.remove_peer(peer);
            }

            let pass = started.elapsed();
            if pass < update_time {
                let (lock, cv) = &self.logic_wake;
                let mut woken = lock.lock();
                if !*woken {
                    cv.wait_for(&mut woken, update_time - pass);
                }
                *woken = false;
            }
        }
    }

    fn trigger_update(&self) {
        let (lock, cv) = &self.logic_wake;
        *lock.lock() = true;
        cv.notify_one();
    }

    fn stop(self: &Arc<Self>, send_disconnect_messages: bool) {
        if !self.running.load(Ordering::SeqCst) {
            return;
        }
        // Send last disconnect — while the sockets are still open and sends still go out.
        let peers: Vec<Arc<LnlPeer>> = self.peers_by_id.iter().map(|e| e.value().clone()).collect();
        for peer in &peers {
            peer.shutdown(&[], !send_disconnect_messages);
        }
        if !self.running.swap(false, Ordering::SeqCst) {
            return; // another stop got there first
        }
        for task in self.receive_tasks.lock().drain(..) {
            task.abort();
        }
        *self.sockets.write() = None;
        self.trigger_update();
        let thread = self.logic_thread.lock().take();
        if let Some(thread) = thread
            && thread.thread().id() != std::thread::current().id()
            && thread.join().is_err()
        {
            BNL::log_error("[NM] the LiteNetLib logic thread panicked");
        }
        *self.peer_pool.lock() = None;
        for peer in &peers {
            // The application never hears from a peer whose transport is stopping; a stop is
            // the C# `Stop()` which cleared the table without raising events.
            self.peers_by_addr.remove(&peer.remote());
            self.peers_by_id.remove(&peer.id);
            peer.recycle_queued_packets();
        }
        self.requests.lock().clear();
        self.peers_to_remove.lock().clear();
        if self.owns_ids {
            self.ids.reset();
        }
        self.connected_peers_count.store(0, Ordering::SeqCst);
        self.recompute_queue_caps();
    }
}

fn hash_ip(ip: IpAddr) -> usize {
    match ip {
        IpAddr::V4(v4) => u32::from(v4) as usize,
        IpAddr::V6(v6) => v6.octets().iter().fold(0usize, |acc, b| acc.wrapping_mul(31).wrapping_add(usize::from(*b))),
    }
}

static SOCKET_BUFFER_CLAMP_WARNED: AtomicBool = AtomicBool::new(false);

/// Linux clamps SO_RCVBUF/SO_SNDBUF to rmem_max/wmem_max and setsockopt succeeds anyway; what
/// a clamp looks like from in here is the kernel discarding datagrams under load. Said once.
fn warn_if_socket_buffers_were_clamped(socket: &socket2::Socket) {
    if SOCKET_BUFFER_CLAMP_WARNED.load(Ordering::Relaxed) {
        return;
    }
    let (Ok(receive), Ok(send)) = (socket.recv_buffer_size(), socket.send_buffer_size()) else {
        return;
    };
    if receive >= NetConstants::SOCKET_BUFFER_SIZE && send >= NetConstants::SOCKET_BUFFER_SIZE {
        return;
    }
    if SOCKET_BUFFER_CLAMP_WARNED.swap(true, Ordering::Relaxed) {
        return;
    }
    NetDebug::write(
        NetLogLevel::Error,
        &format!(
            "[NM] The OS clamped the socket buffers: asked for {} MB, got {} KB receive / {} KB send. On Linux raise net.core.rmem_max and net.core.wmem_max in /etc/sysctl.d and restart; setsockopt reports success either way, so this line is the only place it shows up. Left alone, the kernel drops inbound datagrams under load.",
            NetConstants::SOCKET_BUFFER_SIZE / (1024 * 1024),
            receive / 1024,
            send / 1024
        ),
    );
}

// ────────────────────────────────────────────────────────────────────────────
//  Public surface
// ────────────────────────────────────────────────────────────────────────────

/// A LiteNetLib-protocol peer: the handle the application holds. Cheap to clone; equality is
/// by connection.
#[derive(Clone)]
pub struct LnlNetPeer {
    peer: Arc<LnlPeer>,
}

impl LnlNetPeer {
    pub(super) fn new(peer: Arc<LnlPeer>) -> Self {
        Self { peer }
    }

    pub fn connection_state(&self) -> ConnectionState {
        self.peer.state()
    }

    pub fn remote_end_point(&self) -> SocketAddr {
        self.peer.remote()
    }

    /// The compact merged framing is always on for a protocol-14 peer.
    pub fn compact_merge_active(&self) -> bool {
        self.peer.manager_settings_compact()
    }

    pub fn statistics(&self) -> NetStatistics {
        NetStatistics {
            packets_sent: self.peer.statistics.packets_sent.load(Ordering::Relaxed).max(0) as u64,
            packets_received: self.peer.statistics.packets_received.load(Ordering::Relaxed).max(0) as u64,
            bytes_sent: self.peer.statistics.bytes_sent.load(Ordering::Relaxed).max(0) as u64,
            bytes_received: self.peer.statistics.bytes_received.load(Ordering::Relaxed).max(0) as u64,
            packet_loss: self.peer.statistics.packet_loss.load(Ordering::Relaxed).max(0) as u64,
        }
    }

    fn manager(&self) -> Option<Arc<ManagerInner>> {
        self.peer.manager_upgrade()
    }
}

impl NetPeer for LnlNetPeer {
    fn disconnect(&self) {
        self.disconnect_with(&[]);
    }

    fn disconnect_with(&self, data: &[u8]) {
        if let Some(manager) = self.manager() {
            manager.disconnect_peer(&self.peer, DisconnectReason::DisconnectPeerCalled, 0, false, data, None);
        }
    }

    fn disconnect_force(&self) {
        if let Some(manager) = self.manager() {
            manager.disconnect_peer_force(&self.peer, DisconnectReason::DisconnectPeerCalled, 0, None);
        }
    }

    fn send(&self, data: &[u8], channel_number: u8, delivery_method: DeliveryMethod) -> Result<(), SendError> {
        self.peer.send_internal(data, channel_number, delivery_method)
    }

    fn send_unreliable_raw_merge(&self, data: &[u8], offset: usize, length: usize, channel_number: u8, patch_offset: i32, patch_value: u8) -> Result<(), SendError> {
        let Some(slice) = offset.checked_add(length).and_then(|end| data.get(offset..end)) else {
            return Err(SendError::BadRange { offset, length, len: data.len() });
        };
        self.peer.send_unreliable_raw_merge(slice, channel_number, patch_offset, patch_value)
    }

    fn get_packets_count_in_queue(&self, channel: u8, delivery_method: DeliveryMethod) -> i32 {
        self.peer.get_packets_count_in_queue(channel, delivery_method)
    }

    fn id(&self) -> i32 {
        self.peer.id
    }

    fn address(&self) -> IpAddr {
        self.peer.remote().ip()
    }

    fn remote_id(&self) -> i32 {
        self.peer.remote_id()
    }

    fn round_trip_time(&self) -> i32 {
        self.peer.round_trip_time()
    }

    fn time_since_last_packet(&self) -> f32 {
        self.peer.time_since_last_packet()
    }

    fn remote_time_delta(&self) -> i64 {
        self.peer.remote_time_delta()
    }

    fn mtu(&self) -> i32 {
        i32::try_from(self.peer.mtu()).unwrap_or(i32::MAX)
    }

    fn tag(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.peer.tag()
    }

    fn set_tag(&self, tag: Option<Arc<dyn Any + Send + Sync>>) {
        self.peer.set_tag(tag);
    }

    fn identity(&self) -> u64 {
        self.peer.identity
    }

    fn is_connected(&self) -> bool {
        self.peer.state() == ConnectionState::Connected
    }

    /// A legacy client has no way to hole-punch to another peer: the server relays everything.
    fn direct_link_capable(&self) -> bool {
        false
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// The LiteNetLib-protocol [`NetManager`]: what the existing C# clients connect to.
pub struct LnlNetManager {
    inner: Arc<ManagerInner>,
}

impl LnlNetManager {
    /// The stack registry's factory.
    pub fn create(listener: Arc<EventBasedNetListener>, configuration: &Configuration) -> Option<NetManagerRef> {
        let transport = BasisTransportConfigStore::get::<LNLTransportConfig>(BasisNetworkStackRegistry::LITE_NET_LIB_ID);
        Some(Arc::new(Self::new(listener, &transport, configuration.enable_statistics)))
    }

    pub fn new(listener: Arc<EventBasedNetListener>, transport: &LNLTransportConfig, enable_statistics: bool) -> Self {
        Self::build(listener, LnlSettings::from_config(transport, enable_statistics), PeerIdAllocator::new(), true)
    }

    /// A manager that draws peer ids from an allocator shared with another transport.
    pub fn with_id_allocator(listener: Arc<EventBasedNetListener>, transport: &LNLTransportConfig, enable_statistics: bool, ids: Arc<PeerIdAllocator>) -> Self {
        Self::build(listener, LnlSettings::from_config(transport, enable_statistics), ids, false)
    }

    /// A manager with explicit settings (tests that need a short timeout or a fixed MTU).
    pub fn with_settings(listener: Arc<EventBasedNetListener>, settings: LnlSettings) -> Self {
        Self::build(listener, settings, PeerIdAllocator::new(), true)
    }

    fn build(listener: Arc<EventBasedNetListener>, settings: LnlSettings, ids: Arc<PeerIdAllocator>, owns_ids: bool) -> Self {
        let inner = Arc::new(ManagerInner {
            listener,
            settings,
            priority_channels: BasisNetworkCommons::build_priority_unreliable_channel_map(),
            ids,
            owns_ids,
            sockets: RwLock::new(None),
            local_port: AtomicU16::new(0),
            peers_by_addr: DashMap::new(),
            peers_by_id: DashMap::new(),
            requests: Mutex::new(HashMap::new()),
            connected_peers_count: AtomicI32::new(0),
            running: AtomicBool::new(false),
            logic_thread: Mutex::new(None),
            logic_wake: (Mutex::new(false), Condvar::new()),
            receive_tasks: Mutex::new(Vec::new()),
            peer_pool: Mutex::new(None),
            peers_to_remove: Mutex::new(Vec::new()),
            packets_sent: AtomicI64::new(0),
            packets_received: AtomicI64::new(0),
            bytes_sent: AtomicI64::new(0),
            bytes_received: AtomicI64::new(0),
            packet_loss: AtomicI64::new(0),
            unreliable_dropped: AtomicI64::new(0),
            priority_dropped: AtomicI64::new(0),
            effective_unreliable_queue: AtomicI32::new(0),
            effective_priority_queue: AtomicI32::new(0),
            send_failures_logged: AtomicU32::new(0),
            weak_self: RwLock::new(Weak::new()),
        });
        *inner.weak_self.write() = Arc::downgrade(&inner);
        inner.recompute_queue_caps();
        Self { inner }
    }

    /// The UDP port actually bound (the OS-picked one when started on port 0).
    pub fn local_port(&self) -> u16 {
        self.inner.local_port.load(Ordering::SeqCst)
    }

    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::SeqCst)
    }

    pub fn peer(&self, id: i32) -> Option<LnlNetPeer> {
        self.inner.peers_by_id.get(&id).map(|p| LnlNetPeer::new(p.value().clone()))
    }

    /// The first peer in the table — the C# `FirstPeer`, useful for a client with one connection.
    pub fn first_peer(&self) -> Option<LnlNetPeer> {
        self.inner.peers_by_id.iter().min_by_key(|e| *e.key()).map(|e| LnlNetPeer::new(e.value().clone()))
    }

    pub fn peers_count(&self, state: ConnectionState) -> usize {
        self.inner.peers_by_id.iter().filter(|e| e.value().state() == state).count()
    }

    /// Runs the logic pass now rather than at the next tick.
    pub fn trigger_update(&self) {
        self.inner.trigger_update();
    }

    /// The C# `Stop(false)`: closes the sockets without telling any peer. What a client that
    /// crashed or lost its network looks like from the other side, which is what the timeout
    /// tests need to produce.
    pub fn stop_silently(&self) {
        self.inner.stop(false);
    }

    /// Probes a LiteNetLib server for its info line: the unconnected UDP query the C# clients
    /// send, answered by the server's `SendUnconnectedMessage`.
    pub async fn probe(target: ConnectionTarget, timeout_ms: i32) -> ServerProbeResult {
        let mut result = ServerProbeResult::default();
        let mut t = target.clone();
        if t.get(ConnectionTargetKeys::ADDRESS).is_none() {
            LNLConnectionTargetParser.parse(&mut t);
        }
        let Some(host) = t.get(ConnectionTargetKeys::ADDRESS) else {
            result.error = "connection string has no address".into();
            return result;
        };
        let port = t.get(ConnectionTargetKeys::PORT).and_then(|p| p.parse::<u16>().ok()).unwrap_or(LNLConnectionTargetParser::DEFAULT_PORT);
        let started = Instant::now();
        let probe = async {
            let mut addrs = tokio::net::lookup_host((host.as_str(), port)).await.map_err(|e| format!("could not resolve '{host}': {e}"))?;
            let addr = addrs.next().ok_or_else(|| format!("'{host}' has no addresses"))?;
            let bind: SocketAddr = if addr.is_ipv6() { "[::]:0".parse().map_err(|e| format!("{e}"))? } else { "0.0.0.0:0".parse().map_err(|e| format!("{e}"))? };
            let socket = UdpSocket::bind(bind).await.map_err(|e| e.to_string())?;
            let nonce: u16 = rand::random();
            let mut writer = NetDataWriter::new();
            writer.put_uint(BasisNetworkCommons::SERVER_INFO_QUERY_MAGIC);
            writer.put_ushort(BasisNetworkCommons::SERVER_INFO_PROTOCOL_VERSION);
            writer.put_ushort(nonce);
            while writer.length() < BasisNetworkCommons::SERVER_INFO_MIN_REQUEST_BYTES {
                writer.put_byte(0);
            }
            let mut packet = NetPacket::with_property(PacketProperty::UnconnectedMessage, writer.length());
            packet.raw_mut()[NetConstants::HEADER_SIZE..].copy_from_slice(writer.as_read_only_span());
            socket.send_to(packet.raw(), addr).await.map_err(|e| e.to_string())?;
            let mut buffer = vec![0u8; RECEIVE_BUFFER_BYTES];
            loop {
                let (n, from) = socket.recv_from(&mut buffer).await.map_err(|e| e.to_string())?;
                if from != addr || n < 2 || buffer[0] & 0x1F != PacketProperty::UnconnectedMessage as u8 {
                    continue;
                }
                return Ok::<(Vec<u8>, SocketAddr), String>((buffer[1..n].to_vec(), from));
            }
        };
        match tokio::time::timeout(Duration::from_millis(u64::try_from(timeout_ms.max(1)).unwrap_or(1)), probe).await {
            Err(_) => {
                result.timed_out = true;
                result.error = "timed out".into();
            }
            Ok(Err(e)) => result.error = e,
            Ok(Ok((bytes, from))) => {
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
                        result.round_trip_ms = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
                        result.resolved_address = Some(from.ip());
                    }
                    Err(e) => result.error = e,
                }
            }
        }
        result
    }

    /// Resolves `target`/`port` the way `NetUtils.MakeEndPoint` did, preferring a family this
    /// manager has a socket for. Name resolution blocks, as the C# call did.
    fn resolve(&self, target: &str, port: u16) -> BasisResult<SocketAddr> {
        let (host, port) = if port == 0 {
            let parsed = LNLConnectionTargetParser::try_parse_connection_string(target)
                .ok_or_else(|| BasisError::permanent(ErrorCode::InvalidArgument, format!("'{target}' is not a host:port connection string")))?;
            (parsed.address, parsed.port)
        } else {
            (target.trim().trim_start_matches('[').trim_end_matches(']').to_string(), port)
        };
        let host = if host == "localhost" { "127.0.0.1".to_string() } else { host };
        let has_v6 = self.inner.sockets.read().as_ref().is_some_and(|s| !s.v6.is_empty());
        let addrs: Vec<SocketAddr> = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| BasisError::with_source(basis_error::io_fault_kind(e.kind()), ErrorCode::Dns, format!("could not resolve '{host}'"), e))?
            .collect();
        addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or_else(|| addrs.iter().find(|a| has_v6 && a.is_ipv6()))
            .copied()
            .ok_or_else(|| BasisError::permanent(ErrorCode::Dns, format!("'{host}' has no address this transport can reach")))
    }
}

impl NetManager for LnlNetManager {
    fn start(&self, ipv4_address: IpAddr, ipv6_address: IpAddr, set_port: u16) -> BasisResult<()> {
        self.inner.start(ipv4_address, ipv6_address, set_port).map_err(|e| e.context(format!("starting the LiteNetLib transport on port {set_port}")))
    }

    fn stop(&self) {
        self.inner.stop(true);
    }

    fn connect(&self, target: &str, port: u16, writer: &NetDataWriter) -> BasisResult<NetPeerRef> {
        if !self.inner.running.load(Ordering::SeqCst) {
            return Err(BasisError::permanent(ErrorCode::Conflict, "connect before the LiteNetLib transport was started"));
        }
        let addr = self.resolve(target, port).map_err(|e| e.context(format!("parsing connect target '{target}'")))?;
        let requests = self.inner.requests.lock();
        if requests.contains_key(&addr) {
            return Err(BasisError::transient(ErrorCode::Conflict, format!("a connection request from {addr} is still being decided")));
        }
        let mut connection_number = 0;
        if let Some(peer) = self.inner.try_get_peer(addr) {
            match peer.state() {
                // just return already connected peer
                ConnectionState::Connected | ConnectionState::Outgoing => return Ok(Arc::new(LnlNetPeer::new(peer))),
                _ => {
                    // else reconnect
                    connection_number = (peer.connection_num() + 1) % NetConstants::MAX_CONNECTION_NUMBER;
                    self.inner.remove_peer(&peer);
                }
            }
        }
        // Create reliable connection and send connection request
        let peer = LnlPeer::new_outgoing(&self.inner, addr, self.inner.ids.allocate(), connection_number, writer.as_read_only_span());
        self.inner.add_peer(peer.clone());
        drop(requests);
        Ok(Arc::new(LnlNetPeer::new(peer)))
    }

    fn send_unconnected_message(&self, writer: &NetDataWriter, remote_end_point: SocketAddr) -> bool {
        let data = writer.as_read_only_span();
        let mut packet = NetPacket::with_property(PacketProperty::UnconnectedMessage, data.len());
        packet.raw_mut()[NetConstants::HEADER_SIZE..].copy_from_slice(data);
        self.inner.send_raw(packet.raw(), remote_end_point) > 0
    }

    fn statistics(&self) -> NetStatistics {
        NetStatistics {
            packets_sent: self.inner.packets_sent.load(Ordering::Relaxed).max(0) as u64,
            packets_received: self.inner.packets_received.load(Ordering::Relaxed).max(0) as u64,
            bytes_sent: self.inner.bytes_sent.load(Ordering::Relaxed).max(0) as u64,
            bytes_received: self.inner.bytes_received.load(Ordering::Relaxed).max(0) as u64,
            packet_loss: self.inner.packet_loss.load(Ordering::Relaxed).max(0) as u64,
        }
    }

    fn connected_peers_count(&self) -> i32 {
        self.inner.connected_peers_count.load(Ordering::Relaxed)
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

impl Drop for LnlNetManager {
    fn drop(&mut self) {
        self.inner.stop(true);
    }
}
