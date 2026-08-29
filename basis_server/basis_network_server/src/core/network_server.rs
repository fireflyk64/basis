//! Port of `Core/NetworkServer.cs`: the static hub every subsystem reaches for — the transport,
//! the listener, the authenticated peer table, the configuration snapshot, the writer pool and
//! the send helpers.

use std::any::Any;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};

use basis_error::{BasisError, BasisResult, ErrorCode, ResultExt};
use basis_network_core::compression::{BasisAvatarBitPacking, BasisAvatarBundleZstd, BitQuality};
use basis_network_core::configuration::{BasisTransportConfigStore, Configuration, LNLTransportConfig};
use basis_network_core::identity::BasisUserRestrictionMode;
use basis_network_core::statistics::basis_network_statistics::BasisNetworkStatistics;
use basis_network_core::transport::basis_network_shell::{NetDebug, NetManagerRef, peers_equal};
use basis_network_core::transport::basis_network_stack_registry::BasisNetworkStackRegistry;
use basis_network_core::{BNL, BasisCpuBudget, DeliveryMethod, EventBasedNetListener, NetDataWriter, NetPeerRef};
use crossbeam_queue::ArrayQueue;
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};

use crate::auth::{IAuth, IAuthIdentity, IAuthIdentitySupport, PasswordAuth};
use crate::core::basis_server_handle_events::BasisServerHandleEvents;
use crate::diagnostics::{BasisNetworkUdpDropMonitor, BasisServerLogger, BasisServerMemoryReclaim, BasisStatistics};
use crate::networking::BasisNetworkChat;
use crate::p2p::{BasisServerP2PBroker, IrohPeerIntroducer};
use crate::reduction::{BSRProfiler, BasisServerReductionSystemEvents};
use crate::security::{
    BasisAllowList, BasisAudioRangeLimitManager, BasisAvatarScaleLimitManager, BasisBanList, BasisCrashReportStateManager,
    BasisDIDAuthIdentity, BasisGlobalLockManager, BasisHeadlessConnectionPolicyManager, BasisOpusFrameDurationStateManager,
    BasisPlayerModeration, BasisResourceLimitManager, PermissionIntegration,
};

/// The marker stored in a peer's `Tag` once it has authenticated (the C# `AuthenticatedPeerTag`
/// object; identity here is the type, which only this module can construct).
pub struct AuthenticatedPeerTag(());

struct State {
    listener: RwLock<Option<Arc<EventBasedNetListener>>>,
    server: RwLock<Option<NetManagerRef>>,
    configuration: RwLock<Option<Arc<Configuration>>>,
    allow_list: RwLock<Option<Arc<BasisAllowList>>>,
    ban_list: RwLock<Option<Arc<BasisBanList>>>,
    auth: RwLock<Option<Arc<dyn IAuth>>>,
    auth_identity: RwLock<Option<Arc<dyn IAuthIdentity>>>,
    /// Cached snapshot rebuilt on connect/disconnect — avoids a collect on every broadcast.
    peer_snapshot: RwLock<Arc<[NetPeerRef]>>,
    /// Guards the read-then-publish: OnNetworkAccepted runs on parallel DID-auth continuations,
    /// so concurrent joins could otherwise lost-update the snapshot to a stale array.
    peer_snapshot_lock: Mutex<()>,
    authenticated_peers: DashMap<i32, NetPeerRef>,
    /// Centralized writer pool — single source of truth for all server code. Depth follows the
    /// machine: it absorbs writers borrowed concurrently, and how many that is scales with how
    /// many threads can be in flight.
    writer_pool: ArrayQueue<NetDataWriter>,
    high_quality_length: AtomicUsize,
    authenticated_peer_tag: Arc<AuthenticatedPeerTag>,
}

static STATE: LazyLock<State> = LazyLock::new(|| {
    let max_pooled_writers = usize::try_from(BasisCpuBudget::concurrency_width(4, 32, 2048)).unwrap_or(32);
    State {
        listener: RwLock::new(None),
        server: RwLock::new(None),
        configuration: RwLock::new(None),
        allow_list: RwLock::new(None),
        ban_list: RwLock::new(None),
        auth: RwLock::new(None),
        auth_identity: RwLock::new(None),
        peer_snapshot: RwLock::new(Arc::from(Vec::new())),
        peer_snapshot_lock: Mutex::new(()),
        authenticated_peers: DashMap::new(),
        writer_pool: ArrayQueue::new(max_pooled_writers),
        high_quality_length: AtomicUsize::new(0),
        authenticated_peer_tag: Arc::new(AuthenticatedPeerTag(())),
    }
});

pub struct NetworkServer;

impl NetworkServer {
    /// Reset() only rewinds the cursor; the backing array keeps its high-water size forever. One
    /// oversized serialization would otherwise park a permanently inflated writer in the pool.
    const MAX_POOLED_WRITER_CAPACITY: usize = 64 * 1024;
    /// The C# default `maxMessages` on every send helper.
    pub const DEFAULT_MAX_MESSAGES: i32 = 70;

    // ── Static fields ──────────────────────────────────────────────────────

    pub fn listener() -> Option<Arc<EventBasedNetListener>> {
        STATE.listener.read().clone()
    }

    pub fn server() -> Option<NetManagerRef> {
        STATE.server.read().clone()
    }

    /// The authenticated peer table, keyed by peer id.
    pub fn authenticated_peers() -> &'static DashMap<i32, NetPeerRef> {
        &STATE.authenticated_peers
    }

    /// The tag stored on authenticated peers.
    pub fn authenticated_peer_tag() -> Arc<dyn Any + Send + Sync> {
        STATE.authenticated_peer_tag.clone()
    }

    /// `ReferenceEquals(peer.Tag, AuthenticatedPeerTag)`.
    pub fn is_authenticated_peer(peer: &NetPeerRef) -> bool {
        peer.tag().is_some_and(|tag| tag.downcast_ref::<AuthenticatedPeerTag>().is_some())
    }

    /// The live configuration snapshot. `None` until [`start_server`](Self::start_server) or
    /// [`set_configuration`](Self::set_configuration) has run.
    pub fn configuration() -> Option<Arc<Configuration>> {
        STATE.configuration.read().clone()
    }

    /// The configuration, or the defaults when the server has not been configured yet.
    pub fn configuration_or_default() -> Arc<Configuration> {
        Self::configuration().unwrap_or_else(|| Arc::new(Configuration::default()))
    }

    pub fn set_configuration(configuration: Configuration) {
        *STATE.configuration.write() = Some(Arc::new(configuration));
    }

    /// Edits the live configuration (the C# assigned fields on the shared object directly).
    /// Readers keep the snapshot they hold; the next `configuration()` sees the edit.
    pub fn update_configuration(edit: impl FnOnce(&mut Configuration)) {
        let mut slot = STATE.configuration.write();
        let mut next = slot.as_deref().cloned().unwrap_or_default();
        edit(&mut next);
        *slot = Some(Arc::new(next));
    }

    pub fn clear_configuration() {
        *STATE.configuration.write() = None;
    }

    /// Allow-list consulted at `BasisServerHandleEvents::on_network_accepted` when the restriction
    /// mode is `AllowList`. File-backed under the config folder so admin mutations persist.
    pub fn allow_list() -> Option<Arc<BasisAllowList>> {
        STATE.allow_list.read().clone()
    }

    pub fn ban_list() -> Option<Arc<BasisBanList>> {
        STATE.ban_list.read().clone()
    }

    pub fn auth() -> Option<Arc<dyn IAuth>> {
        STATE.auth.read().clone()
    }

    pub fn set_auth(auth: Option<Arc<dyn IAuth>>) {
        *STATE.auth.write() = auth;
    }

    pub fn auth_identity() -> Option<Arc<dyn IAuthIdentity>> {
        STATE.auth_identity.read().clone()
    }

    pub fn set_auth_identity(identity: Option<Arc<dyn IAuthIdentity>>) {
        *STATE.auth_identity.write() = identity;
    }

    /// Installs (or clears) the transport shell. Production sets this from `start_server`; tests
    /// install a stand-in whose peer count they control.
    pub fn set_server(server: Option<NetManagerRef>) {
        *STATE.server.write() = server;
    }

    pub fn set_allow_list(list: Option<Arc<BasisAllowList>>) {
        *STATE.allow_list.write() = list;
    }

    pub fn set_ban_list(list: Option<Arc<BasisBanList>>) {
        *STATE.ban_list.write() = list;
    }

    pub fn set_high_quality_length(length: usize) {
        STATE.high_quality_length.store(length, Ordering::Relaxed);
    }

    pub fn high_quality_length() -> usize {
        STATE.high_quality_length.load(Ordering::Relaxed)
    }

    /// The C# `NetIDToUUID` through the auth identity: `None` when there is no identity or the
    /// peer is unknown to it.
    pub fn net_id_to_uuid(peer: &NetPeerRef) -> Option<String> {
        Self::auth_identity().and_then(|identity| identity.net_id_to_uuid(peer))
    }

    pub fn uuid_to_net_id(uuid: &str) -> Option<i32> {
        Self::auth_identity().and_then(|identity| identity.uuid_to_net_id(uuid))
    }

    pub fn connected_peers_count() -> i32 {
        Self::server().map(|s| s.connected_peers_count()).unwrap_or(0)
    }

    // ── Peer snapshot ──────────────────────────────────────────────────────

    pub fn peer_snapshot() -> Arc<[NetPeerRef]> {
        STATE.peer_snapshot.read().clone()
    }

    pub fn rebuild_peer_snapshot() {
        let _guard = STATE.peer_snapshot_lock.lock();
        let peers: Vec<NetPeerRef> = STATE.authenticated_peers.iter().map(|p| p.value().clone()).collect();
        *STATE.peer_snapshot.write() = Arc::from(peers);
    }

    /// The C# `((ICollection<KeyValuePair<int, NetPeer>>)AuthenticatedPeers).Remove(kvp)`:
    /// removes the entry only if it still holds `peer`.
    pub fn remove_authenticated_peer_if_same(id: i32, peer: &NetPeerRef) -> bool {
        STATE.authenticated_peers.remove_if(&id, |_, held| peers_equal(held, peer)).is_some()
    }

    // ── Writer pool ────────────────────────────────────────────────────────

    pub fn rent_writer() -> NetDataWriter {
        Self::rent_writer_with_capacity(208)
    }

    pub fn rent_writer_with_capacity(initial_capacity: usize) -> NetDataWriter {
        match STATE.writer_pool.pop() {
            Some(writer) => writer,
            None => NetDataWriter::with_capacity(initial_capacity),
        }
    }

    pub fn return_writer(mut writer: NetDataWriter) {
        writer.reset();
        if writer.capacity() <= Self::MAX_POOLED_WRITER_CAPACITY {
            // A full pool drops the writer: the allocator reclaims it, the pool stays bounded.
            let _ = STATE.writer_pool.push(writer);
        }
    }

    // ── Server entry point ─────────────────────────────────────────────────

    pub fn start_server(mut configuration: Configuration) -> BasisResult<()> {
        Self::stop_server();

        // Rejoin-only lockdown means "the players here right now" — meaningless after a restart,
        // and a persisted RejoinOnly would boot with an empty snapshot and lock everyone out.
        if configuration.basis_user_restriction_mode == BasisUserRestrictionMode::RejoinOnly {
            configuration.basis_user_restriction_mode = BasisUserRestrictionMode::Normal;
        }
        Self::set_configuration(configuration);
        let configuration = Self::configuration_or_default();

        STATE
            .high_quality_length
            .store(BasisAvatarBitPacking::convert_to_size(BitQuality::High), Ordering::Relaxed);
        Self::initialize_pulse_settings();
        Self::initialize_auth().context("initializing authentication")?;
        BasisHeadlessConnectionPolicyManager::initialize_from_config(configuration.disallow_headless);
        BasisGlobalLockManager::initialize_from_config(&configuration);
        BasisCrashReportStateManager::initialize_from_config(&configuration);
        BasisAudioRangeLimitManager::initialize_from_config(&configuration);
        BasisAvatarScaleLimitManager::initialize_from_config(&configuration);
        BasisResourceLimitManager::initialize_from_config(&configuration);
        Self::setup_server(&configuration).context("setting up the network stack")?;
        Self::subscribe_events(&configuration).context("subscribing server events")?;

        if configuration.enable_statistics
            && let Some(server) = Self::server()
        {
            BasisStatistics::start_worker_thread(server);
        }

        BasisNetworkUdpDropMonitor::start();
        BasisServerMemoryReclaim::start();

        BNL::log("Server Worker Threads Booted");
        Ok(())
    }

    pub fn stop_server() {
        let Some(server) = STATE.server.write().take() else {
            return;
        };
        server.stop();
        BasisNetworkUdpDropMonitor::stop();
        BasisServerMemoryReclaim::stop();
        // start_server builds a fresh AuthIdentity; without this the old one stays subscribed to
        // the static OnAuthReceived event — pinned forever, and handling every auth packet twice.
        // Left non-null so a straggling disconnect event can still resolve UUIDs while stopping.
        if let Some(identity) = Self::auth_identity() {
            identity.de_initialize();
        }
        *STATE.listener.write() = None;
        STATE.authenticated_peers.clear();
        *STATE.peer_snapshot.write() = Arc::from(Vec::new());
    }

    pub fn initialize_pulse_settings() {
        let configuration = Self::configuration_or_default();
        BasisServerReductionSystemEvents::set_max_degree_of_parallelism(configuration.bsr_max_degree_of_parallelism);
        BasisServerReductionSystemEvents::set_send_phase_budget_percent(configuration.bsr_send_phase_budget_percent);
        let configured_max_sockets =
            BasisTransportConfigStore::get::<LNLTransportConfig>(BasisNetworkStackRegistry::LITE_NET_LIB_ID).max_send_sockets;
        // 0 = auto, derived from the core count. See BasisCpuBudget::auto_max_send_sockets.
        BasisServerReductionSystemEvents::set_max_send_sockets(if configured_max_sockets > 0 {
            configured_max_sockets
        } else {
            BasisCpuBudget::auto_max_send_sockets()
        });
        BasisServerReductionSystemEvents::set_bsr_base_multiplier(configuration.bsr_base_multiplier as f32);
        BasisServerReductionSystemEvents::set_bsrs_millisecond_default_interval(configuration.bsrs_millisecond_default_interval);
        BasisServerReductionSystemEvents::set_bsrs_increase_rate(configuration.bsrs_increase_rate);
        BasisServerReductionSystemEvents::set_distance_update_interval_ticks(configuration.distance_update_interval_ticks.max(1));
        BasisServerReductionSystemEvents::set_enable_compute_offload(configuration.enable_compute_offload);
        BasisServerReductionSystemEvents::set_compute_device(&configuration.compute_device);
        BasisServerReductionSystemEvents::set_compute_distance_update_interval_ticks(
            configuration.compute_distance_update_interval_ticks.max(1),
        );
        BasisServerReductionSystemEvents::set_high_distance_sq(configuration.high_quality_distance * configuration.high_quality_distance);
        BasisServerReductionSystemEvents::set_medium_distance_sq(
            configuration.medium_quality_distance * configuration.medium_quality_distance,
        );
        BasisServerReductionSystemEvents::set_low_distance_sq(configuration.low_quality_distance * configuration.low_quality_distance);
        BasisServerReductionSystemEvents::set_enable_avatar_bundle_compression(configuration.enable_avatar_bundle_compression);
        BasisServerReductionSystemEvents::set_avatar_bundle_min_messages(configuration.avatar_bundle_min_messages);
        BasisServerReductionSystemEvents::set_avatar_bundle_min_bytes(configuration.avatar_bundle_min_bytes);
        BasisServerReductionSystemEvents::set_enable_avatar_bundle_zstd(configuration.enable_avatar_bundle_zstd);
        BasisServerReductionSystemEvents::set_avatar_bundle_zstd_delta_bundles(configuration.avatar_bundle_zstd_delta_bundles);
        BasisServerReductionSystemEvents::set_avatar_bundle_zstd_level(configuration.avatar_bundle_zstd_level);
        BasisServerReductionSystemEvents::set_avatar_bundle_zstd_max_shed_tier(configuration.avatar_bundle_zstd_max_shed_tier);
        // Level lives on the codec rather than being passed per call: it decides how the pooled
        // compression contexts are built, so a change has to invalidate them.
        BasisAvatarBundleZstd::set_level(configuration.avatar_bundle_zstd_level);
        BasisServerReductionSystemEvents::set_enable_avatar_delta_compression(configuration.enable_avatar_delta_compression);
        BasisServerReductionSystemEvents::set_avatar_delta_keyframe_interval_ms(configuration.avatar_delta_keyframe_interval_ms);
        BasisServerReductionSystemEvents::set_avatar_delta_keyframe_max_interval_ms(configuration.avatar_delta_keyframe_max_interval_ms);
        BasisServerReductionSystemEvents::set_strip_additional_data_at_low_quality(configuration.strip_additional_data_at_low_quality);
        BSRProfiler::set_enabled(configuration.enable_bsr_profiling || configuration.health_include_bsr_profiling);
        BSRProfiler::set_write_to_log(configuration.enable_bsr_profiling && !configuration.health_include_bsr_profiling);
        BasisServerReductionSystemEvents::set_write_load_log(!configuration.health_include_bsr_profiling);
        // Re-broadcast when a (re)applied config changes the live value so already-connected
        // clients stay consistent with what new joiners are told.
        if BasisOpusFrameDurationStateManager::set_frame_duration_ms(configuration.voice_frame_duration_ms) {
            BasisOpusFrameDurationStateManager::broadcast_state();
        }
        // Report whether the Zstd path is actually live, not just whether it is configured on.
        let zstd_state = if !configuration.enable_avatar_bundle_zstd {
            "off".to_string()
        } else if !BasisAvatarBundleZstd::available() {
            "INERT (no dictionary embedded)".to_string()
        } else {
            format!(
                "on (level {}, dictGen {}, maxShedTier {}{})",
                configuration.avatar_bundle_zstd_level,
                BasisAvatarBundleZstd::dictionary_generation(),
                configuration.avatar_bundle_zstd_max_shed_tier,
                if configuration.avatar_bundle_zstd_delta_bundles { ", deltas too" } else { "" }
            )
        };
        BNL::log(format!(
            "[BSR] AvatarBundleCompression={} (minMsgs={}, minBytes={}) BundleZstd={zstd_state} DeltaCompression={} (keyframeMs={}) VoiceFrameDurationMs={}",
            configuration.enable_avatar_bundle_compression,
            configuration.avatar_bundle_min_messages,
            configuration.avatar_bundle_min_bytes,
            configuration.enable_avatar_delta_compression,
            configuration.avatar_delta_keyframe_interval_ms,
            BasisOpusFrameDurationStateManager::frame_duration_ms()
        ));
    }

    /// Re-applies every setting that can take effect without a restart and pushes the new state
    /// to connected clients, so a runtime edit (console /config, admin panel) is live at once.
    ///
    /// Deliberately not a re-run of `start_server`'s boot sequence: `initialize_auth` builds a
    /// fresh DID identity that subscribes to a static event and reloads permissions, allow and
    /// ban lists from disc. Only the password comparer is rebuilt.
    pub fn apply_live_configuration() {
        let Some(configuration) = Self::configuration() else {
            return;
        };

        Self::initialize_pulse_settings();

        Self::set_auth(Some(Arc::new(PasswordAuth::new(&configuration.password))));

        BasisHeadlessConnectionPolicyManager::initialize_from_config(configuration.disallow_headless);
        BasisGlobalLockManager::initialize_from_config(&configuration);
        BasisCrashReportStateManager::initialize_from_config(&configuration);
        BasisAudioRangeLimitManager::initialize_from_config(&configuration);
        BasisAvatarScaleLimitManager::initialize_from_config(&configuration);
        BasisResourceLimitManager::initialize_from_config(&configuration);

        if Self::server().is_none() {
            return;
        }

        BasisHeadlessConnectionPolicyManager::broadcast_state();
        BasisGlobalLockManager::broadcast_lock_state();
        BasisCrashReportStateManager::broadcast_state();
        BasisAudioRangeLimitManager::broadcast_state();
        BasisAvatarScaleLimitManager::broadcast_state();
        BasisResourceLimitManager::broadcast_state();
    }

    fn initialize_auth() -> BasisResult<()> {
        let configuration = Self::configuration_or_default();
        let has_file_support = configuration.has_file_support;
        BasisPlayerModeration::set_use_file_on_disc(has_file_support);
        IAuthIdentitySupport::set_has_file_support(has_file_support);

        Self::set_auth(Some(Arc::new(PasswordAuth::new(&configuration.password))));
        Self::set_auth_identity(Some(BasisDIDAuthIdentity::new()));

        if has_file_support {
            // Keep permissions with other config files.
            let config_dir = Self::config_directory();
            std::fs::create_dir_all(&config_dir)
                .with_context(|| format!("creating the config folder '{}'", config_dir.display()))?;
            PermissionIntegration::init(config_dir.join("permissions.xml")).context("loading permissions.xml")?;
            *STATE.allow_list.write() = Some(Arc::new(BasisAllowList::with_file(config_dir.join("BasisAllowList.txt"))));
            *STATE.ban_list.write() = Some(Arc::new(BasisBanList::with_file(config_dir.join("BasisBanList.txt"))));
        } else {
            PermissionIntegration::init_without_disc();
            // Best-effort in-memory lists when the host disabled disk support.
            *STATE.allow_list.write() = Some(Arc::new(BasisAllowList::in_memory()));
            *STATE.ban_list.write() = Some(Arc::new(BasisBanList::in_memory()));
        }
        Ok(())
    }

    /// `AppDomain.CurrentDomain.BaseDirectory/config`.
    pub fn config_directory() -> PathBuf {
        Configuration::base_directory().join(Configuration::CONFIG_FOLDER_NAME)
    }

    fn subscribe_events(configuration: &Configuration) -> BasisResult<()> {
        BasisServerHandleEvents::subscribe_server_events()?;
        if let Err(e) = BasisPlayerModeration::load_banned_players() {
            BNL::log_error(format!("Load banned failed: {e}"));
        }
        if let Err(e) = BasisNetworkChat::load_word_filter(configuration) {
            BNL::log_error(format!("Failed to load chat word filter: {e}"));
        }
        BasisNetworkStackRegistry::register_introducer_factory(
            BasisNetworkStackRegistry::IROH_ID,
            Arc::new(|_manager| Arc::new(IrohPeerIntroducer)),
        );
        BasisServerP2PBroker::initialize();
        Ok(())
    }

    // ── Server setup ───────────────────────────────────────────────────────

    pub fn setup_server(configuration: &Configuration) -> BasisResult<()> {
        let listener = EventBasedNetListener::new();
        *STATE.listener.write() = Some(listener.clone());
        let server = BasisNetworkStackRegistry::create(&configuration.network_stack_id, listener, configuration).ok_or_else(|| {
            BasisError::permanent(
                ErrorCode::Transport,
                format!("network stack '{}' could not be created", configuration.network_stack_id),
            )
        })?;
        *STATE.server.write() = Some(server);

        NetDebug::set_logger(Some(Arc::new(BasisServerLogger)));
        Self::start_listening(configuration)
    }

    pub fn start_listening(configuration: &Configuration) -> BasisResult<()> {
        let (ipv4, ipv6) = if configuration.override_auto_discovery_of_ipv {
            let ipv4 = configuration.i_pv4_address.parse::<IpAddr>().unwrap_or_else(|_| {
                BNL::log_warning(format!(
                    "Failed to parse IPv4 bind address '{}', falling back to 0.0.0.0",
                    configuration.i_pv4_address
                ));
                IpAddr::from([0, 0, 0, 0])
            });
            let ipv6 = configuration.i_pv6_address.parse::<IpAddr>().unwrap_or_else(|_| {
                BNL::log_warning(format!("Failed to parse IPv6 bind address '{}', falling back to [::]", configuration.i_pv6_address));
                IpAddr::from([0u16; 8])
            });
            (ipv4, ipv6)
        } else {
            (IpAddr::from([0, 0, 0, 0]), IpAddr::from([0u16; 8]))
        };

        let server = Self::server().ok_or_else(|| BasisError::permanent(ErrorCode::Conflict, "start_listening before setup_server"))?;
        server
            .start(ipv4, ipv6, configuration.set_port)
            .with_context(|| format!("listening on port {}", configuration.set_port))?;
        BNL::log(format!("Listening on UDP port {}", configuration.set_port));
        BNL::log(format!("  IPv4 bind: {ipv4}"));
        BNL::log(format!("  IPv6 bind: [{ipv6}]"));
        Ok(())
    }

    // ── Sending ────────────────────────────────────────────────────────────

    /// Broadcasts to every client except `sender`.
    pub fn broadcast_message_to_clients_excluding(
        writer: &NetDataWriter,
        channel: u8,
        sender: &NetPeerRef,
        clients: &[NetPeerRef],
        delivery_method: DeliveryMethod,
    ) {
        Self::broadcast_message_to_clients_excluding_with_limit(writer, channel, sender, clients, delivery_method, Self::DEFAULT_MAX_MESSAGES);
    }

    pub fn broadcast_message_to_clients_excluding_with_limit(
        writer: &NetDataWriter,
        channel: u8,
        sender: &NetPeerRef,
        clients: &[NetPeerRef],
        delivery_method: DeliveryMethod,
        max_messages: i32,
    ) {
        if !Self::check_validated(writer) {
            return;
        }
        let sender_id = sender.id();
        let mut sent: i64 = 0;
        for client in clients {
            if client.id() != sender_id && Self::try_send_no_record(client, writer, channel, delivery_method, max_messages) {
                sent += 1;
            }
        }
        BasisNetworkStatistics::record_outbound_batch(channel, sent, sent * writer.length() as i64);
    }

    pub fn broadcast_message_to_clients(writer: &NetDataWriter, channel: u8, clients: &[NetPeerRef], delivery_method: DeliveryMethod) {
        Self::broadcast_message_to_clients_with_limit(writer, channel, clients, delivery_method, Self::DEFAULT_MAX_MESSAGES);
    }

    pub fn broadcast_message_to_clients_with_limit(
        writer: &NetDataWriter,
        channel: u8,
        clients: &[NetPeerRef],
        delivery_method: DeliveryMethod,
        max_messages: i32,
    ) {
        if !Self::check_validated(writer) {
            return;
        }
        let mut sent: i64 = 0;
        for client in clients {
            if Self::try_send_no_record(client, writer, channel, delivery_method, max_messages) {
                sent += 1;
            }
        }
        BasisNetworkStatistics::record_outbound_batch(channel, sent, sent * writer.length() as i64);
    }

    pub fn try_send(client: &NetPeerRef, writer: &NetDataWriter, channel: u8, delivery_method: DeliveryMethod) {
        Self::try_send_with_limit(client, writer, channel, delivery_method, Self::DEFAULT_MAX_MESSAGES);
    }

    pub fn try_send_with_limit(client: &NetPeerRef, writer: &NetDataWriter, channel: u8, delivery_method: DeliveryMethod, max_messages: i32) {
        if Self::try_send_no_record(client, writer, channel, delivery_method, max_messages) {
            BasisNetworkStatistics::record_outbound(channel, writer.length());
        }
    }

    /// True if the send actually went out (vs dropped by the per-channel queue cap or refused
    /// by the transport). Splits the queue/send decision from the stats record so broadcast
    /// loops can fold N× atomics into one `record_outbound_batch` call per (channel, broadcast).
    fn try_send_no_record(client: &NetPeerRef, writer: &NetDataWriter, channel: u8, delivery_method: DeliveryMethod, max_messages: i32) -> bool {
        if matches!(delivery_method, DeliveryMethod::Sequenced | DeliveryMethod::Unreliable) {
            let queued_messages = client.get_packets_count_in_queue(channel, delivery_method);
            if queued_messages > max_messages {
                return false;
            }
        }
        match client.send_writer(writer, channel, delivery_method) {
            Ok(()) => true,
            Err(e) => {
                // The C# transport threw here; a payload the transport cannot carry is a server
                // bug worth the log line, never a reason to stop the broadcast loop.
                BNL::log_error(format!("Send to peer {} on channel {channel} refused: {e}", client.id()));
                false
            }
        }
    }

    pub fn check_validated(writer: &NetDataWriter) -> bool {
        if writer.length() == 0 {
            BNL::log_error("Trying to send a message with zero length!");
            return false;
        }
        true
    }
}
