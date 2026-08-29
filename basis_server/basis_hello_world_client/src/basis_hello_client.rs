//! Port of `BasisHelloClient.cs`.
//!
//! Connect, prove who you are, learn your own player id, then send numbers and strings to
//! another player by id. Messages ride the scene channel, which the server relays verbatim to
//! the recipients named in the message and stamps with the sender's player id.
//!
//! Wire format of the payload, one byte of tag and then the body: `[kind:1][int32 LE]` for a
//! number, `[kind:1][utf8...]` for text.
//!
//! Thread model: the iroh transport delivers events from its own runtime, so the handlers fire
//! on transport threads; the send methods may be called from any thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use basis_crypto::{Ed25519, Payload};
use basis_error::{BasisError, BasisResult, ErrorCode};
use basis_network_client::{BasisDIDAuthIdentityClient, ClientIdentity, NetworkClient};
use basis_network_core::SerializableBasis::{
    BytesMessage, ClientAvatarChangeMessage, ClientMetaDataMessage, LocalAvatarSyncMessage, ReadyMessage, SceneDataMessage,
    ServerSceneDataMessage,
};
use basis_network_core::compression::{BasisAvatarBitPacking, BitQuality};
use basis_network_core::configuration::Configuration;
use basis_network_core::transport::basis_network_shell::ConnectionRequest;
use basis_network_core::transport::basis_network_stack_registry::BasisNetworkStackRegistry;
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetDataReader, NetDataWriter, NetPeerRef};
use parking_lot::{Condvar, Mutex, RwLock};

/// How a hello message reached its recipient.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelloTransport {
    /// Relayed by the server, which stamped it with the sender's player id.
    ServerRelay,
    /// Carried over a direct peer-to-peer link, never touching the server.
    DirectLink,
}

impl std::fmt::Display for HelloTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            HelloTransport::ServerRelay => "ServerRelay",
            HelloTransport::DirectLink => "DirectLink",
        })
    }
}

pub type NumberHandler = Arc<dyn Fn(u16, i32, HelloTransport) + Send + Sync>;
pub type TextHandler = Arc<dyn Fn(u16, String, HelloTransport) + Send + Sync>;

/// A subclass hook: the C# `HelloPeerClient` overrode the base client's virtuals; here it
/// installs itself as the base client's extension.
pub trait HelloExtension: Send + Sync {
    /// A message on a channel the base client does not know. Return true when handled.
    fn handle_other_channel(&self, peer: &NetPeerRef, reader: &mut NetDataReader, channel: u8) -> bool;
    /// A message that arrived from a peer that is not the server connection.
    fn handle_peer_message(&self, peer: &NetPeerRef, reader: &mut NetDataReader, channel: u8) -> bool;
    fn on_connection_request(&self, request: Arc<dyn ConnectionRequest>);
    fn on_peer_connected(&self, peer: &NetPeerRef);
    fn on_disconnect(&self);
}

struct Joined {
    flag: Mutex<bool>,
    signal: Condvar,
}

impl Joined {
    fn set(&self) {
        *self.flag.lock() = true;
        self.signal.notify_all();
    }

    fn is_set(&self) -> bool {
        *self.flag.lock()
    }

    fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut flag = self.flag.lock();
        while !*flag {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            self.signal.wait_for(&mut flag, deadline - now);
        }
        true
    }
}

pub struct BasisHelloClient {
    display_name: String,
    /// The transport this client speaks: iroh (the default) or `litenetlib`, the protocol the
    /// legacy C# clients use — the same server admits both.
    network_stack_id: String,
    identity: ClientIdentity,
    avatar_bytes: Vec<u8>,
    joined: Joined,
    client: RwLock<Option<Arc<NetworkClient>>>,
    peer: RwLock<Option<NetPeerRef>>,
    server_target: RwLock<(String, u16)>,
    number_handlers: RwLock<Vec<NumberHandler>>,
    text_handlers: RwLock<Vec<TextHandler>>,
    extension: RwLock<Option<Arc<dyn HelloExtension>>>,
    self_ref: std::sync::Weak<BasisHelloClient>,
}

impl BasisHelloClient {
    /// Identifies this app's traffic on the shared scene channel. A real deployment gets a
    /// network id from the server's id database; a hello-world picks a constant both ends agree on.
    pub const HELLO_MESSAGE_INDEX: u16 = 0xE0C0;
    const KIND_NUMBER: u8 = 0;
    const KIND_TEXT: u8 = 1;

    /// A client with a freshly generated did:key identity, on the iroh stack.
    pub fn new(display_name: &str) -> BasisResult<Arc<Self>> {
        Self::with_stack(display_name, BasisNetworkStackRegistry::IROH_ID)
    }

    /// A client on a named stack: `iroh`, or `litenetlib` to join as a legacy client would.
    pub fn with_stack(display_name: &str, network_stack_id: &str) -> BasisResult<Arc<Self>> {
        if !BasisNetworkStackRegistry::is_registered(network_stack_id) {
            return Err(BasisError::permanent(ErrorCode::InvalidArgument, format!("'{network_stack_id}' is not a registered network stack")));
        }
        let identity = ClientIdentity::generate()?;
        Ok(Arc::new_cyclic(|weak| Self {
            display_name: display_name.to_string(),
            network_stack_id: network_stack_id.to_string(),
            identity,
            // The server stores this blob and replays it to other players without ever decoding
            // it. It has to be non-empty, or the ready message fails validation and the join is
            // refused.
            avatar_bytes: b"basis-hello-world-no-avatar".to_vec(),
            joined: Joined { flag: Mutex::new(false), signal: Condvar::new() },
            client: RwLock::new(None),
            peer: RwLock::new(None),
            server_target: RwLock::new((String::new(), 0)),
            number_handlers: RwLock::new(Vec::new()),
            text_handlers: RwLock::new(Vec::new()),
            extension: RwLock::new(None),
            self_ref: weak.clone(),
        }))
    }

    /// Name this client shows up under in the server's player list.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// The stack this client was created on.
    pub fn network_stack_id(&self) -> &str {
        &self.network_stack_id
    }

    /// This client's did:key identity.
    pub fn did(&self) -> &str {
        self.identity.did.v()
    }

    /// The id the server knows this client by, and the one other clients address it with. Only
    /// meaningful once joined.
    pub fn player_id(&self) -> u16 {
        self.peer.read().as_ref().map(|p| p.remote_id() as u16).unwrap_or(0)
    }

    /// True once the server has accepted the identity challenge and sent our metadata.
    pub fn is_joined(&self) -> bool {
        self.joined.is_set()
    }

    pub fn on_number_received(&self, handler: NumberHandler) {
        self.number_handlers.write().push(handler);
    }

    pub fn on_text_received(&self, handler: TextHandler) {
        self.text_handlers.write().push(handler);
    }

    pub fn set_extension(&self, extension: Option<Arc<dyn HelloExtension>>) {
        *self.extension.write() = extension;
    }

    /// The connection to the server, for an extension that needs to signal on it.
    pub fn server_peer(&self) -> Option<NetPeerRef> {
        self.peer.read().clone()
    }

    /// The transport, for an extension that opens direct links on the same endpoint.
    pub fn network_client(&self) -> Option<Arc<NetworkClient>> {
        self.client.read().clone()
    }

    /// `(host, port)` the server was connected at.
    pub fn server_target(&self) -> (String, u16) {
        self.server_target.read().clone()
    }

    fn ready_message(&self) -> ReadyMessage {
        ReadyMessage {
            player_meta_data_message: ClientMetaDataMessage {
                player_display_name: self.display_name.clone(),
                player_uuid: self.did().to_string(),
                player_platform: "Headless".to_string(),
            },
            client_avatar_change_message: ClientAvatarChangeMessage {
                byte_array: Some(self.avatar_bytes.clone()),
                load_mode: 0,
                local_avatar_index: 0,
                arm_scale: 1.0,
                leg_scale: 1.0,
                torso_scale: 1.0,
            },
            local_avatar_sync_message: LocalAvatarSyncMessage {
                // A pose of all zeros: the server only cares that the payload is exactly the
                // length its quality level declares.
                array: Some(vec![0u8; BasisAvatarBitPacking::convert_to_size(BitQuality::High)]),
                data_quality_level: BitQuality::High as u8,
                additional_avatar_data_size: 0,
                additional_avatar_datas: None,
                linked_avatar_index: 0,
            },
        }
    }

    /// Connects, authenticates, and waits until the server has admitted this client.
    /// `Ok(false)` when that has not happened within `timeout`; an error when the transport
    /// could not even start. One use per instance: reconnecting means a new client.
    pub fn connect(&self, target: &str, port: u16, password: &str, timeout: Duration) -> BasisResult<bool> {
        if self.client.read().is_some() {
            return Err(BasisError::permanent(ErrorCode::Conflict, format!("{} has already connected; construct a new client.", self.display_name)));
        }
        *self.server_target.write() = (target.to_string(), port);
        let client = Arc::new(NetworkClient::new());
        let mut ready = self.ready_message();
        let peer = client.start_client(target, port, &mut ready, password.as_bytes(), &self.create_configuration())?;
        let Some(listener) = client.listener() else {
            client.shutdown();
            return Err(BasisError::permanent(ErrorCode::Internal, "the client transport has no listener"));
        };
        let weak = self.self_ref.clone();
        listener.network_receive_event.subscribe(Arc::new(move |peer, reader, channel, _dm| {
            if let Some(this) = weak.upgrade() {
                this.on_receive(&peer, reader, channel);
            }
        }));
        let weak = self.self_ref.clone();
        listener.connection_request_event.subscribe(Arc::new(move |request| {
            if let Some(this) = weak.upgrade()
                && let Some(extension) = this.extension.read().clone()
            {
                extension.on_connection_request(request);
            }
        }));
        let weak = self.self_ref.clone();
        listener.peer_connected_event.subscribe(Arc::new(move |peer| {
            if let Some(this) = weak.upgrade()
                && let Some(extension) = this.extension.read().clone()
            {
                extension.on_peer_connected(&peer);
            }
        }));
        *self.peer.write() = Some(peer);
        *self.client.write() = Some(client);
        Ok(self.joined.wait(timeout))
    }

    /// Sends one number to one player. Reliable and ordered, so a volley cannot overtake itself.
    pub fn send_number(&self, target_player_id: u16, value: i32) -> BasisResult<()> {
        self.send(target_player_id, &Self::encode_number(value))
    }

    /// Sends one string to one player.
    pub fn send_text(&self, target_player_id: u16, text: &str) -> BasisResult<()> {
        self.send(target_player_id, &Self::encode_text(text))
    }

    pub fn encode_number(value: i32) -> Vec<u8> {
        let mut payload = vec![Self::KIND_NUMBER];
        payload.extend_from_slice(&value.to_le_bytes());
        payload
    }

    pub fn encode_text(text: &str) -> Vec<u8> {
        let mut payload = vec![Self::KIND_TEXT];
        payload.extend_from_slice(text.as_bytes());
        payload
    }

    fn send(&self, target_player_id: u16, payload: &[u8]) -> BasisResult<()> {
        let peer = self.server_peer().filter(|_| self.is_joined()).ok_or_else(|| Self::not_joined(&self.display_name))?;
        Self::send_via(&peer, target_player_id, payload, BasisNetworkCommons::SCENE_CHANNEL)
    }

    pub(crate) fn not_joined(display_name: &str) -> BasisError {
        BasisError::permanent(ErrorCode::Conflict, format!("{display_name} has not joined a server yet."))
    }

    /// Puts one payload on a server relay channel addressed to one player. The channel is a
    /// parameter because the server runs the same relay for the plain scene channel and for the
    /// direct-origin fallback channel.
    pub fn send_via(peer: &NetPeerRef, target_player_id: u16, payload: &[u8], channel: u8) -> BasisResult<()> {
        let mut message = SceneDataMessage {
            message_index: Self::HELLO_MESSAGE_INDEX,
            // A non-empty recipient list is what makes this a direct message: the server relays
            // to exactly these player ids. Leaving it empty would broadcast to the whole room.
            recipients_size: 1,
            recipients: Some(vec![target_player_id]),
            payload: Some(payload.to_vec()),
        };
        let mut writer = NetDataWriter::new();
        message.serialize(&mut writer)?;
        peer.send_writer(&writer, channel, DeliveryMethod::ReliableOrdered)?;
        Ok(())
    }

    /// Tells the server we are leaving, then closes the socket. Idempotent.
    pub fn disconnect(&self) {
        let Some(client) = self.client.write().take() else {
            return;
        };
        if let Some(extension) = self.extension.read().clone() {
            extension.on_disconnect();
        }
        client.disconnect();
        *self.peer.write() = None;
    }

    fn on_receive(&self, peer: &NetPeerRef, mut reader: NetDataReader, channel: u8) {
        let is_server = self.server_peer().is_some_and(|server| basis_network_core::transport::basis_network_shell::peers_equal(&server, peer));
        if !is_server {
            if let Some(extension) = self.extension.read().clone() {
                extension.handle_peer_message(peer, &mut reader, channel);
            }
            return;
        }
        match channel {
            BasisNetworkCommons::AUTH_IDENTITY_CHANNEL => self.respond_to_challenge(peer, &mut reader),
            // The server only sends this once it has admitted us, which makes it the signal that
            // the connection is usable — and the accept has already populated the player id.
            BasisNetworkCommons::META_DATA_CHANNEL => self.joined.set(),
            BasisNetworkCommons::SCENE_CHANNEL => self.handle_relayed_scene(&mut reader),
            _ => {
                if let Some(extension) = self.extension.read().clone() {
                    extension.handle_other_channel(peer, &mut reader, channel);
                }
            }
        }
    }

    fn respond_to_challenge(&self, peer: &NetPeerRef, reader: &mut NetDataReader) {
        let Some(nonce) = BytesMessage.deserialize(reader) else {
            BNL::log_error(format!("{} received a malformed auth challenge.", self.display_name));
            return;
        };
        let signed = Ed25519::sign(&self.identity.private_key, &Payload::new(nonce)).ok_or(()).and_then(|signature| {
            let mut writer = NetDataWriter::new();
            BytesMessage.serialize(&mut writer, signature.v()).map_err(|_| ())?;
            // The fragment names which key in a multi-key DID answered; this client has one.
            BytesMessage.serialize(&mut writer, b"N/A").map_err(|_| ())?;
            Ok(writer)
        });
        match signed {
            Ok(writer) => {
                if let Err(e) = peer.send_writer(&writer, BasisNetworkCommons::AUTH_IDENTITY_CHANNEL, DeliveryMethod::ReliableOrdered) {
                    BNL::log_error(format!("{} could not send its auth response: {e}", self.display_name));
                }
            }
            Err(()) => BNL::log_error(format!("{} could not sign the auth challenge.", self.display_name)),
        }
    }

    /// Reads one server-relayed scene message, which carries the sender's id in the frame.
    pub fn handle_relayed_scene(&self, reader: &mut NetDataReader) {
        let mut message = ServerSceneDataMessage::default();
        if message.deserialize(reader).is_err() || message.scene_data_message.message_index != Self::HELLO_MESSAGE_INDEX {
            return;
        }
        let payload = message.scene_data_message.payload.as_deref().unwrap_or(&[]);
        self.raise_payload(message.player_id_message.player_id, payload, HelloTransport::ServerRelay);
    }

    /// Turns one decoded payload into an event. Separate from the frame parsing because a direct
    /// link identifies its peer by the connection rather than by a sender id in the bytes.
    pub fn raise_payload(&self, sender: u16, payload: &[u8], transport: HelloTransport) {
        let Some((&kind, body)) = payload.split_first() else {
            return;
        };
        match kind {
            Self::KIND_NUMBER if body.len() >= 4 => {
                let value = i32::from_le_bytes([body[0], body[1], body[2], body[3]]);
                for handler in self.number_handlers.read().iter() {
                    handler(sender, value, transport);
                }
            }
            Self::KIND_TEXT => {
                let text = String::from_utf8_lossy(body).into_owned();
                for handler in self.text_handlers.read().iter() {
                    handler(sender, text.clone(), transport);
                }
            }
            _ => {}
        }
    }

    fn create_configuration(&self) -> Configuration {
        Configuration {
            network_stack_id: self.network_stack_id.clone(),
            use_auth_identity: true,
            // Nothing here reads the per-channel counters.
            enable_statistics: false,
            has_file_support: false,
            set_port: 0,
            ..Configuration::default()
        }
    }
}

impl Drop for BasisHelloClient {
    fn drop(&mut self) {
        let Some(client) = self.client.write().take() else {
            return;
        };
        client.disconnect();
    }
}

/// Keeps the identity store off disk for hello clients: each instance is a fresh identity.
pub fn fresh_identity() -> BasisResult<ClientIdentity> {
    BasisDIDAuthIdentityClient::client_key_creation().map(|((public_key, private_key), did)| ClientIdentity {
        public_key,
        private_key,
        did,
        did_url_fragment: basis_did::newtypes::DidUrlFragment::new(String::new()),
    })
}
