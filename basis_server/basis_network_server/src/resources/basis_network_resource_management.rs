//! Port of `Resources/BasisNetworkResourceManagement.cs`: the loaded-resource database.

use std::sync::LazyLock;

use basis_network_core::SerializableBasis::{LocalLoadResource, ModifyResource, UnLoadResource};
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetPeerRef};
use dashmap::DashMap;

use crate::NetworkServer;
use crate::resources::BasisNetworkPreloadResourceManagement;
use crate::security::{PermNodes, PermissionIntegration};

static USHORT_NETWORK_DATABASE: LazyLock<DashMap<String, LocalLoadResource>> = LazyLock::new(DashMap::new);
/// Loaded-resource count per creator UUID, so the cap check is O(1) on the hot load path
/// instead of an O(N) scan of the whole database. Kept in step with the database by
/// note_resource_added/removed; can_creator_load_more re-derives it at the cap boundary so a
/// missed decrement can never permanently block a legitimate creator.
static PER_CREATOR_COUNT: LazyLock<DashMap<String, i32>> = LazyLock::new(DashMap::new);

pub struct BasisNetworkResourceManagement;

impl BasisNetworkResourceManagement {
    const DEFAULT_MAX_LOADED_RESOURCES_PER_PLAYER: i32 = 16384;

    pub fn ushort_network_database() -> &'static DashMap<String, LocalLoadResource> {
        &USHORT_NETWORK_DATABASE
    }

    /// Inserts without broadcasting; false when the id is already present.
    pub fn try_add_to_database(resource: LocalLoadResource) -> bool {
        match USHORT_NETWORK_DATABASE.entry(resource.loaded_net_id.clone()) {
            dashmap::mapref::entry::Entry::Occupied(_) => false,
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(resource);
                true
            }
        }
    }

    fn resolve_max_loaded_per_player() -> i32 {
        let configured = NetworkServer::configuration().map(|c| c.max_loaded_resources_per_player).unwrap_or(0);
        if configured > 0 { configured } else { Self::DEFAULT_MAX_LOADED_RESOURCES_PER_PLAYER }
    }

    /// True when `uuid` is under its loaded-resource cap. Server-authoritative loads (empty
    /// UUID) are never capped. Only a creator that appears to be at the cap pays an O(N)
    /// recount, which also heals any counter drift.
    pub fn can_creator_load_more(uuid: &str) -> bool {
        if uuid.is_empty() {
            return true;
        }
        let cap = Self::resolve_max_loaded_per_player();
        let approx = PER_CREATOR_COUNT.get(uuid).map(|c| *c).unwrap_or(0);
        if approx < cap {
            return true;
        }
        let real = USHORT_NETWORK_DATABASE.iter().filter(|e| e.value().uuid_of_creator == uuid).count() as i32;
        PER_CREATOR_COUNT.insert(uuid.to_string(), real);
        real < cap
    }

    pub fn note_resource_added(uuid: &str) {
        if uuid.is_empty() {
            return;
        }
        *PER_CREATOR_COUNT.entry(uuid.to_string()).or_insert(0) += 1;
    }

    pub fn note_resource_removed(uuid: &str) {
        if uuid.is_empty() {
            return;
        }
        if let dashmap::mapref::entry::Entry::Occupied(mut entry) = PER_CREATOR_COUNT.entry(uuid.to_string()) {
            if *entry.get() <= 1 {
                entry.remove();
            } else {
                *entry.get_mut() -= 1;
            }
        }
    }

    fn broadcast_unload(unload: &mut UnLoadResource) {
        let mut writer = NetworkServer::rent_writer();
        if unload.serialize(&mut writer).is_ok() {
            NetworkServer::broadcast_message_to_clients(
                &writer,
                BasisNetworkCommons::UNLOAD_RESOURCE_CHANNEL,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        }
        NetworkServer::return_writer(writer);
    }

    /// Unloads (and forgets) every non-persistent resource.
    pub fn reset() {
        let resources: Vec<LocalLoadResource> = USHORT_NETWORK_DATABASE.iter().map(|e| e.value().clone()).collect();
        for llr in resources {
            if llr.persist {
                continue;
            }
            let mut unload = UnLoadResource { mode: llr.mode, loaded_net_id: llr.loaded_net_id.clone() };
            Self::broadcast_unload(&mut unload);
            if let Some((_, removed)) = USHORT_NETWORK_DATABASE.remove(&llr.loaded_net_id) {
                Self::note_resource_removed(&removed.uuid_of_creator);
            }
        }
    }

    pub fn remove_peer_resources(uuid: &str) {
        if uuid.is_empty() {
            return;
        }
        let resources: Vec<LocalLoadResource> = USHORT_NETWORK_DATABASE.iter().map(|e| e.value().clone()).collect();
        for llr in resources {
            if llr.persist || llr.uuid_of_creator != uuid {
                continue;
            }
            let mut unload = UnLoadResource { mode: llr.mode, loaded_net_id: llr.loaded_net_id.clone() };
            Self::broadcast_unload(&mut unload);
            if let Some((_, removed)) = USHORT_NETWORK_DATABASE.remove(&llr.loaded_net_id) {
                Self::note_resource_removed(&removed.uuid_of_creator);
            }
        }
    }

    pub fn send_out_all_resources(new_connection: &NetPeerRef) {
        let resources: Vec<LocalLoadResource> = USHORT_NETWORK_DATABASE.iter().map(|e| e.value().clone()).collect();
        let mut writer = NetworkServer::rent_writer();
        for mut llr in resources {
            writer.reset();
            // For synchronized resources (LoadStrategy == 2), check if the session is still
            // active. If it already completed, send as immediate (0) so the late joiner spawns
            // right away instead of waiting for a spawn signal that will never come. If still
            // active, add the late joiner to the session so they participate.
            if llr.load_strategy == 2 && !BasisNetworkPreloadResourceManagement::add_late_joiner(&llr.loaded_net_id) {
                llr.load_strategy = 0;
            }
            if llr.serialize(&mut writer).is_ok() {
                NetworkServer::try_send(new_connection, &writer, BasisNetworkCommons::LOAD_RESOURCE_CHANNEL, DeliveryMethod::ReliableOrdered);
            }
        }
        NetworkServer::return_writer(writer);
    }

    /// Predownload broadcast: tell every connected client to cache the bundle to disc now.
    /// Deliberately NOT added to the database — it is not a loaded resource, so it is never
    /// replayed to late joiners and never spawns anything.
    pub fn predownload_resource(mut resource: LocalLoadResource) {
        let mut writer = NetworkServer::rent_writer();
        if resource.serialize(&mut writer).is_ok() {
            BNL::log(format!("Broadcasting predownload for {}", resource.combined_url));
            NetworkServer::broadcast_message_to_clients(
                &writer,
                BasisNetworkCommons::LOAD_RESOURCE_CHANNEL,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        }
        NetworkServer::return_writer(writer);
    }

    pub fn load_resource(mut resource: LocalLoadResource) {
        if !Self::can_creator_load_more(&resource.uuid_of_creator) {
            BNL::log_error(format!(
                "Creator {} reached the per-player loaded-object limit ({}); dropping {}.",
                resource.uuid_of_creator,
                Self::resolve_max_loaded_per_player(),
                resource.loaded_net_id
            ));
            return;
        }
        if USHORT_NETWORK_DATABASE.contains_key(&resource.loaded_net_id) {
            BNL::log_error(format!("Already have Object Loaded With {}", resource.loaded_net_id));
            return;
        }
        let mut writer = NetworkServer::rent_writer();
        let serialized = resource.serialize(&mut writer).is_ok();
        if serialized && Self::try_add_to_database(resource.clone()) {
            Self::note_resource_added(&resource.uuid_of_creator);
            BNL::log(format!("Adding Object {}", resource.loaded_net_id));
            NetworkServer::broadcast_message_to_clients(
                &writer,
                BasisNetworkCommons::LOAD_RESOURCE_CHANNEL,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        } else if serialized {
            BNL::log_error(format!("Try Add Failed Already have Object Loaded With {}", resource.loaded_net_id));
        }
        NetworkServer::return_writer(writer);
    }

    /// Server-authoritative path — skips the admin-lock peer check because the caller (REST API,
    /// etc.) is already authenticated at a higher level than any game peer. Returns false if the
    /// resource was not found.
    pub fn unload_resource_server(unload: &mut UnLoadResource) -> bool {
        let Some((_, removed)) = USHORT_NETWORK_DATABASE.remove(&unload.loaded_net_id) else {
            BNL::log_error(format!("[Server] Trying to unload an object that does not exist: {}", unload.loaded_net_id));
            return false;
        };
        Self::note_resource_removed(&removed.uuid_of_creator);
        BNL::log(format!("Removing Object (server) {}", unload.loaded_net_id));
        Self::broadcast_unload(unload);
        true
    }

    pub fn unload_resource(unload: &mut UnLoadResource, peer: &NetPeerRef) {
        let Some(resource) = USHORT_NETWORK_DATABASE.get(&unload.loaded_net_id).map(|r| r.clone()) else {
            BNL::log_error(format!("Trying to unload an object that does not exist! ID Provided was [{}]", unload.loaded_net_id));
            return;
        };
        // Admin lock validation
        if resource.is_admin_locked && !PermissionIntegration::has_valid_requirement(peer, PermNodes::PROTECTION) {
            return;
        }
        // Creator-or-moderator, same rule set_static applies. The unload permission node is in
        // the default group, so without this any player can delete every other player's props.
        let is_moderator_unload = PermissionIntegration::has_valid_requirement(peer, PermNodes::PROTECTION);
        let is_creator_unload =
            NetworkServer::net_id_to_uuid(peer).is_some_and(|uuid| !resource.uuid_of_creator.is_empty() && uuid == resource.uuid_of_creator);
        if !is_creator_unload && !is_moderator_unload {
            BNL::log_error(format!("Peer {} tried to unload [{}] they did not create.", peer.id(), unload.loaded_net_id));
            return;
        }
        // Only remove AFTER validation
        if USHORT_NETWORK_DATABASE.remove(&unload.loaded_net_id).is_none() {
            BNL::log_error(format!("Failed to remove object [{}] after validation.", unload.loaded_net_id));
            return;
        }
        Self::note_resource_removed(&resource.uuid_of_creator);
        BNL::log(format!("Removing Object {}", unload.loaded_net_id));
        Self::broadcast_unload(unload);
    }

    /// Toggle the server-authoritative "Static" flag on an already-spawned resource. Only the
    /// item's creator or a moderator (protection permission) may change it. On success the new
    /// state is stored and rebroadcast to every client (and replayed to late joiners).
    pub fn set_static(modify: &mut ModifyResource, peer: &NetPeerRef) {
        let Some(resource) = USHORT_NETWORK_DATABASE.get(&modify.loaded_net_id).map(|r| r.clone()) else {
            BNL::log_error(format!("Trying to modify an object that does not exist! ID Provided was [{}]", modify.loaded_net_id));
            return;
        };
        // Admin-lock implies frozen — a request can't ask for "admin-locked but movable".
        let target_admin_locked = modify.static_admin_locked;
        let target_static = modify.r#static || target_admin_locked;

        // Any transition that touches the admin tier (entering OR leaving it) requires a
        // moderator — the item's creator can't set or clear an admin lock. Plain static toggles
        // also allow the creator.
        let involves_admin_tier = resource.static_admin_locked || target_admin_locked;
        let is_moderator = PermissionIntegration::has_valid_requirement(peer, PermNodes::PROTECTION);
        let is_creator =
            NetworkServer::net_id_to_uuid(peer).is_some_and(|uuid| !resource.uuid_of_creator.is_empty() && uuid == resource.uuid_of_creator);
        let allowed = if involves_admin_tier { is_moderator } else { is_creator || is_moderator };
        if !allowed {
            return;
        }
        // No-op if nothing changes, to avoid spamming the network.
        if resource.r#static == target_static && resource.static_admin_locked == target_admin_locked {
            return;
        }
        if let Some(mut stored) = USHORT_NETWORK_DATABASE.get_mut(&modify.loaded_net_id) {
            stored.r#static = target_static;
            stored.static_admin_locked = target_admin_locked;
        }
        // Normalize the broadcast so every client agrees on the resolved state + routing.
        modify.r#static = target_static;
        modify.static_admin_locked = target_admin_locked;
        modify.mode = resource.mode;

        let mut writer = NetworkServer::rent_writer();
        if modify.serialize(&mut writer).is_ok() {
            BNL::log(format!("Set Static={target_static} AdminLocked={target_admin_locked} on Object {}", modify.loaded_net_id));
            NetworkServer::broadcast_message_to_clients(
                &writer,
                BasisNetworkCommons::MODIFY_RESOURCE_CHANNEL,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        }
        NetworkServer::return_writer(writer);
    }

    /// Drops the database and counters without broadcasting. Tests.
    pub fn clear_for_tests() {
        USHORT_NETWORK_DATABASE.clear();
        PER_CREATOR_COUNT.clear();
    }
}
