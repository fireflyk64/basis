//! Port of `HelloPeerClient.cs` for the iroh stack: a hello client that can also talk to another
//! player directly, with the server acting only as an introducer.
//!
//! The sequence, all of it real:
//! 1. Initiator sends `P2PSub_Request` on the P2P channel with a session token and an X25519
//!    ephemeral public key. The server forwards it and answers `ServerArmed`.
//! 2. Target answers `P2PSub_Accept` with its own ephemeral key; both derive the per-pair keys.
//! 3. Both send an `IntroduceRequest` carrying their own iroh endpoint address. LiteNetLib
//!    punched from a second socket here; an iroh endpoint hole-punches itself, so the server
//!    just hands each side the other's address (`Introduce`) and tells the initiator to dial.
//! 4. The initiator dials the address; the target accepts the connection whose payload names a
//!    session it is punching for.
//! 5. Both report `P2PSub_LinkUp`; once the server has heard from both it marks the pair
//!    offloaded and answers `P2PSub_Offloaded` — the point at which it stops relaying.
//!
//! Sends through `send_number_direct` take the direct link when one is up and fall back to the
//! server's direct-origin relay channel when one is not. The direct link is a QUIC connection,
//! encrypted end to end by iroh; the ephemeral key exchange is kept for protocol parity.

use std::sync::Arc;
use std::time::{Duration, Instant};

use basis_error::{BasisError, BasisResult, ErrorCode};
use basis_network_core::SerializableBasis::{BasisP2PIntroduce, BasisP2PIntroduceRequest, BasisP2PSignalMessage};
use basis_network_core::encryption::basis_crypto_handshake::BasisCryptoHandshake;
use basis_network_core::transport::basis_network_shell::{ConnectionRequest, peers_equal};
use basis_network_core::transport::iroh_network_impl::IrohNetManager;
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetDataReader, NetDataWriter, NetPeerRef};
use dashmap::DashMap;
use parking_lot::{Condvar, Mutex};

use crate::basis_hello_client::{BasisHelloClient, HelloExtension, HelloTransport};

struct DirectSession {
    token: String,
    other_player_id: u16,
    local_private: Vec<u8>,
    local_public: Vec<u8>,
    keys: Mutex<Option<(Vec<u8>, Vec<u8>)>>,
    peer: Mutex<Option<NetPeerRef>>,
    punching: std::sync::atomic::AtomicBool,
    offloaded: std::sync::atomic::AtomicBool,
    dialed: std::sync::atomic::AtomicBool,
    confirmed: (Mutex<bool>, Condvar),
}

impl DirectSession {
    fn new(token: String, other_player_id: u16) -> Self {
        let (local_private, local_public) = BasisCryptoHandshake::generate_key_pair();
        Self {
            token,
            other_player_id,
            local_private,
            local_public,
            keys: Mutex::new(None),
            peer: Mutex::new(None),
            punching: std::sync::atomic::AtomicBool::new(false),
            offloaded: std::sync::atomic::AtomicBool::new(false),
            dialed: std::sync::atomic::AtomicBool::new(false),
            confirmed: (Mutex::new(false), Condvar::new()),
        }
    }

    fn confirm(&self) {
        *self.confirmed.0.lock() = true;
        self.confirmed.1.notify_all();
    }

    fn wait_confirmed(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut flag = self.confirmed.0.lock();
        while !*flag {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            self.confirmed.1.wait_for(&mut flag, deadline - now);
        }
        true
    }

    fn is_offloaded(&self) -> bool {
        self.offloaded.load(std::sync::atomic::Ordering::Acquire)
    }
}

pub struct HelloPeerClient {
    base: Arc<BasisHelloClient>,
    by_token: DashMap<String, Arc<DirectSession>>,
    by_player: DashMap<u16, Arc<DirectSession>>,
}

impl HelloPeerClient {
    pub fn new(display_name: &str) -> BasisResult<Arc<Self>> {
        let base = BasisHelloClient::new(display_name)?;
        let this = Arc::new(Self { base: base.clone(), by_token: DashMap::new(), by_player: DashMap::new() });
        base.set_extension(Some(this.clone()));
        Ok(this)
    }

    pub fn base(&self) -> &Arc<BasisHelloClient> {
        &self.base
    }

    pub fn display_name(&self) -> &str {
        self.base.display_name()
    }

    pub fn player_id(&self) -> u16 {
        self.base.player_id()
    }

    pub fn is_joined(&self) -> bool {
        self.base.is_joined()
    }

    pub fn connect(&self, target: &str, port: u16, password: &str, timeout: Duration) -> BasisResult<bool> {
        self.base.connect(target, port, password, timeout)
    }

    pub fn disconnect(&self) {
        self.base.disconnect();
    }

    /// Number of peers this client currently has a server-confirmed direct link to.
    pub fn direct_link_count(&self) -> usize {
        self.by_player.iter().filter(|s| s.is_offloaded() && s.peer.lock().is_some()).count()
    }

    /// True once the server has confirmed a direct link to that player is carrying traffic.
    pub fn has_direct_link(&self, other_player_id: u16) -> bool {
        self.by_player.get(&other_player_id).is_some_and(|s| s.is_offloaded() && s.peer.lock().is_some())
    }

    /// The iroh address other peers dial to reach this client, JSON-encoded for the introducer.
    pub fn endpoint_addr_bytes(&self) -> Option<Vec<u8>> {
        let client = self.base.network_client()?;
        let manager = client.client()?;
        let iroh = manager.as_any().downcast_ref::<IrohNetManager>()?;
        let addr = iroh.endpoint_addr()?;
        serde_json::to_vec(&addr).ok()
    }

    /// Opens a direct link to another player and waits for the server to confirm it, returning
    /// `Ok(false)` if that has not happened within `timeout`. A false here is not a failure to
    /// communicate — sends fall back to the server relay.
    pub fn open_direct_link(&self, other_player_id: u16, timeout: Duration) -> BasisResult<bool> {
        let server = self.base.server_peer().filter(|_| self.base.is_joined()).ok_or_else(|| BasisHelloClient::not_joined(self.display_name()))?;
        if other_player_id == self.player_id() {
            return Err(BasisError::permanent(ErrorCode::InvalidArgument, "A client cannot open a direct link to itself."));
        }
        if let Some(existing) = self.by_player.get(&other_player_id).map(|s| s.clone()) {
            return Ok(Self::await_link(&existing, timeout));
        }
        let session = Arc::new(DirectSession::new(uuid::Uuid::new_v4().simple().to_string(), other_player_id));
        if !self.register(&session) {
            // Both sides asked at once and the other's session won the slot.
            return Ok(self.by_player.get(&other_player_id).map(|s| s.clone()).is_some_and(|winner| Self::await_link(&winner, timeout)));
        }
        self.send_signal(&server, BasisNetworkCommons::P2P_SUB_REQUEST, other_player_id, &session.token, Some(session.local_public.clone()))?;
        Ok(Self::await_link(&session, timeout))
    }

    /// True only for a link the server confirmed. A session the server declined (no such
    /// player, a link that was lost) also wakes the waiter, and that is a false, not a timeout.
    fn await_link(session: &DirectSession, timeout: Duration) -> bool {
        session.wait_confirmed(timeout) && session.is_offloaded()
    }

    pub fn send_number_direct(&self, target_player_id: u16, value: i32) -> BasisResult<()> {
        self.send_direct(target_player_id, &BasisHelloClient::encode_number(value))
    }

    pub fn send_text_direct(&self, target_player_id: u16, text: &str) -> BasisResult<()> {
        self.send_direct(target_player_id, &BasisHelloClient::encode_text(text))
    }

    fn send_direct(&self, target_player_id: u16, payload: &[u8]) -> BasisResult<()> {
        if let Some(session) = self.by_player.get(&target_player_id).map(|s| s.clone())
            && let Some(peer) = session.peer.lock().clone()
            && peer.is_connected()
        {
            // No recipient list and no sender id: a direct link is point to point.
            let mut writer = NetDataWriter::new();
            writer.put_ushort(BasisHelloClient::HELLO_MESSAGE_INDEX);
            writer.put_bytes(payload);
            peer.send_writer(&writer, BasisNetworkCommons::DIRECT_SCENE_CHANNEL, DeliveryMethod::ReliableOrdered)?;
            return Ok(());
        }
        let server = self.base.server_peer().filter(|_| self.base.is_joined()).ok_or_else(|| BasisHelloClient::not_joined(self.display_name()))?;
        // The fallback the protocol is built around: same message, relayed by the server on the
        // channel that says "this would have gone direct if it could".
        BasisHelloClient::send_via(&server, target_player_id, payload, BasisNetworkCommons::DIRECT_SCENE_SERVER_CHANNEL)
    }

    // ── P2P signalling, all of it through the server on the P2P channel ──

    fn handle_signal(&self, server: &NetPeerRef, reader: &mut NetDataReader) {
        let Ok(sub) = reader.get_byte() else {
            return;
        };
        if sub == BasisNetworkCommons::P2P_SUB_INTRODUCE {
            let mut msg = BasisP2PIntroduce::default();
            if msg.deserialize(reader).is_ok() {
                self.on_introduce(msg);
            }
            return;
        }
        let mut msg = BasisP2PSignalMessage::default();
        if msg.deserialize(reader).is_err() {
            return;
        }
        match sub {
            BasisNetworkCommons::P2P_SUB_REQUEST => self.on_inbound_request(server, msg),
            BasisNetworkCommons::P2P_SUB_ACCEPT => self.on_inbound_accept(server, msg),
            BasisNetworkCommons::P2P_SUB_OFFLOADED => {
                if let Some(session) = self.by_token.get(&msg.session_token).map(|s| s.clone()) {
                    session.offloaded.store(true, std::sync::atomic::Ordering::Release);
                    session.punching.store(false, std::sync::atomic::Ordering::Release);
                    session.confirm();
                }
            }
            BasisNetworkCommons::P2P_SUB_DECLINE | BasisNetworkCommons::P2P_SUB_CANCEL | BasisNetworkCommons::P2P_SUB_LINK_LOST => {
                self.drop_session(&msg.session_token);
            }
            // ServerArmed only confirms the session is registered.
            _ => {}
        }
    }

    fn on_inbound_request(&self, server: &NetPeerRef, msg: BasisP2PSignalMessage) {
        // The server rewrites other_player_id to the sender's id on the way out. A hello client
        // accepts everyone; a real one asks its user first.
        let initiator = msg.other_player_id;
        if msg.session_token.is_empty() {
            return;
        }
        let session = Arc::new(DirectSession::new(msg.session_token.clone(), initiator));
        if !self.register(&session) {
            return;
        }
        if !self.derive_keys(&session, msg.ephemeral_public_key.as_deref()) {
            return;
        }
        if let Err(e) = self.send_signal(server, BasisNetworkCommons::P2P_SUB_ACCEPT, initiator, &session.token, Some(session.local_public.clone())) {
            BNL::log_error(format!("{} could not accept a direct link: {e}", self.display_name()));
            return;
        }
        self.begin_punching(server, &session);
    }

    fn on_inbound_accept(&self, server: &NetPeerRef, msg: BasisP2PSignalMessage) {
        let Some(session) = self.by_token.get(&msg.session_token).map(|s| s.clone()) else {
            return;
        };
        if !self.derive_keys(&session, msg.ephemeral_public_key.as_deref()) {
            return;
        }
        self.begin_punching(server, &session);
    }

    /// Hands the server this endpoint's address so it can introduce the pair.
    fn begin_punching(&self, server: &NetPeerRef, session: &Arc<DirectSession>) {
        session.punching.store(true, std::sync::atomic::Ordering::Release);
        let Some(endpoint_addr) = self.endpoint_addr_bytes() else {
            BNL::log_error(format!("{} has no endpoint address to be introduced with.", self.display_name()));
            return;
        };
        let mut request = BasisP2PIntroduceRequest { session_token: session.token.clone(), endpoint_addr };
        let mut writer = NetDataWriter::new();
        writer.put_byte(BasisNetworkCommons::P2P_SUB_INTRODUCE_REQUEST);
        if request.serialize(&mut writer).is_ok()
            && let Err(e) = server.send_writer(&writer, BasisNetworkCommons::P2P_CHANNEL, DeliveryMethod::ReliableOrdered)
        {
            BNL::log_error(format!("{} could not ask for an introduction to player {}: {e}", self.display_name(), session.other_player_id));
        }
    }

    /// The server's introduction: the other side's address, and whether this side dials.
    fn on_introduce(&self, msg: BasisP2PIntroduce) {
        let Some(session) = self.by_token.get(&msg.session_token).map(|s| s.clone()) else {
            return;
        };
        if !msg.dial || session.dialed.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        let Ok(addr) = serde_json::from_slice::<iroh::EndpointAddr>(&msg.endpoint_addr) else {
            BNL::log_error(format!("{} got an unreadable endpoint address for player {}.", self.display_name(), session.other_player_id));
            return;
        };
        let target = Self::connection_string(&addr);
        let Some(manager) = self.base.network_client().and_then(|c| c.client()) else {
            return;
        };
        let mut connect_data = NetDataWriter::new();
        if connect_data.put_string(&session.token).is_err() {
            return;
        }
        match manager.connect(&target, 0, &connect_data) {
            Ok(peer) => {
                *session.peer.lock() = Some(peer);
            }
            Err(e) => {
                BNL::log_error(format!("{} could not dial player {}: {e}", self.display_name(), session.other_player_id));
                session.dialed.store(false, std::sync::atomic::Ordering::Release);
            }
        }
    }

    /// `<endpoint-id>[@host:port]` for the transport's connect parser, preferring a direct address.
    pub fn connection_string(addr: &iroh::EndpointAddr) -> String {
        let id = addr.id.to_z32();
        match addr.ip_addrs().next() {
            Some(socket) => format!("{id}@{socket}"),
            None => id,
        }
    }

    fn send_signal(&self, server: &NetPeerRef, sub: u8, other_player_id: u16, token: &str, ephemeral_public_key: Option<Vec<u8>>) -> BasisResult<()> {
        let mut msg = BasisP2PSignalMessage { other_player_id, session_token: token.to_string(), ephemeral_public_key };
        let mut writer = NetDataWriter::new();
        writer.put_byte(sub);
        msg.serialize(&mut writer)?;
        server.send_writer(&writer, BasisNetworkCommons::P2P_CHANNEL, DeliveryMethod::ReliableOrdered)?;
        Ok(())
    }

    fn register(&self, session: &Arc<DirectSession>) -> bool {
        match self.by_player.entry(session.other_player_id) {
            dashmap::mapref::entry::Entry::Occupied(_) => false,
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(session.clone());
                self.by_token.insert(session.token.clone(), session.clone());
                true
            }
        }
    }

    fn derive_keys(&self, session: &DirectSession, remote_public_key: Option<&[u8]>) -> bool {
        let Some(remote) = remote_public_key.filter(|k| k.len() == BasisP2PSignalMessage::PUBLIC_KEY_SIZE) else {
            BNL::log_error(format!("{} got no usable ephemeral key for player {}.", self.display_name(), session.other_player_id));
            return false;
        };
        match BasisCryptoHandshake::derive_peer_keys(&session.local_private, &session.local_public, remote) {
            Ok(keys) => {
                *session.keys.lock() = Some(keys);
                true
            }
            Err(e) => {
                BNL::log_error(format!("{} could not derive direct-link keys for player {}: {e}", self.display_name(), session.other_player_id));
                false
            }
        }
    }

    fn session_for_peer(&self, peer: &NetPeerRef) -> Option<Arc<DirectSession>> {
        self.by_token.iter().find(|s| s.peer.lock().as_ref().is_some_and(|p| peers_equal(p, peer))).map(|s| s.clone())
    }

    fn drop_session(&self, token: &str) {
        if token.is_empty() {
            return;
        }
        let Some((_, session)) = self.by_token.remove(token) else {
            return;
        };
        self.by_player.remove_if(&session.other_player_id, |_, held| Arc::ptr_eq(held, &session));
        session.punching.store(false, std::sync::atomic::Ordering::Release);
        session.offloaded.store(false, std::sync::atomic::Ordering::Release);
        if let Some(peer) = session.peer.lock().take() {
            peer.disconnect();
        }
        session.confirm();
    }
}

impl HelloExtension for HelloPeerClient {
    fn handle_other_channel(&self, peer: &NetPeerRef, reader: &mut NetDataReader, channel: u8) -> bool {
        match channel {
            BasisNetworkCommons::P2P_CHANNEL => {
                self.handle_signal(peer, reader);
                true
            }
            // A direct-origin message the server had to relay after all: still a relayed message.
            BasisNetworkCommons::DIRECT_SCENE_SERVER_CHANNEL => {
                self.base.handle_relayed_scene(reader);
                true
            }
            _ => false,
        }
    }

    fn handle_peer_message(&self, peer: &NetPeerRef, reader: &mut NetDataReader, channel: u8) -> bool {
        if channel != BasisNetworkCommons::DIRECT_SCENE_CHANNEL {
            return false;
        }
        // The connection identifies the sender: a direct link has exactly one peer on it.
        let Some(session) = self.session_for_peer(peer) else {
            return false;
        };
        let Ok(message_index) = reader.get_ushort() else {
            return true;
        };
        if message_index != BasisHelloClient::HELLO_MESSAGE_INDEX {
            return true;
        }
        let payload = reader.get_remaining_bytes();
        self.base.raise_payload(session.other_player_id, &payload, HelloTransport::DirectLink);
        true
    }

    fn on_connection_request(&self, request: Arc<dyn ConnectionRequest>) {
        let token = request.data().get_string_max(BasisP2PSignalMessage::MAX_TOKEN_LENGTH).unwrap_or_default();
        // Only a peer naming a token we are punching for gets in.
        let Some(session) = self.by_token.get(&token).map(|s| s.clone()).filter(|s| s.punching.load(std::sync::atomic::Ordering::Acquire)) else {
            let _ = request.reject(&NetDataWriter::new());
            return;
        };
        match request.accept() {
            Ok(peer) => {
                *session.peer.lock() = Some(peer);
                self.report_link_up(&session);
            }
            Err(e) => BNL::log_error(format!("{} could not accept a direct link: {e}", self.display_name())),
        }
    }

    fn on_peer_connected(&self, peer: &NetPeerRef) {
        if let Some(session) = self.session_for_peer(peer) {
            self.report_link_up(&session);
        }
    }

    fn on_disconnect(&self) {
        let tokens: Vec<String> = self.by_token.iter().map(|s| s.key().clone()).collect();
        for token in tokens {
            self.drop_session(&token);
        }
    }
}

impl HelloPeerClient {
    fn report_link_up(&self, session: &Arc<DirectSession>) {
        if let Some(server) = self.base.server_peer()
            && let Err(e) = self.send_signal(&server, BasisNetworkCommons::P2P_SUB_LINK_UP, session.other_player_id, &session.token, None)
        {
            BNL::log_error(format!("{} could not report its direct link: {e}", self.display_name()));
        }
    }
}
