//! C ABI over the Basis iroh transport, for the C# clients (P/Invoke).
//!
//! This wraps the very same [`IrohNetManager`] the Rust server runs, so a C# client that goes
//! through here speaks the server's wire protocol byte for byte: the connect payload, the channel
//! and delivery semantics, the connection-request accept/reject handshake and the peer ids the
//! server assigns all come from the one implementation.
//!
//! Shape of the ABI:
//! - Managers and peers are `u64` handles; nothing crosses the boundary as a pointer that the
//!   caller has to free. A manager handle comes from [`basis_iroh_manager_create`]; a peer handle
//!   is the transport's stable peer identity and is delivered in events.
//! - Events are pulled, not pushed: the caller drains [`basis_iroh_manager_poll`] on whatever
//!   thread it likes (the C# manual-mode `PollEvents`, or a dedicated thread), which keeps
//!   managed callbacks off the transport's runtime threads.
//! - Every call returns `0`/a non-negative value on success and a negative code on failure, with
//!   the message available from [`basis_iroh_last_error`] on the same thread.
//!
//! All pointer arguments are borrowed for the duration of the call only.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]
#![deny(unused_must_use)]

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, c_char};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use basis_network_core::NetDataWriter;
use basis_network_core::configuration::{BasisTransportConfigStore, IrohTransportConfig};
use basis_network_core::transport::basis_network_shell::{ConnectionRequest, DeliveryMethod, DisconnectInfo, EventBasedNetListener, NetManager, NetPeerRef};
use basis_network_core::transport::{BasisNetworkStackRegistry, IrohNetManager};
use parking_lot::Mutex;

/// Bumped when the exported functions or structs change shape; the C# side checks it first.
/// 2 added `basis_iroh_manager_send_unconnected`, which a server needs to answer the
/// server-info probe. A host built against 1 will refuse to load this library, which is the
/// intent: the ABI check exists so a stale native library fails loudly at start rather than at
/// the first missing entry point.
pub const ABI_VERSION: u32 = 2;

pub const OK: i32 = 0;
/// A handle that names nothing (destroyed, never created, or a peer that has gone away).
pub const ERR_NO_HANDLE: i32 = -1;
/// A null pointer or a string that was not UTF-8.
pub const ERR_BAD_ARGUMENT: i32 = -2;
/// The caller's buffer is too small; the required size is reported alongside.
pub const ERR_BUFFER_TOO_SMALL: i32 = -3;
/// The transport refused the operation; `basis_iroh_last_error` has the reason.
pub const ERR_TRANSPORT: i32 = -4;
/// The Basis crate panicked underneath the ABI. Never expected — reported, never propagated.
pub const ERR_PANIC: i32 = -5;

pub const EVENT_NONE: u32 = 0;
pub const EVENT_PEER_CONNECTED: u32 = 1;
pub const EVENT_PEER_DISCONNECTED: u32 = 2;
pub const EVENT_RECEIVE: u32 = 3;
pub const EVENT_CONNECTION_REQUEST: u32 = 4;
pub const EVENT_NETWORK_ERROR: u32 = 5;
pub const EVENT_RECEIVE_UNCONNECTED: u32 = 6;

/// One transport event, filled in by [`basis_iroh_manager_poll`]. Payload bytes (a received
/// message, a connect payload, disconnect data) are copied into the caller's data buffer and
/// `data_len` says how many.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BasisIrohEvent {
    pub kind: u32,
    pub data_len: u32,
    pub peer: u64,
    pub request: u64,
    /// `DisconnectReason` as its C# enum ordinal, or the socket error code for a network error.
    pub reason: i32,
    pub socket_error: i32,
    pub channel: u8,
    /// `DeliveryMethod` as its wire value (LiteNetLib's numbering).
    pub delivery: u8,
    /// 4 or 16 bytes of `remote_ip` are meaningful; 0 when no address applies.
    pub remote_ip_len: u8,
    pub _reserved: u8,
    pub remote_port: u16,
    pub _reserved2: u16,
    pub remote_ip: [u8; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BasisIrohStatistics {
    pub packets_sent: u64,
    pub packets_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub packet_loss: u64,
    pub unreliable_dropped: i64,
    pub priority_unreliable_dropped: i64,
    pub connected_peers: i32,
    pub _reserved: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BasisIrohPeerInfo {
    pub id: i32,
    pub remote_id: i32,
    pub round_trip_time: i32,
    pub mtu: i32,
    pub time_since_last_packet: f32,
    pub connected: u8,
    pub ip_len: u8,
    pub _reserved: u16,
    pub ip: [u8; 16],
}

enum Queued {
    Connected(NetPeerRef),
    Disconnected(NetPeerRef, DisconnectInfo),
    Receive { peer: NetPeerRef, data: Vec<u8>, channel: u8, delivery: DeliveryMethod },
    Request { id: u64, data: Vec<u8>, remote: SocketAddr },
    Error(SocketAddr, i32),
    Unconnected(SocketAddr, Vec<u8>),
}

struct Manager {
    manager: Arc<IrohNetManager>,
    #[allow(dead_code)]
    listener: Arc<EventBasedNetListener>,
    queue: Mutex<VecDeque<Queued>>,
    peers: Mutex<HashMap<u64, NetPeerRef>>,
    requests: Mutex<HashMap<u64, Arc<dyn ConnectionRequest>>>,
    next_request: AtomicU64,
}

static MANAGERS: Mutex<Option<HashMap<u64, Arc<Manager>>>> = Mutex::new(None);
static NEXT_MANAGER: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_last_error(message: impl Into<String>) {
    let message = message.into();
    LAST_ERROR.with(|e| *e.borrow_mut() = message);
}

fn manager(handle: u64) -> Option<Arc<Manager>> {
    MANAGERS.lock().as_ref().and_then(|m| m.get(&handle).cloned())
}

fn with_manager(handle: u64, f: impl FnOnce(&Manager) -> i32) -> i32 {
    match manager(handle) {
        Some(m) => f(&m),
        None => {
            set_last_error(format!("manager handle {handle} does not exist"));
            ERR_NO_HANDLE
        }
    }
}

/// Runs `f` and turns a panic inside the Basis crates into an error code rather than unwinding
/// across the FFI boundary, which would abort the host process.
fn guarded(f: impl FnOnce() -> i32) -> i32 {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(payload) => {
            let message = payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown panic".to_string());
            set_last_error(format!("internal panic: {message}"));
            ERR_PANIC
        }
    }
}

/// # Safety
/// `ptr` must be valid for `len` bytes or null (an empty slice).
unsafe fn slice_from<'a>(ptr: *const u8, len: usize) -> &'a [u8] {
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: the caller guarantees the pointer is valid for `len` bytes.
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

/// # Safety
/// `ptr` must be a NUL-terminated string or null.
unsafe fn str_from<'a>(ptr: *const c_char) -> Result<&'a str, i32> {
    if ptr.is_null() {
        return Ok("");
    }
    // SAFETY: the caller guarantees a NUL-terminated string.
    unsafe { CStr::from_ptr(ptr) }.to_str().map_err(|_| {
        set_last_error("string argument was not valid UTF-8");
        ERR_BAD_ARGUMENT
    })
}

/// Copies `bytes` into the caller's buffer. Returns the length written, or the length needed
/// (as a negative buffer-too-small) when the buffer is short.
fn copy_out(bytes: &[u8], buf: *mut u8, cap: usize) -> i32 {
    if bytes.len() > cap || (buf.is_null() && !bytes.is_empty()) {
        set_last_error(format!("buffer of {cap} bytes is too small for {} bytes", bytes.len()));
        return ERR_BUFFER_TOO_SMALL;
    }
    if !bytes.is_empty() {
        // SAFETY: the caller guarantees `buf` is valid for `cap` bytes and cap >= bytes.len().
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len()) };
    }
    bytes.len() as i32
}

fn fill_addr(ip: &mut [u8; 16], ip_len: &mut u8, addr: IpAddr) {
    match addr {
        IpAddr::V4(v4) => {
            ip[..4].copy_from_slice(&v4.octets());
            *ip_len = 4;
        }
        IpAddr::V6(v6) => {
            ip.copy_from_slice(&v6.octets());
            *ip_len = 16;
        }
    }
}

fn delivery_from_wire(value: u8) -> Option<DeliveryMethod> {
    Some(match value {
        0 => DeliveryMethod::ReliableUnordered,
        1 => DeliveryMethod::Sequenced,
        2 => DeliveryMethod::ReliableOrdered,
        3 => DeliveryMethod::ReliableSequenced,
        4 => DeliveryMethod::Unreliable,
        _ => return None,
    })
}

/// The ABI version this library was built with; a caller compares it with its own constant.
#[unsafe(no_mangle)]
pub extern "C" fn basis_iroh_abi_version() -> u32 {
    ABI_VERSION
}

/// The message for the last failure on this thread, copied into `buf`. Returns the length.
///
/// # Safety
/// `buf` must be valid for `cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_last_error(buf: *mut u8, cap: usize) -> i32 {
    let message = LAST_ERROR.with(|e| e.borrow().clone());
    let bytes = message.as_bytes();
    let n = bytes.len().min(cap);
    if n > 0 && !buf.is_null() {
        // SAFETY: the caller guarantees `buf` is valid for `cap` bytes and n <= cap.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf, n) };
    }
    n as i32
}

/// Sets one field of the iroh transport configuration by its XML element name (`RelayMode`,
/// `IdleTimeoutMs`, ...), the way the server's `transports/IrohTransportConfig.xml` does.
/// Affects managers created afterwards.
///
/// # Safety
/// `name` and `value` must be NUL-terminated strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_set_transport_setting(name: *const c_char, value: *const c_char) -> i32 {
    guarded(|| {
        // SAFETY: documented contract of this function.
        let (name, value) = match unsafe { (str_from(name), str_from(value)) } {
            (Ok(n), Ok(v)) => (n, v),
            (Err(code), _) | (_, Err(code)) => return code,
        };
        use basis_network_core::configuration::BasisXmlConfig;
        let result = BasisTransportConfigStore::with_mut::<IrohTransportConfig, _>(BasisNetworkStackRegistry::IROH_ID, |c| c.set_field(name, value));
        match result {
            Ok(()) => OK,
            Err(e) => {
                set_last_error(e.to_string());
                ERR_BAD_ARGUMENT
            }
        }
    })
}

/// Creates a manager with a fresh (ephemeral) endpoint identity. Returns its handle, or 0 with
/// the reason in `basis_iroh_last_error`.
#[unsafe(no_mangle)]
pub extern "C" fn basis_iroh_manager_create(enable_statistics: i32) -> u64 {
    let mut handle = 0u64;
    let code = guarded(|| {
        let listener = EventBasedNetListener::new();
        let transport = BasisTransportConfigStore::get::<IrohTransportConfig>(BasisNetworkStackRegistry::IROH_ID);
        let manager = Arc::new(IrohNetManager::new(listener.clone(), transport, enable_statistics != 0, None));
        let entry = Arc::new(Manager { manager, listener: listener.clone(), queue: Mutex::new(VecDeque::new()), peers: Mutex::new(HashMap::new()), requests: Mutex::new(HashMap::new()), next_request: AtomicU64::new(1) });
        subscribe(&listener, &entry);
        let id = NEXT_MANAGER.fetch_add(1, Ordering::Relaxed);
        MANAGERS.lock().get_or_insert_with(HashMap::new).insert(id, entry);
        handle = id;
        OK
    });
    if code != OK { 0 } else { handle }
}

fn subscribe(listener: &Arc<EventBasedNetListener>, entry: &Arc<Manager>) {
    let weak: Weak<Manager> = Arc::downgrade(entry);
    let push = move |event: Queued| {
        if let Some(m) = weak.upgrade() {
            m.queue.lock().push_back(event);
        }
    };
    let p = push.clone();
    listener.peer_connected_event.subscribe(Arc::new(move |peer| p(Queued::Connected(peer))));
    let p = push.clone();
    listener.peer_disconnected_event.subscribe(Arc::new(move |peer, info| p(Queued::Disconnected(peer, info))));
    let p = push.clone();
    listener.network_receive_event.subscribe(Arc::new(move |peer, mut reader, channel, delivery| {
        let data = reader.get_remaining_bytes();
        p(Queued::Receive { peer, data, channel, delivery });
    }));
    let weak_for_requests: Weak<Manager> = Arc::downgrade(entry);
    let p = push.clone();
    listener.connection_request_event.subscribe(Arc::new(move |request| {
        let Some(m) = weak_for_requests.upgrade() else { return };
        let id = m.next_request.fetch_add(1, Ordering::Relaxed);
        let mut reader = request.data();
        let data = reader.get_remaining_bytes();
        let remote = request.remote_end_point();
        m.requests.lock().insert(id, request);
        p(Queued::Request { id, data, remote });
    }));
    let p = push.clone();
    listener.network_error_event.subscribe(Arc::new(move |endpoint, code| p(Queued::Error(endpoint, code))));
    listener.network_receive_unconnected_event.subscribe(Arc::new(move |endpoint, mut reader| {
        let data = reader.get_remaining_bytes();
        push(Queued::Unconnected(endpoint, data));
    }));
}

/// Binds the endpoint. Empty or null addresses mean "any"; port 0 lets the OS choose.
///
/// # Safety
/// `ipv4` and `ipv6` must be NUL-terminated strings or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_manager_start(handle: u64, ipv4: *const c_char, ipv6: *const c_char, port: u16) -> i32 {
    guarded(|| {
        // SAFETY: documented contract of this function.
        let (ipv4, ipv6) = match unsafe { (str_from(ipv4), str_from(ipv6)) } {
            (Ok(a), Ok(b)) => (a, b),
            (Err(code), _) | (_, Err(code)) => return code,
        };
        let v4: IpAddr = if ipv4.is_empty() { IpAddr::from([0, 0, 0, 0]) } else {
            match ipv4.parse() {
                Ok(a) => a,
                Err(_) => {
                    set_last_error(format!("'{ipv4}' is not an IPv4 address"));
                    return ERR_BAD_ARGUMENT;
                }
            }
        };
        let v6: IpAddr = if ipv6.is_empty() { IpAddr::from([0u16; 8]) } else {
            match ipv6.parse() {
                Ok(a) => a,
                Err(_) => {
                    set_last_error(format!("'{ipv6}' is not an IPv6 address"));
                    return ERR_BAD_ARGUMENT;
                }
            }
        };
        with_manager(handle, |m| match m.manager.start(v4, v6, port) {
            Ok(()) => OK,
            Err(e) => {
                set_last_error(e.report());
                ERR_TRANSPORT
            }
        })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn basis_iroh_manager_stop(handle: u64) -> i32 {
    guarded(|| {
        with_manager(handle, |m| {
            m.manager.stop();
            OK
        })
    })
}

/// Stops the manager if it is still running and frees the handle.
#[unsafe(no_mangle)]
pub extern "C" fn basis_iroh_manager_destroy(handle: u64) -> i32 {
    guarded(|| {
        let removed = MANAGERS.lock().as_mut().and_then(|m| m.remove(&handle));
        match removed {
            Some(m) => {
                m.manager.stop();
                m.queue.lock().clear();
                m.peers.lock().clear();
                m.requests.lock().clear();
                OK
            }
            None => {
                set_last_error(format!("manager handle {handle} does not exist"));
                ERR_NO_HANDLE
            }
        }
    })
}

/// Connects to `target` (`<endpoint-id>[@host:port][#password]`) presenting `payload` as the
/// connect data. On success the peer handle is written to `out_peer`; the outcome arrives later
/// as a PeerConnected or PeerDisconnected event for that handle.
///
/// # Safety
/// `target` must be a NUL-terminated string; `payload` valid for `len` bytes; `out_peer` non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_manager_connect(handle: u64, target: *const c_char, port: u16, payload: *const u8, len: usize, out_peer: *mut u64) -> i32 {
    guarded(|| {
        if out_peer.is_null() {
            set_last_error("out_peer was null");
            return ERR_BAD_ARGUMENT;
        }
        // SAFETY: documented contract of this function.
        let target = match unsafe { str_from(target) } {
            Ok(t) => t,
            Err(code) => return code,
        };
        // SAFETY: documented contract of this function.
        let payload = unsafe { slice_from(payload, len) };
        let writer = NetDataWriter::from_slice(payload);
        with_manager(handle, |m| match m.manager.connect(target, port, &writer) {
            Ok(peer) => {
                let id = peer.identity();
                m.peers.lock().insert(id, peer);
                // SAFETY: out_peer was checked non-null above.
                unsafe { *out_peer = id };
                OK
            }
            Err(e) => {
                set_last_error(e.report());
                ERR_TRANSPORT
            }
        })
    })
}

/// Takes the next queued event. Returns 1 with `out` filled when there was one, 0 when the queue
/// is empty, or `ERR_BUFFER_TOO_SMALL` (the event stays queued and `out.data_len` says how much
/// room it needs).
///
/// # Safety
/// `out` must be non-null; `data` valid for `cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_manager_poll(handle: u64, out: *mut BasisIrohEvent, data: *mut u8, cap: usize) -> i32 {
    guarded(|| {
        if out.is_null() {
            set_last_error("out was null");
            return ERR_BAD_ARGUMENT;
        }
        with_manager(handle, |m| {
            let mut queue = m.queue.lock();
            let Some(event) = queue.pop_front() else { return 0 };
            let mut ev = BasisIrohEvent::default();
            let payload: &[u8] = match &event {
                Queued::Connected(peer) => {
                    ev.kind = EVENT_PEER_CONNECTED;
                    ev.peer = peer.identity();
                    m.peers.lock().insert(peer.identity(), peer.clone());
                    &[]
                }
                Queued::Disconnected(peer, info) => {
                    ev.kind = EVENT_PEER_DISCONNECTED;
                    ev.peer = peer.identity();
                    ev.reason = info.reason as i32;
                    ev.socket_error = info.socket_error_code;
                    info.additional_data.get_remaining_bytes_span()
                }
                Queued::Receive { peer, data, channel, delivery } => {
                    ev.kind = EVENT_RECEIVE;
                    ev.peer = peer.identity();
                    ev.channel = *channel;
                    ev.delivery = *delivery as u8;
                    data
                }
                Queued::Request { id, data, remote, .. } => {
                    ev.kind = EVENT_CONNECTION_REQUEST;
                    ev.request = *id;
                    ev.remote_port = remote.port();
                    fill_addr(&mut ev.remote_ip, &mut ev.remote_ip_len, remote.ip());
                    data
                }
                Queued::Error(endpoint, code) => {
                    ev.kind = EVENT_NETWORK_ERROR;
                    ev.socket_error = *code;
                    ev.reason = *code;
                    ev.remote_port = endpoint.port();
                    fill_addr(&mut ev.remote_ip, &mut ev.remote_ip_len, endpoint.ip());
                    &[]
                }
                Queued::Unconnected(endpoint, data) => {
                    ev.kind = EVENT_RECEIVE_UNCONNECTED;
                    ev.remote_port = endpoint.port();
                    fill_addr(&mut ev.remote_ip, &mut ev.remote_ip_len, endpoint.ip());
                    data
                }
            };
            let needed = payload.len();
            ev.data_len = needed as u32;
            let too_small = needed > cap || (data.is_null() && needed > 0);
            if !too_small && needed > 0 {
                // SAFETY: the caller guarantees `data` is valid for `cap` bytes and cap >= needed.
                unsafe { std::ptr::copy_nonoverlapping(payload.as_ptr(), data, needed) };
            }
            if too_small {
                // SAFETY: out was checked non-null.
                unsafe { *out = ev };
                queue.push_front(event);
                set_last_error(format!("event needs {needed} bytes, buffer holds {cap}"));
                return ERR_BUFFER_TOO_SMALL;
            }
            if let Queued::Disconnected(peer, _) = &event {
                // The transport is done with this peer; the handle stays valid for the caller's
                // own bookkeeping but sends to it will fail with ERR_NO_HANDLE from now on.
                m.peers.lock().remove(&peer.identity());
            }
            // SAFETY: out was checked non-null.
            unsafe { *out = ev };
            1
        })
    })
}

/// Number of events waiting, so a pump can size its buffer or decide to keep draining.
#[unsafe(no_mangle)]
pub extern "C" fn basis_iroh_manager_pending_events(handle: u64) -> i32 {
    guarded(|| with_manager(handle, |m| m.queue.lock().len() as i32))
}

#[unsafe(no_mangle)]
pub extern "C" fn basis_iroh_manager_connected_peers(handle: u64) -> i32 {
    guarded(|| with_manager(handle, |m| m.manager.connected_peers_count()))
}

/// # Safety
/// `out` must be non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_manager_statistics(handle: u64, out: *mut BasisIrohStatistics) -> i32 {
    guarded(|| {
        if out.is_null() {
            set_last_error("out was null");
            return ERR_BAD_ARGUMENT;
        }
        with_manager(handle, |m| {
            let s = m.manager.statistics();
            let stats = BasisIrohStatistics {
                packets_sent: s.packets_sent,
                packets_received: s.packets_received,
                bytes_sent: s.bytes_sent,
                bytes_received: s.bytes_received,
                packet_loss: s.packet_loss,
                unreliable_dropped: m.manager.unreliable_dropped(),
                priority_unreliable_dropped: m.manager.priority_unreliable_dropped(),
                connected_peers: m.manager.connected_peers_count(),
                _reserved: 0,
            };
            // SAFETY: out was checked non-null.
            unsafe { *out = stats };
            OK
        })
    })
}

/// `<endpoint-id>@host:port` for this endpoint — what another client passes to connect. Written
/// to `buf`; returns the length.
///
/// # Safety
/// `buf` must be valid for `cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_manager_connection_string(handle: u64, buf: *mut u8, cap: usize) -> i32 {
    guarded(|| with_manager(handle, |m| copy_out(m.manager.connection_string().as_bytes(), buf, cap)))
}

/// This endpoint's full iroh address as JSON — the bytes a P2P introduce request carries.
///
/// # Safety
/// `buf` must be valid for `cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_manager_endpoint_addr_json(handle: u64, buf: *mut u8, cap: usize) -> i32 {
    guarded(|| {
        with_manager(handle, |m| match m.manager.endpoint_addr().and_then(|a| serde_json::to_vec(&a).ok()) {
            Some(json) => copy_out(&json, buf, cap),
            None => {
                set_last_error("the endpoint has not been started");
                ERR_TRANSPORT
            }
        })
    })
}

/// Turns an endpoint address JSON (from `basis_iroh_manager_endpoint_addr_json` on the other
/// side) into the connection string `basis_iroh_manager_connect` takes, preferring a direct
/// address.
///
/// # Safety
/// `json` must be valid for `json_len` bytes; `buf` for `cap` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_endpoint_addr_to_connection_string(json: *const u8, json_len: usize, buf: *mut u8, cap: usize) -> i32 {
    guarded(|| {
        // SAFETY: documented contract of this function.
        let json = unsafe { slice_from(json, json_len) };
        match serde_json::from_slice::<iroh::EndpointAddr>(json) {
            Ok(addr) => {
                let id = addr.id.to_z32();
                let text = match addr.ip_addrs().next() {
                    Some(socket) => format!("{id}@{socket}"),
                    None => id,
                };
                copy_out(text.as_bytes(), buf, cap)
            }
            Err(e) => {
                set_last_error(format!("not an endpoint address: {e}"));
                ERR_BAD_ARGUMENT
            }
        }
    })
}

fn with_peer(handle: u64, peer: u64, f: impl FnOnce(&NetPeerRef) -> i32) -> i32 {
    with_manager(handle, |m| {
        let found = m.peers.lock().get(&peer).cloned();
        match found {
            Some(p) => f(&p),
            None => {
                set_last_error(format!("peer {peer} is not connected"));
                ERR_NO_HANDLE
            }
        }
    })
}

/// # Safety
/// `data` must be valid for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_peer_send(handle: u64, peer: u64, channel: u8, delivery: u8, data: *const u8, len: usize) -> i32 {
    guarded(|| {
        let Some(method) = delivery_from_wire(delivery) else {
            set_last_error(format!("{delivery} is not a delivery method"));
            return ERR_BAD_ARGUMENT;
        };
        // SAFETY: documented contract of this function.
        let data = unsafe { slice_from(data, len) };
        with_peer(handle, peer, |p| match p.send(data, channel, method) {
            Ok(()) => OK,
            Err(e) => {
                set_last_error(e.to_string());
                ERR_TRANSPORT
            }
        })
    })
}

/// # Safety
/// `data` must be valid for `len` bytes or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_peer_disconnect(handle: u64, peer: u64, data: *const u8, len: usize) -> i32 {
    guarded(|| {
        // SAFETY: documented contract of this function.
        let data = unsafe { slice_from(data, len) };
        with_peer(handle, peer, |p| {
            if data.is_empty() {
                p.disconnect();
            } else {
                p.disconnect_with(data);
            }
            OK
        })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn basis_iroh_peer_queue_count(handle: u64, peer: u64, channel: u8, delivery: u8) -> i32 {
    guarded(|| {
        let Some(method) = delivery_from_wire(delivery) else {
            set_last_error(format!("{delivery} is not a delivery method"));
            return ERR_BAD_ARGUMENT;
        };
        with_peer(handle, peer, |p| p.get_packets_count_in_queue(channel, method))
    })
}

/// # Safety
/// `out` must be non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_peer_info(handle: u64, peer: u64, out: *mut BasisIrohPeerInfo) -> i32 {
    guarded(|| {
        if out.is_null() {
            set_last_error("out was null");
            return ERR_BAD_ARGUMENT;
        }
        with_peer(handle, peer, |p| {
            let mut info = BasisIrohPeerInfo { id: p.id(), remote_id: p.remote_id(), round_trip_time: p.round_trip_time(), mtu: p.mtu(), time_since_last_packet: p.time_since_last_packet(), connected: u8::from(p.is_connected()), ..Default::default() };
            fill_addr(&mut info.ip, &mut info.ip_len, p.address());
            // SAFETY: out was checked non-null.
            unsafe { *out = info };
            OK
        })
    })
}

/// Answers an unconnected message — the server-info probe — on the connection the probe arrived
/// on. `ip`/`ip_len` and `port` identify the probe, and must be the values the
/// `ReceiveUnconnected` event carried; the transport holds that connection open briefly for
/// exactly this reply.
///
/// Returns `OK` when the reply was handed to the transport, `ERR_TRANSPORT` when there was no
/// probe waiting at that address (it timed out, or the address does not match).
///
/// # Safety
/// `ip` must point to `ip_len` readable bytes (4 or 16) and `data` to `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_manager_send_unconnected(
    handle: u64,
    ip: *const u8,
    ip_len: u8,
    port: u16,
    data: *const u8,
    len: usize,
) -> i32 {
    guarded(|| {
        let address = match ip_len {
            4 => {
                if ip.is_null() {
                    set_last_error("ip was null");
                    return ERR_BAD_ARGUMENT;
                }
                // SAFETY: ip_len says four readable bytes; checked non-null above.
                let octets = unsafe { slice_from(ip, 4) };
                IpAddr::V4(std::net::Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3]))
            }
            16 => {
                if ip.is_null() {
                    set_last_error("ip was null");
                    return ERR_BAD_ARGUMENT;
                }
                // SAFETY: ip_len says sixteen readable bytes; checked non-null above.
                let octets = unsafe { slice_from(ip, 16) };
                let mut bytes = [0u8; 16];
                bytes.copy_from_slice(octets);
                IpAddr::V6(std::net::Ipv6Addr::from(bytes))
            }
            other => {
                set_last_error(format!("ip_len must be 4 or 16, got {other}"));
                return ERR_BAD_ARGUMENT;
            }
        };
        // SAFETY: the caller promises len readable bytes at data; an empty reply is allowed.
        let payload = unsafe { slice_from(data, len) };
        with_manager(handle, |m| {
            let writer = NetDataWriter::from_slice(payload);
            if m.manager.send_unconnected_message(&writer, SocketAddr::new(address, port)) {
                OK
            } else {
                set_last_error(format!("no probe from {address}:{port} was waiting for a reply"));
                ERR_TRANSPORT
            }
        })
    })
}

/// Forgets a peer handle the caller is done with (a disconnected one is forgotten automatically).
#[unsafe(no_mangle)]
pub extern "C" fn basis_iroh_peer_release(handle: u64, peer: u64) -> i32 {
    guarded(|| {
        with_manager(handle, |m| {
            m.peers.lock().remove(&peer);
            OK
        })
    })
}

/// Admits a pending connection. Writes the new peer's handle to `out_peer`.
///
/// # Safety
/// `out_peer` must be non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_request_accept(handle: u64, request: u64, out_peer: *mut u64) -> i32 {
    guarded(|| {
        if out_peer.is_null() {
            set_last_error("out_peer was null");
            return ERR_BAD_ARGUMENT;
        }
        with_manager(handle, |m| {
            let pending = m.requests.lock().remove(&request);
            let Some(pending) = pending else {
                set_last_error(format!("connection request {request} is not pending"));
                return ERR_NO_HANDLE;
            };
            match pending.accept() {
                Ok(peer) => {
                    let id = peer.identity();
                    m.peers.lock().insert(id, peer);
                    // SAFETY: out_peer was checked non-null.
                    unsafe { *out_peer = id };
                    OK
                }
                Err(e) => {
                    set_last_error(e.report());
                    ERR_TRANSPORT
                }
            }
        })
    })
}

/// Refuses a pending connection, sending `data` as the reject payload.
///
/// # Safety
/// `data` must be valid for `len` bytes or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn basis_iroh_request_reject(handle: u64, request: u64, data: *const u8, len: usize) -> i32 {
    guarded(|| {
        // SAFETY: documented contract of this function.
        let data = unsafe { slice_from(data, len) };
        with_manager(handle, |m| {
            let pending = m.requests.lock().remove(&request);
            let Some(pending) = pending else {
                set_last_error(format!("connection request {request} is not pending"));
                return ERR_NO_HANDLE;
            };
            match pending.reject(&NetDataWriter::from_slice(data)) {
                Ok(()) => OK,
                Err(e) => {
                    set_last_error(e.report());
                    ERR_TRANSPORT
                }
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::time::{Duration, Instant};

    fn last_error() -> String {
        let mut buf = vec![0u8; 512];
        let n = unsafe { basis_iroh_last_error(buf.as_mut_ptr(), buf.len()) };
        String::from_utf8_lossy(&buf[..n.max(0) as usize]).into_owned()
    }

    fn poll_until(handle: u64, kind: u32, timeout: Duration) -> (BasisIrohEvent, Vec<u8>) {
        let deadline = Instant::now() + timeout;
        let mut data = vec![0u8; 4096];
        loop {
            let mut ev = BasisIrohEvent::default();
            let code = unsafe { basis_iroh_manager_poll(handle, &mut ev, data.as_mut_ptr(), data.len()) };
            assert!(code >= 0, "poll failed: {code} {}", last_error());
            if code == 1 && ev.kind == kind {
                return (ev, data[..ev.data_len as usize].to_vec());
            }
            assert!(Instant::now() < deadline, "no event of kind {kind} within {timeout:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn abi_round_trip_over_loopback() {
        assert_eq!(basis_iroh_abi_version(), ABI_VERSION);

        let server = basis_iroh_manager_create(1);
        let client = basis_iroh_manager_create(0);
        assert!(server != 0 && client != 0, "{}", last_error());

        let any = CString::new("127.0.0.1").unwrap();
        let none = CString::new("").unwrap();
        assert_eq!(unsafe { basis_iroh_manager_start(server, any.as_ptr(), none.as_ptr(), 0) }, OK, "{}", last_error());
        assert_eq!(unsafe { basis_iroh_manager_start(client, any.as_ptr(), none.as_ptr(), 0) }, OK, "{}", last_error());

        let mut buf = vec![0u8; 256];
        let n = unsafe { basis_iroh_manager_connection_string(server, buf.as_mut_ptr(), buf.len()) };
        assert!(n > 0, "{}", last_error());
        let target = CString::new(&buf[..n as usize]).unwrap();

        // A buffer that cannot hold the answer reports how much it needed instead of truncating.
        let mut tiny = [0u8; 4];
        assert_eq!(unsafe { basis_iroh_manager_connection_string(server, tiny.as_mut_ptr(), tiny.len()) }, ERR_BUFFER_TOO_SMALL);

        let payload = b"connect-payload";
        let mut outgoing = 0u64;
        assert_eq!(unsafe { basis_iroh_manager_connect(client, target.as_ptr(), 0, payload.as_ptr(), payload.len(), &mut outgoing) }, OK, "{}", last_error());
        assert_ne!(outgoing, 0);

        let (request, data) = poll_until(server, EVENT_CONNECTION_REQUEST, Duration::from_secs(20));
        assert_eq!(data, payload);
        assert_eq!(request.remote_ip_len, 4);
        let mut accepted = 0u64;
        assert_eq!(unsafe { basis_iroh_request_accept(server, request.request, &mut accepted) }, OK, "{}", last_error());
        // Accepting twice is refused rather than silently repeated.
        assert_eq!(unsafe { basis_iroh_request_accept(server, request.request, &mut accepted) }, ERR_NO_HANDLE);

        let (connected, _) = poll_until(client, EVENT_PEER_CONNECTED, Duration::from_secs(20));
        assert_eq!(connected.peer, outgoing);

        let message = b"ping";
        assert_eq!(unsafe { basis_iroh_peer_send(client, outgoing, 3, 2, message.as_ptr(), message.len()) }, OK, "{}", last_error());
        let (received, data) = poll_until(server, EVENT_RECEIVE, Duration::from_secs(20));
        assert_eq!(data, message);
        assert_eq!(received.channel, 3);
        assert_eq!(received.delivery, 2);
        assert_eq!(received.peer, accepted);

        let mut info = BasisIrohPeerInfo::default();
        assert_eq!(unsafe { basis_iroh_peer_info(server, accepted, &mut info) }, OK);
        assert_eq!(info.connected, 1);

        // Bad arguments are reported, never panicked on.
        assert_eq!(unsafe { basis_iroh_peer_send(client, outgoing, 3, 9, message.as_ptr(), message.len()) }, ERR_BAD_ARGUMENT);
        assert_eq!(unsafe { basis_iroh_peer_send(client, 12345, 3, 2, message.as_ptr(), message.len()) }, ERR_NO_HANDLE);
        assert_eq!(basis_iroh_manager_stop(999_999), ERR_NO_HANDLE);

        assert_eq!(unsafe { basis_iroh_peer_disconnect(client, outgoing, std::ptr::null(), 0) }, OK, "{}", last_error());
        let (gone, _) = poll_until(server, EVENT_PEER_DISCONNECTED, Duration::from_secs(20));
        assert_eq!(gone.peer, accepted);

        assert_eq!(basis_iroh_manager_destroy(client), OK);
        assert_eq!(basis_iroh_manager_destroy(server), OK);
        assert_eq!(basis_iroh_manager_destroy(server), ERR_NO_HANDLE);
    }

    #[test]
    fn endpoint_addr_json_becomes_a_connection_string() {
        let handle = basis_iroh_manager_create(0);
        let any = CString::new("127.0.0.1").unwrap();
        assert_eq!(unsafe { basis_iroh_manager_start(handle, any.as_ptr(), std::ptr::null(), 0) }, OK, "{}", last_error());
        let mut json = vec![0u8; 4096];
        let n = unsafe { basis_iroh_manager_endpoint_addr_json(handle, json.as_mut_ptr(), json.len()) };
        assert!(n > 0, "{}", last_error());
        let mut text = vec![0u8; 256];
        let m = unsafe { basis_iroh_endpoint_addr_to_connection_string(json.as_ptr(), n as usize, text.as_mut_ptr(), text.len()) };
        assert!(m > 0, "{}", last_error());
        let connection = String::from_utf8_lossy(&text[..m as usize]).into_owned();
        assert!(connection.contains('@') && connection.contains("127.0.0.1:"), "{connection}");
        let garbage = b"not json";
        assert_eq!(unsafe { basis_iroh_endpoint_addr_to_connection_string(garbage.as_ptr(), garbage.len(), text.as_mut_ptr(), text.len()) }, ERR_BAD_ARGUMENT);
        assert_eq!(basis_iroh_manager_destroy(handle), OK);
    }

    #[test]
    fn transport_settings_are_validated() {
        let name = CString::new("IdleTimeoutMs").unwrap();
        let good = CString::new("45000").unwrap();
        let bad = CString::new("soon").unwrap();
        assert_eq!(unsafe { basis_iroh_set_transport_setting(name.as_ptr(), good.as_ptr()) }, OK);
        assert_eq!(unsafe { basis_iroh_set_transport_setting(name.as_ptr(), bad.as_ptr()) }, ERR_BAD_ARGUMENT);
        assert!(last_error().contains("IdleTimeoutMs"));
        let unknown = CString::new("NoSuchSetting").unwrap();
        assert_eq!(unsafe { basis_iroh_set_transport_setting(unknown.as_ptr(), good.as_ptr()) }, ERR_BAD_ARGUMENT);
        let reset = CString::new("30000").unwrap();
        assert_eq!(unsafe { basis_iroh_set_transport_setting(name.as_ptr(), reset.as_ptr()) }, OK);
    }
}
