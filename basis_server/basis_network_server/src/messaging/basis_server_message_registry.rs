//! Port of `Messaging/BasisServerMessageRegistry.cs`: table-driven inbound dispatch. Core
//! messages bind to their dedicated channel (0-59); multiplexed plugin messages bind to a u16 id
//! read from the 61-63 channel payload.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Once};

use basis_network_core::SerializableBasis::{
    BasisMessageCatalog, BasisMessageDescriptor, BasisMessageFlags, BasisMessageSubscribe, BasisMessageSupply, ServerStatisticMessage,
};
use basis_network_core::statistics::Snapshot as StatisticsSnapshot;
use basis_network_core::statistics::basis_network_statistics::BasisNetworkStatistics;
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetDataWriter, NetPacketReader, NetPeerRef, NetResult};
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};

use crate::NetworkServer;
use crate::core::basis_server_events_router::BasisServerEventsRouter;
use crate::core::basis_server_handle_events::BasisServerHandleEvents;
use crate::handlers::BasisNetworkPIPCamera;
use crate::networking::{BasisNetworkChat, BasisNetworkContentShare, BasisNetworkOwnership, BasisNetworkingGeneric};
use crate::p2p::BasisServerP2PBroker;
use crate::reduction::BasisServerReductionSystemEvents;
use crate::security::{BasisPlayerModeration, PermNodes, PermissionIntegration};

/// `(peer, reader, channel, delivery)`. The reader is owned: a handler consumes the packet.
pub type BasisServerMessageHandler = Arc<dyn Fn(&NetPeerRef, NetPacketReader, u8, DeliveryMethod) + Send + Sync>;

struct PluginIds {
    by_name: HashMap<String, u16>,
    next: u16,
}

static CORE_HANDLERS: LazyLock<RwLock<Vec<Option<BasisServerMessageHandler>>>> =
    LazyLock::new(|| RwLock::new(vec![None; usize::from(BasisNetworkCommons::TOTAL_CHANNELS)]));
static PLUGIN_HANDLERS: LazyLock<DashMap<u16, BasisServerMessageHandler>> = LazyLock::new(DashMap::new);
static PLUGIN_DESCRIPTORS: LazyLock<DashMap<u16, BasisMessageDescriptor>> = LazyLock::new(DashMap::new);
static SUBSCRIPTIONS: LazyLock<DashMap<i32, HashSet<u16>>> = LazyLock::new(DashMap::new);
static PLUGIN_IDS: LazyLock<Mutex<PluginIds>> =
    LazyLock::new(|| Mutex::new(PluginIds { by_name: HashMap::new(), next: BasisServerMessageRegistry::PLUGIN_ID_BASE }));
/// The supplied manifest only changes when plugins (un)register, which is expected at startup.
/// Cached as one atomically-swapped snapshot so per-connect `send_supply_to` is allocation-free.
static SUPPLY_SNAPSHOT: RwLock<Option<(u64, Arc<[BasisMessageDescriptor]>)>> = RwLock::new(None);
static SUPPLY_VERSION: AtomicU64 = AtomicU64::new(0);
static CORE_INIT: Once = Once::new();

pub struct BasisServerMessageRegistry;

impl BasisServerMessageRegistry {
    /// Plugin ids start above the core channel range (0-63) so they never collide with core ids
    /// in the flat manifest/subscription space.
    pub const PLUGIN_ID_BASE: u16 = 64;

    /// Registers the core handlers. Safe to call repeatedly.
    pub fn ensure_initialized() {
        CORE_INIT.call_once(Self::register_core_handlers);
    }

    pub fn register_core(channel: u8, handler: BasisServerMessageHandler) {
        if let Some(slot) = CORE_HANDLERS.write().get_mut(usize::from(channel)) {
            *slot = Some(handler);
        }
    }

    pub fn resolve_core(channel: u8) -> Option<BasisServerMessageHandler> {
        Self::ensure_initialized();
        CORE_HANDLERS.read().get(usize::from(channel)).and_then(|h| h.clone())
    }

    /// Bind a multiplexed plugin message id (carried on channels 61-63) to a handler. Not
    /// advertised in the manifest; prefer [`register_plugin_descriptor`](Self::register_plugin_descriptor).
    pub fn register_plugin(id: u16, handler: BasisServerMessageHandler) {
        PLUGIN_HANDLERS.insert(id, handler);
    }

    /// Bind a plugin message and advertise it in the supplied manifest so clients can subscribe
    /// by name.
    pub fn register_plugin_descriptor(descriptor: BasisMessageDescriptor, handler: BasisServerMessageHandler) {
        PLUGIN_HANDLERS.insert(descriptor.id, handler);
        PLUGIN_DESCRIPTORS.insert(descriptor.id, descriptor);
        Self::invalidate_supply();
    }

    /// Remove a plugin message handler and its manifest descriptor. Returns true if a handler
    /// was bound.
    pub fn unregister_plugin(id: u16) -> bool {
        PLUGIN_DESCRIPTORS.remove(&id);
        let removed = PLUGIN_HANDLERS.remove(&id).is_some();
        Self::invalidate_supply();
        removed
    }

    /// Register a plugin message by name with an auto-assigned id, advertise it in the manifest,
    /// and bind its handler. Returns the assigned id. Ids are assigned in registration order from
    /// `PLUGIN_ID_BASE`; register plugins in a deterministic order for stable ids across restarts.
    pub fn register_server_plugin(
        name: &str,
        delivery: DeliveryMethod,
        handler: BasisServerMessageHandler,
        version: u8,
        extra_flags: BasisMessageFlags,
    ) -> u16 {
        let id = {
            let mut ids = PLUGIN_IDS.lock();
            match ids.by_name.get(name) {
                Some(id) => *id,
                None => {
                    let id = ids.next;
                    ids.next = ids.next.wrapping_add(1);
                    ids.by_name.insert(name.to_string(), id);
                    id
                }
            }
        };
        let descriptor = BasisMessageDescriptor {
            id,
            version,
            channel: BasisNetworkCommons::get_plugin_channel_for_delivery(delivery),
            flags: (BasisMessageFlags::MULTIPLEXED | extra_flags).bits(),
            name: name.to_string(),
        };
        PLUGIN_HANDLERS.insert(id, handler);
        PLUGIN_DESCRIPTORS.insert(id, descriptor);
        Self::invalidate_supply();
        id
    }

    /// Look up a plugin's assigned message id by name.
    pub fn try_get_plugin_id(name: &str) -> Option<u16> {
        PLUGIN_IDS.lock().by_name.get(name).copied()
    }

    /// Send a plugin message to a peer by name: prepends the id and uses the descriptor's
    /// channel. Skips peers that did not subscribe to the id. Returns false if the plugin is
    /// unknown, skipped, or the payload could not be written.
    pub fn send_to_peer(peer: &NetPeerRef, name: &str, write_payload: impl FnOnce(&mut NetDataWriter) -> NetResult<()>) -> bool {
        let Some(id) = Self::try_get_plugin_id(name) else {
            return false;
        };
        let Some(descriptor) = PLUGIN_DESCRIPTORS.get(&id).map(|d| d.clone()) else {
            return false;
        };
        if !Self::is_subscribed(peer.id(), id) {
            return false;
        }
        let mut writer = NetworkServer::rent_writer();
        writer.put_ushort(id);
        let written = write_payload(&mut writer).is_ok();
        if written {
            NetworkServer::try_send(peer, &writer, descriptor.channel, BasisNetworkCommons::get_delivery_for_plugin_channel(descriptor.channel));
        }
        NetworkServer::return_writer(writer);
        written
    }

    /// Core catalog plus any registered plugin descriptors — the manifest supplied to each
    /// client. Cached until a plugin (un)registers.
    pub fn build_supply() -> Arc<[BasisMessageDescriptor]> {
        let version = SUPPLY_VERSION.load(Ordering::Acquire);
        if let Some((cached_version, descriptors)) = SUPPLY_SNAPSHOT.read().as_ref()
            && *cached_version == version
        {
            return descriptors.clone();
        }
        let mut combined: Vec<BasisMessageDescriptor> = BasisMessageCatalog::build_core().to_vec();
        let mut plugins: Vec<BasisMessageDescriptor> = PLUGIN_DESCRIPTORS.iter().map(|d| d.value().clone()).collect();
        plugins.sort_by_key(|d| d.id);
        combined.extend(plugins);
        let result: Arc<[BasisMessageDescriptor]> = Arc::from(combined);
        *SUPPLY_SNAPSHOT.write() = Some((version, result.clone()));
        result
    }

    /// Invalidate the cached manifest after a plugin (un)registers.
    fn invalidate_supply() {
        SUPPLY_VERSION.fetch_add(1, Ordering::AcqRel);
    }

    /// Send the registry manifest to a peer (RegistryControlChannel, RegistrySub_Supply). Called
    /// once per connect.
    pub fn send_supply_to(peer: &NetPeerRef) {
        let supply = BasisMessageSupply { descriptors: Self::build_supply().to_vec() };
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(BasisNetworkCommons::REGISTRY_SUB_SUPPLY);
        if supply.serialize(&mut writer).is_ok() {
            NetworkServer::try_send(peer, &writer, BasisNetworkCommons::REGISTRY_CONTROL_CHANNEL, DeliveryMethod::ReliableOrdered);
        }
        NetworkServer::return_writer(writer);
    }

    /// Record the message ids a peer reported it can handle (from RegistrySub_Subscribe).
    pub fn set_subscription(peer_id: i32, ids: &[u16]) {
        SUBSCRIPTIONS.insert(peer_id, ids.iter().copied().collect());
    }

    /// True if the peer subscribed to this message id. Also true when the peer never sent a
    /// subscription (no filtering until it does).
    pub fn is_subscribed(peer_id: i32, id: u16) -> bool {
        SUBSCRIPTIONS.get(&peer_id).is_none_or(|set| set.contains(&id))
    }

    /// Drop a peer's subscription record on disconnect.
    pub fn clear_subscription(peer_id: i32) {
        SUBSCRIPTIONS.remove(&peer_id);
    }

    /// Reads the leading u16 message id from a plugin channel payload and dispatches it. Returns
    /// false (leaving the caller to error-count) when the id is unknown or the payload is too
    /// short to carry an id.
    pub fn dispatch_plugin(peer: &NetPeerRef, mut reader: NetPacketReader, channel: u8, delivery_method: DeliveryMethod) -> bool {
        let Ok(id) = reader.get_ushort() else {
            return false;
        };
        let Some(handler) = PLUGIN_HANDLERS.get(&id).map(|h| h.clone()) else {
            return false;
        };
        handler(peer, reader, channel, delivery_method);
        true
    }

    /// Drops plugin registrations and subscriptions (core handlers stay). Tests.
    pub fn reset_plugins_for_tests() {
        PLUGIN_HANDLERS.clear();
        PLUGIN_DESCRIPTORS.clear();
        SUBSCRIPTIONS.clear();
        let mut ids = PLUGIN_IDS.lock();
        ids.by_name.clear();
        ids.next = Self::PLUGIN_ID_BASE;
        drop(ids);
        Self::invalidate_supply();
    }

    fn core(f: impl Fn(&NetPeerRef, NetPacketReader, u8, DeliveryMethod) + Send + Sync + 'static) -> BasisServerMessageHandler {
        Arc::new(f)
    }

    fn register_core_handlers() {
        use BasisNetworkCommons as C;
        Self::register_core(C::SHOUT_VOICE_CHANNEL, Self::core(|peer, reader, _, _| BasisServerHandleEvents::handle_shout_voice_message(reader, peer)));
        Self::register_core(C::AUTH_IDENTITY_CHANNEL, Self::core(|peer, reader, _, _| BasisServerHandleEvents::handle_auth(reader, peer)));

        let avatar_movement = Self::core(|peer, reader, channel, _| BasisServerReductionSystemEvents::handle_avatar_movement(reader, peer, channel));
        Self::register_core(C::PLAYER_AVATAR_HIGH_CHANNEL, avatar_movement.clone());
        Self::register_core(C::PLAYER_AVATAR_HIGH_ADDITIONAL_CHANNEL, avatar_movement);

        Self::register_core(C::DELTA_AVATAR_CHANNEL, Self::core(|peer, reader, _, _| BasisServerReductionSystemEvents::handle_delta_channel_inbound(reader, peer)));
        Self::register_core(C::VOICE_CHANNEL, Self::core(|peer, reader, _, _| BasisServerHandleEvents::handle_voice_message(reader, peer)));
        Self::register_core(C::AVATAR_CHANNEL, Self::core(|peer, reader, _, dm| BasisNetworkingGeneric::handle_avatar_default(reader, dm, peer)));
        Self::register_core(C::SCENE_CHANNEL, Self::core(|peer, reader, _, dm| BasisNetworkingGeneric::handle_scene_default(reader, dm, peer)));
        Self::register_core(
            C::DIRECT_AVATAR_SERVER_CHANNEL,
            Self::core(|peer, reader, _, dm| BasisNetworkingGeneric::handle_avatar(reader, dm, peer, C::DIRECT_AVATAR_SERVER_CHANNEL)),
        );
        Self::register_core(
            C::DIRECT_SCENE_SERVER_CHANNEL,
            Self::core(|peer, reader, _, dm| BasisNetworkingGeneric::handle_scene(reader, dm, peer, C::DIRECT_SCENE_SERVER_CHANNEL)),
        );
        Self::register_core(C::AVATAR_CHANGE_MESSAGE_CHANNEL, Self::core(|peer, reader, _, _| BasisServerHandleEvents::send_avatar_message_to_clients(reader, peer)));

        Self::register_core(
            C::CHANGE_CURRENT_OWNER_REQUEST_CHANNEL,
            Self::core(|peer, reader, _, _| Self::handle_permitted(peer, reader, PermNodes::OWNERSHIP_TRANSFER, BasisNetworkOwnership::ownership_transfer)),
        );
        Self::register_core(
            C::GET_CURRENT_OWNER_REQUEST_CHANNEL,
            Self::core(|peer, reader, _, _| Self::handle_permitted(peer, reader, PermNodes::OWNERSHIP_GET, BasisNetworkOwnership::ownership_response)),
        );
        Self::register_core(
            C::REMOVE_CURRENT_OWNER_REQUEST_CHANNEL,
            Self::core(|peer, reader, _, _| Self::handle_permitted(peer, reader, PermNodes::OWNERSHIP_REMOVE, BasisNetworkOwnership::remove_ownership)),
        );

        Self::register_core(C::AUDIO_RECIPIENTS_CHANNEL, Self::core(|peer, reader, _, _| BasisServerHandleEvents::update_voice_receivers(reader, peer, false)));
        Self::register_core(C::AUDIO_RECIPIENTS_LARGE_CHANNEL, Self::core(|peer, reader, _, _| BasisServerHandleEvents::update_voice_receivers(reader, peer, true)));
        Self::register_core(
            C::AUDIO_RECIPIENTS_INVERTED_CHANNEL,
            Self::core(|peer, reader, _, _| BasisServerHandleEvents::update_voice_receivers_inverted(reader, peer, false)),
        );
        Self::register_core(
            C::AUDIO_RECIPIENTS_INVERTED_LARGE_CHANNEL,
            Self::core(|peer, reader, _, _| BasisServerHandleEvents::update_voice_receivers_inverted(reader, peer, true)),
        );
        Self::register_core(C::AUDIO_RECIPIENTS_BITFIELD_CHANNEL, Self::core(|peer, reader, _, _| BasisServerHandleEvents::update_voice_receivers_bitfield(reader, peer)));
        Self::register_core(C::NET_ID_ASSIGN_CHANNEL, Self::core(|peer, reader, _, _| BasisServerHandleEvents::net_id_assign(reader, peer)));

        Self::register_core(
            C::LOAD_RESOURCE_CHANNEL,
            Self::core(|peer, reader, _, _| match NetworkServer::net_id_to_uuid(peer) {
                Some(uuid) => BasisServerHandleEvents::load_resource(reader, peer, &uuid),
                None => BNL::log_error(format!("User UUID not found for peer: {}", peer.id())),
            }),
        );
        Self::register_core(C::UNLOAD_RESOURCE_CHANNEL, Self::core(|peer, reader, _, _| BasisServerHandleEvents::unload_resource(reader, peer)));
        Self::register_core(C::MODIFY_RESOURCE_CHANNEL, Self::core(|peer, reader, _, _| BasisServerHandleEvents::handle_modify_resource(reader, peer)));
        Self::register_core(C::ADMIN_CHANNEL, Self::core(|peer, reader, _, _| BasisPlayerModeration::on_admin_message(peer, reader)));

        Self::register_core(
            C::CONTENT_SHARE_CHANNEL,
            Self::core(|peer, mut reader, _, _| {
                // Multiplexed: first byte selects drop vs cleanup.
                let Ok(sub) = reader.get_byte() else {
                    return;
                };
                if sub == C::CONTENT_SHARE_SUB_CLEANUP {
                    BasisNetworkContentShare::handle_content_share_cleanup(reader, peer);
                } else {
                    BasisNetworkContentShare::handle_content_share_drop(reader, peer);
                }
            }),
        );

        Self::register_core(C::SERVER_BOUND_CHANNEL, Self::core(|peer, reader, _, dm| BasisServerHandleEvents::raise_server_received(peer, reader, dm)));

        Self::register_core(
            C::SERVER_STATISTICS_CHANNEL,
            Self::core(|peer, mut reader, _, _| {
                // Permission-gated stats
                if Self::try_with_permission(peer, PermNodes::SERVER_STATS).is_none() {
                    return;
                }
                if reader.get_bool().unwrap_or(false) {
                    BNL::log("requested Server StatisticsChannel");
                    BasisNetworkStatistics::set_is_recording_data(true);
                    let data = match StatisticsSnapshot::snapshot_reset_encode(true) {
                        Ok(data) => data,
                        Err(e) => {
                            BNL::log_error(format!("Could not encode the statistics snapshot: {e}"));
                            return;
                        }
                    };
                    let mut message = ServerStatisticMessage { data };
                    let mut writer = NetworkServer::rent_writer();
                    message.serialize(&mut writer);
                    NetworkServer::try_send(peer, &writer, C::SERVER_STATISTICS_CHANNEL, DeliveryMethod::ReliableOrdered);
                    NetworkServer::return_writer(writer);
                } else {
                    BasisNetworkStatistics::set_is_recording_data(false);
                }
            }),
        );

        Self::register_core(C::CHAT_CHANNEL, Self::core(|peer, reader, _, _| BasisNetworkChat::handle_chat_message(reader, peer)));
        Self::register_core(C::CAMERA_PIP_STATE_CHANNEL, Self::core(|peer, reader, _, _| BasisNetworkPIPCamera::handle_pip_state_change(reader, peer)));
        Self::register_core(C::CAMERA_PIP_POSITION_CHANNEL, Self::core(|peer, reader, _, _| BasisNetworkPIPCamera::handle_pip_position_update(reader, peer)));
        Self::register_core(C::PRELOAD_READY_CHANNEL, Self::core(|peer, reader, _, _| BasisServerHandleEvents::handle_preload_ready(reader, peer)));
        Self::register_core(C::EVENTS_CHANNEL, Self::core(|peer, reader, _, _| BasisServerEventsRouter::handle_event(reader, peer)));
        Self::register_core(C::P2P_CHANNEL, Self::core(|peer, reader, _, _| BasisServerP2PBroker::handle_p2p_message(reader, peer)));

        Self::register_core(
            C::REGISTRY_CONTROL_CHANNEL,
            Self::core(|peer, mut reader, _, _| {
                if reader.get_byte().ok() == Some(C::REGISTRY_SUB_SUBSCRIBE) {
                    let mut subscribe = BasisMessageSubscribe::default();
                    if subscribe.deserialize(&mut reader) {
                        Self::set_subscription(peer.id(), &subscribe.ids);
                    }
                }
            }),
        );
    }

    /// The peer's UUID when it holds `perm_node` (or the wildcard); `None` — with the refusal
    /// logged — otherwise.
    fn try_with_permission(peer: &NetPeerRef, perm_node: &str) -> Option<String> {
        let Some(uuid) = NetworkServer::net_id_to_uuid(peer) else {
            BNL::log_error(format!("User UUID not found for peer: {}", peer.id()));
            return None;
        };
        if PermissionIntegration::has_valid_requirement_uuid(&uuid, perm_node) {
            return Some(uuid);
        }
        BNL::log_error(format!("Unauthorized access attempt by UUID: {uuid} for {perm_node}"));
        None
    }

    fn handle_permitted(peer: &NetPeerRef, reader: NetPacketReader, perm_node: &str, action: fn(NetPacketReader, &NetPeerRef)) {
        if Self::try_with_permission(peer, perm_node).is_some() {
            action(reader, peer);
        }
    }
}
