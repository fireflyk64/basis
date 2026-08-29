//! Port of `ClientManager.cs` and `ConsoleClientIdentity`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use basis_crypto::{Ed25519, Payload, PrivKey};
use basis_error::BasisResult;
use basis_network_client::{BasisDIDAuthIdentityClient, NetworkClient};
use basis_network_core::BNL;
use basis_network_core::SerializableBasis::{BytesMessage, ClientAvatarChangeMessage, ClientMetaDataMessage, LocalAvatarSyncMessage, ReadyMessage};
use basis_network_core::compression::{BasisAvatarBitPacking, BitQuality};
use basis_network_core::configuration::Configuration;
use basis_network_core::transport::basis_network_shell::SubscriptionId;
use basis_network_core::{NetDataReader, NetDataWriter, NetPeerRef};
use parking_lot::RwLock;
use rand::{RngExt, SeedableRng};

use crate::avatar::avatar_key_store_loader::AvatarKeyStoreLoader;
use crate::avatar::basis_avatar_network_load::BasisAvatarNetworkLoad;
use crate::client::config_manager::ConfigManager;
use crate::client::message_handler::MessageHandler;
use crate::client::movement_sender::MovementSender;
use crate::util::name_generator::NameGenerator;

/// `ClientManager.Size`: the High-quality pose payload length.
static SIZE: AtomicUsize = AtomicUsize::new(0);

/// One simulated player's transport, connection and identity — the C# `FinalClients[i]` /
/// `FinalPeers[i]` pair, readable from every driver thread.
#[derive(Default)]
pub struct ClientSlot {
    client: RwLock<Option<Arc<NetworkClient>>>,
    peer: RwLock<Option<NetPeerRef>>,
    identity: RwLock<Option<Arc<ConsoleClientIdentity>>>,
    disconnect_subscription: RwLock<Option<SubscriptionId>>,
}

impl ClientSlot {
    pub fn client(&self) -> Option<Arc<NetworkClient>> {
        self.client.read().clone()
    }

    pub fn peer(&self) -> Option<NetPeerRef> {
        self.peer.read().clone()
    }

    pub fn identity(&self) -> Option<Arc<ConsoleClientIdentity>> {
        self.identity.read().clone()
    }

    /// `(peer.Tag as ConsoleClientIdentity)?.Authenticated == true`.
    pub fn is_authenticated(&self) -> bool {
        self.identity.read().as_ref().is_some_and(|i| i.is_authenticated())
    }

    fn install(&self, client: Arc<NetworkClient>, peer: NetPeerRef, identity: Arc<ConsoleClientIdentity>, subscription: Option<SubscriptionId>) {
        *self.client.write() = Some(client);
        *self.peer.write() = Some(peer);
        *self.identity.write() = Some(identity);
        *self.disconnect_subscription.write() = subscription;
    }
}

pub struct ClientManager {
    slots: Arc<Vec<ClientSlot>>,
    cancelled: AtomicBool,
    // Cached once — config doesn't change at runtime
    cached_password_bytes: Vec<u8>,
    cached_avatar_bytes: Vec<u8>,
    avatar_pool: Vec<(Vec<u8>, u8)>,
}

impl Default for ClientManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientManager {
    pub fn new() -> Self {
        Self { slots: Arc::new(Vec::new()), cancelled: AtomicBool::new(false), cached_password_bytes: Vec::new(), cached_avatar_bytes: Vec::new(), avatar_pool: Vec::new() }
    }

    pub fn client_count(&self) -> usize {
        ConfigManager::current().client_count.max(0) as usize
    }

    pub fn size() -> usize {
        SIZE.load(Ordering::Relaxed)
    }

    pub fn slots(&self) -> Arc<Vec<ClientSlot>> {
        self.slots.clone()
    }

    pub fn prepare(&mut self) {
        let size = BasisAvatarBitPacking::convert_to_size(BitQuality::High);
        SIZE.store(size, Ordering::Relaxed);
        BNL::log(format!("Payload Size for muscles is now {size}"));

        let config = ConfigManager::current();
        self.cached_password_bytes = config.password.as_bytes().to_vec();
        let avatar_info = BasisAvatarNetworkLoad { url: config.avatar_url.clone(), unlock_password: config.avatar_password.clone(), version_tag: String::new() };
        self.cached_avatar_bytes = avatar_info.encode_to_bytes();

        self.build_avatar_pool();

        let count = self.client_count();
        self.slots = Arc::new((0..count).map(|_| ClientSlot::default()).collect());
    }

    // Platform and body fit both ride the per-player metadata the server stores and replays to
    // every joiner, so 2000 identical simulated clients under-report what a real crowd costs on
    // the wire. Distribution is a rough desktop/standalone-VR split.
    const SIMULATED_PLATFORMS: [&'static str; 10] = ["WindowsPlayer", "WindowsPlayer", "WindowsPlayer", "WindowsPlayer", "Android", "Android", "Android", "LinuxPlayer", "OSXPlayer", "WindowsEditor"];

    fn platform_for_client(client_index: usize) -> String {
        if !ConfigManager::current().simulate_realistic_platforms {
            return "Headless".to_string();
        }
        Self::SIMULATED_PLATFORMS[client_index % Self::SIMULATED_PLATFORMS.len()].to_string()
    }

    /// Per-client body-fit scales inside the band BasisBodyFitCore can actually produce
    /// (1 +/- maxDeviation, ceiling 0.5). Leg and torso are opposed, mirroring the real solver's
    /// height-neutral construction.
    fn body_fit_for_client(client_index: usize) -> (f32, f32, f32) {
        if !ConfigManager::current().simulate_body_fit {
            return (1.0, 1.0, 1.0);
        }
        let seed = (client_index as i32).wrapping_mul(8663) ^ 0x5eed;
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed as u32 as u64);
        let arm = 1.0 + (rng.random::<f64>() * 0.30 - 0.15) as f32;
        let shift = (rng.random::<f64>() * 0.24 - 0.12) as f32;
        (arm, 1.0 + shift, 1.0 - shift)
    }

    fn build_avatar_pool(&mut self) {
        let config = ConfigManager::current();
        if !config.use_random_avatar_from_key_store {
            return;
        }
        let avatars = AvatarKeyStoreLoader::load(&config.avatar_key_store_path, config.avatar_load_mode as u8);
        if avatars.is_empty() {
            BNL::log_warning("UseRandomAvatarFromKeyStore is on but no avatars were found; falling back to the configured AvatarUrl.");
            return;
        }
        self.avatar_pool = avatars
            .iter()
            .enumerate()
            .map(|(i, entry)| {
                let info = BasisAvatarNetworkLoad { url: entry.url.clone(), unlock_password: entry.password.clone(), version_tag: String::new() };
                BNL::log(format!("Avatar pool [{i}] loadMode={} url={}", entry.load_mode, entry.url));
                (info.encode_to_bytes(), entry.load_mode)
            })
            .collect();
        BNL::log(format!("Loaded {} avatar(s) from keystore for random assignment.", self.avatar_pool.len()));
    }

    fn pick_avatar(&self) -> (Vec<u8>, u8) {
        if !self.avatar_pool.is_empty() {
            let pick = rand::rng().random_range(0..self.avatar_pool.len());
            return self.avatar_pool[pick].clone();
        }
        (self.cached_avatar_bytes.clone(), ConfigManager::current().avatar_load_mode as u8)
    }

    pub fn start_clients(&self) {
        let interval = ConfigManager::current().client_connect_interval_ms;
        for index in 0..self.slots.len() {
            if self.cancelled.load(Ordering::Acquire) {
                return;
            }
            match self.connect(index, 0) {
                Ok((name, did)) => BNL::log(format!("Connecting: {name} ({did})")),
                Err(e) => BNL::log_error(format!("Client {index} could not connect: {}", e.report())),
            }
            if interval > 0 {
                std::thread::sleep(Duration::from_millis(interval as u64));
            }
        }
    }

    /// Builds one client and points it at the server. Returns the name and DID it joined under.
    fn connect(&self, index: usize, local_avatar_index: u8) -> BasisResult<(String, String)> {
        let name = NameGenerator::generate_random_player_name();
        let identity = Arc::new(ConsoleClientIdentity::new()?);
        let (avatar_bytes, avatar_load_mode) = self.pick_avatar();
        let (arm_scale, leg_scale, torso_scale) = Self::body_fit_for_client(index);

        let mut ready_message = ReadyMessage {
            player_meta_data_message: ClientMetaDataMessage { player_display_name: name.clone(), player_uuid: identity.did.clone(), player_platform: Self::platform_for_client(index) },
            client_avatar_change_message: ClientAvatarChangeMessage {
                byte_array: Some(avatar_bytes),
                load_mode: avatar_load_mode,
                local_avatar_index,
                arm_scale,
                leg_scale,
                torso_scale,
            },
            local_avatar_sync_message: LocalAvatarSyncMessage {
                array: MovementSender::generate(Some(index)).message.array,
                additional_avatar_data_size: 0,
                linked_avatar_index: 0,
                data_quality_level: BitQuality::High as u8,
                additional_avatar_datas: None,
            },
        };

        let config = ConfigManager::current();
        let net_client = Arc::new(NetworkClient::new());
        let peer = net_client.start_client(&config.ip, config.port.clamp(0, u16::MAX as i32) as u16, &mut ready_message, &self.cached_password_bytes, &Self::create_config())?;
        peer.set_tag(Some(identity.clone()));

        let mut subscription = None;
        if let Some(listener) = net_client.listener() {
            let receive_identity = identity.clone();
            listener.network_receive_event.subscribe(Arc::new(move |p, r, ch, m| MessageHandler::on_receive(&receive_identity, index, &p, r, ch, m)));
            subscription = Some(listener.peer_disconnected_event.subscribe(Arc::new(|p, info| MessageHandler::on_disconnect(&p, &info))));
        }
        self.slots[index].install(net_client, peer, identity.clone(), subscription);
        Ok((name, identity.did.clone()))
    }

    pub fn reconnect_client(&self, index: usize) {
        let Some(slot) = self.slots.get(index) else {
            return;
        };
        if let Some(old_client) = slot.client() {
            if let (Some(listener), Some(subscription)) = (old_client.listener(), slot.disconnect_subscription.write().take()) {
                listener.peer_disconnected_event.unsubscribe(subscription);
            }
            old_client.disconnect();
        }
        BNL::log(format!("Disconnected client at index {index}"));

        std::thread::sleep(Duration::from_secs(3)); // wait before reconnecting

        match self.connect(index, 1) {
            Ok((name, did)) => {
                // Fresh server session — the old delta baseline is meaningless to it, so the
                // first send after a reconnect must be a full keyframe.
                MovementSender::request_keyframe(index);
                BNL::log(format!("Reconnected: {name} ({did}) at index {index}"));
            }
            Err(e) => BNL::log_error(format!("Client {index} could not reconnect: {}", e.report())),
        }
    }

    /// Leaves the server cleanly, then tears down.
    ///
    /// The two passes are the point. Departure notices are one datagram each and go out in a few
    /// milliseconds for the whole population; the teardown behind them takes far longer than the
    /// shutdown budget a killed process gets. Nothing is logged per client here for the same
    /// reason.
    pub fn stop_clients(&self) {
        self.cancelled.store(true, Ordering::Release);
        let mut announced = 0;
        for slot in self.slots.iter() {
            if let Some(client) = slot.client() {
                client.notify_server_of_departure();
                announced += 1;
            }
        }
        BNL::log(format!("Told the server {announced} client(s) are leaving; tearing down."));
        for slot in self.slots.iter() {
            if let Some(client) = slot.client() {
                client.shutdown();
            }
        }
    }

    pub fn create_config() -> Configuration {
        Configuration { use_auth_identity: true, enable_statistics: false, has_file_support: false, set_port: 0, ..Configuration::default() }
    }
}

pub struct ConsoleClientIdentity {
    private_key: PrivKey,
    authenticated: AtomicBool,
    pub did: String,
}

impl ConsoleClientIdentity {
    pub fn new() -> BasisResult<Self> {
        let ((_, private_key), did) = BasisDIDAuthIdentityClient::client_key_creation()?;
        Ok(Self { private_key, authenticated: AtomicBool::new(false), did: did.v().to_string() })
    }

    pub fn is_authenticated(&self) -> bool {
        self.authenticated.load(Ordering::Acquire)
    }

    pub fn set_authenticated(&self, value: bool) {
        self.authenticated.store(value, Ordering::Release);
    }

    pub fn try_respond_to_challenge(&self, reader: &mut NetDataReader) -> Option<NetDataWriter> {
        let Some(nonce) = BytesMessage.deserialize(reader) else {
            BNL::log_error("Malformed auth challenge from server");
            return None;
        };
        let Some(signature) = Ed25519::sign(&self.private_key, &Payload::new(nonce)) else {
            BNL::log_error("Unable to sign auth challenge");
            return None;
        };
        let mut writer = NetDataWriter::new();
        BytesMessage.serialize(&mut writer, signature.v()).ok()?;
        BytesMessage.serialize(&mut writer, b"N/A").ok()?;
        Some(writer)
    }
}
