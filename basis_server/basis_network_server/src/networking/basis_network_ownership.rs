//! Port of `Networking/BasisNetworkOwnership.cs`: who owns which networked object.

use std::sync::LazyLock;

use basis_network_core::SerializableBasis::{OwnershipTransferMessage, PlayerIdMessage};
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetPacketReader, NetPeerRef};
use dashmap::DashMap;
use parking_lot::Mutex;

use crate::NetworkServer;

/// Object unique string ID -> owning player id.
static OWNERSHIP_BY_OBJECT_ID: LazyLock<DashMap<String, u16>> = LazyLock::new(DashMap::new);
/// Objects held per owner, so the per-player cap is a lookup rather than a scan of the table.
static OWNED_COUNT: LazyLock<DashMap<u16, usize>> = LazyLock::new(DashMap::new);
/// For synchronized multi-step operations.
static LOCK_OBJECT: Mutex<()> = Mutex::new(());

pub struct BasisNetworkOwnership;

impl BasisNetworkOwnership {
    pub fn ownership_by_object_id() -> &'static DashMap<String, u16> {
        &OWNERSHIP_BY_OBJECT_ID
    }

    pub fn send_out_ownership_information(peer: &NetPeerRef) {
        let mut writer = NetworkServer::rent_writer();
        let mut message = OwnershipTransferMessage::default();
        for ownership in OWNERSHIP_BY_OBJECT_ID.iter() {
            message.player_id_message.player_id = *ownership.value();
            message.ownership_id = ownership.key().clone();
            if message.serialize(&mut writer).is_ok() {
                NetworkServer::try_send(peer, &writer, BasisNetworkCommons::GET_CURRENT_OWNER_REQUEST_CHANNEL, DeliveryMethod::ReliableOrdered);
            }
            writer.reset();
        }
        NetworkServer::return_writer(writer);
    }

    fn read(mut reader: NetPacketReader, peer: &NetPeerRef, what: &str) -> Option<OwnershipTransferMessage> {
        let mut message = OwnershipTransferMessage::default();
        match message.deserialize(&mut reader) {
            Ok(()) => Some(message),
            Err(e) => {
                BNL::log_error(format!("Malformed {what} from peer {}: {e}", peer.id()));
                None
            }
        }
    }

    pub fn ownership_response(reader: NetPacketReader, peer: &NetPeerRef) {
        let Some(mut message) = Self::read(reader, peer, "ownership request") else {
            return;
        };
        // If we are not aware of this ownership id, only tell the requesting client it has been
        // assigned to them: ownership understanding has to be requested. Once requested it is
        // good for life, or until an ownership switch happens.
        let (_, current_owner) = Self::network_request_new_or_existing(&message, peer.id() as u16);
        let mut writer = NetworkServer::rent_writer();
        message.player_id_message.player_id = current_owner;
        if message.serialize(&mut writer).is_ok() {
            BNL::log(format!("OwnershipResponse {current_owner} for {}", message.player_id_message.player_id));
            NetworkServer::try_send(peer, &writer, BasisNetworkCommons::GET_CURRENT_OWNER_REQUEST_CHANNEL, DeliveryMethod::ReliableOrdered);
        }
        NetworkServer::return_writer(writer);
    }

    /// Removes an owner from the object, e.g. dropping a pickup tells the server nobody owns it.
    pub fn remove_ownership(reader: NetPacketReader, peer: &NetPeerRef) {
        let Some(mut message) = Self::read(reader, peer, "ownership release") else {
            return;
        };
        let _guard = LOCK_OBJECT.lock();
        let Some(player_id) = OWNERSHIP_BY_OBJECT_ID.get(&message.ownership_id).map(|v| *v) else {
            BNL::log_error(format!("Ownership was not found for {}", message.ownership_id));
            return;
        };
        // Authorize against the sending peer, not the id in the packet: the client fills that
        // field in itself, so trusting it lets any peer release any object by naming its owner.
        if player_id != peer.id() as u16 {
            BNL::log_error("the player that requested this did not own the object");
            return;
        }
        message.player_id_message.player_id = player_id;
        if Self::remove_object_locked(&message.ownership_id) {
            let mut writer = NetworkServer::rent_writer();
            if message.serialize(&mut writer).is_ok() {
                NetworkServer::broadcast_message_to_clients(
                    &writer,
                    BasisNetworkCommons::REMOVE_CURRENT_OWNER_REQUEST_CHANNEL,
                    &NetworkServer::peer_snapshot(),
                    DeliveryMethod::ReliableOrdered,
                );
            }
            NetworkServer::return_writer(writer);
        } else {
            BNL::log_error(format!("{} failure to remove!", message.ownership_id));
        }
    }

    /// Handles the ownership transfer for all clients.
    pub fn ownership_transfer(reader: NetPacketReader, peer: &NetPeerRef) {
        let Some(mut message) = Self::read(reader, peer, "ownership transfer") else {
            return;
        };
        let client_id = peer.id() as u16;
        let mut writer = NetworkServer::rent_writer();
        // All clients need to know about an ownership switch.
        if Self::switch_ownership(&message.ownership_id, client_id) {
            message.player_id_message.player_id = client_id;
            if message.serialize(&mut writer).is_ok() {
                BNL::log(format!("OwnershipResponse {} for {}", message.ownership_id, message.player_id_message.player_id));
                NetworkServer::broadcast_message_to_clients(
                    &writer,
                    BasisNetworkCommons::CHANGE_CURRENT_OWNER_REQUEST_CHANNEL,
                    &NetworkServer::peer_snapshot(),
                    DeliveryMethod::ReliableOrdered,
                );
            }
        } else {
            let (_, current_owner) = Self::network_request_new_or_existing(&message, client_id);
            message.player_id_message.player_id = current_owner;
            if message.serialize(&mut writer).is_ok() {
                NetworkServer::broadcast_message_to_clients(
                    &writer,
                    BasisNetworkCommons::CHANGE_CURRENT_OWNER_REQUEST_CHANNEL,
                    &NetworkServer::peer_snapshot(),
                    DeliveryMethod::ReliableOrdered,
                );
            }
        }
        NetworkServer::return_writer(writer);
    }

    /// Requests either new or existing ownership. Returns `(added, owner)`: `added` is true when
    /// the object was newly registered to `requester_id`; `owner` is who owns it now.
    pub fn network_request_new_or_existing(message: &OwnershipTransferMessage, requester_id: u16) -> (bool, u16) {
        if let Some(existing) = Self::get_ownership_information(&message.ownership_id) {
            // Ownership already exists, no need to add
            return (false, existing);
        }
        if !Self::add_ownership(&message.ownership_id, requester_id) {
            BNL::log_error(format!("Error while adding ownership for: {}", message.ownership_id));
            // The C# left the out parameter at 0 on this path.
            return (false, Self::get_ownership_information(&message.ownership_id).unwrap_or(0));
        }
        (true, requester_id)
    }

    /// The per-player ceiling on owned objects, from the configuration.
    fn owned_cap() -> usize {
        usize::try_from(NetworkServer::configuration_or_default().max_owned_objects_per_player).unwrap_or(0)
    }

    /// Adds an object with ownership information to the database.
    ///
    /// Refuses the claim once `owner_id` is at its ceiling: the ids are client-supplied strings
    /// and entries only leave when the owner disconnects, so this cap is what stops one client
    /// growing the table for as long as it stays connected.
    pub fn add_ownership(object_id: &str, owner_id: u16) -> bool {
        let cap = Self::owned_cap();
        // Counted, not scanned: a ceiling this high would make an O(n) scan per claim O(n^2)
        // to reach, which is its own denial of service.
        if cap > 0 && Self::owned_count(owner_id) >= cap {
            BNL::log_warning(format!("Player {owner_id} already owns {cap} objects (the per-player cap); refusing ownership of {object_id}."));
            return false;
        }
        match OWNERSHIP_BY_OBJECT_ID.entry(object_id.to_string()) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                BNL::log_error(format!("Failed to add Object {object_id} to object ownership lookup."));
                false
            }
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(owner_id);
                Self::note_owned(owner_id, 1);
                BNL::log(format!("Object {object_id} added with owner {owner_id}"));
                true
            }
        }
    }

    /// Objects `owner_id` currently holds, from the running count.
    pub fn owned_count(owner_id: u16) -> usize {
        OWNED_COUNT.get(&owner_id).map(|c| *c.value()).unwrap_or(0)
    }

    /// Moves an owner's count by `delta`, dropping the entry when it reaches zero so the counter
    /// map is bounded by the live population rather than by every id ever seen.
    fn note_owned(owner_id: u16, delta: isize) {
        let mut entry = OWNED_COUNT.entry(owner_id).or_insert(0);
        *entry = entry.saturating_add_signed(delta);
        let now = *entry;
        drop(entry);
        if now == 0 {
            OWNED_COUNT.remove_if(&owner_id, |_, count| *count == 0);
        }
    }

    /// Removes an object and its ownership information from the database.
    pub fn remove_object(object_id: &str) -> bool {
        let _guard = LOCK_OBJECT.lock();
        Self::remove_object_locked(object_id)
    }

    fn remove_object_locked(object_id: &str) -> bool {
        match OWNERSHIP_BY_OBJECT_ID.remove(object_id) {
            Some((_, owner)) => {
                Self::note_owned(owner, -1);
                BNL::log(format!("Object {object_id} owned by {owner} removed from database."));
                true
            }
            None => {
                BNL::log_error(format!("Failed to remove object with ID {object_id}."));
                false
            }
        }
    }

    /// Switches the ownership of an object.
    pub fn switch_ownership(object_id: &str, new_owner_id: u16) -> bool {
        let _guard = LOCK_OBJECT.lock();
        match OWNERSHIP_BY_OBJECT_ID.get_mut(object_id) {
            Some(mut current) => {
                let current_owner_id = *current;
                *current = new_owner_id;
                drop(current);
                if current_owner_id != new_owner_id {
                    Self::note_owned(current_owner_id, -1);
                    Self::note_owned(new_owner_id, 1);
                }
                BNL::log(format!("Ownership of object {object_id} switched from {current_owner_id} to {new_owner_id}."));
                true
            }
            None => {
                Self::add_ownership(object_id, new_owner_id);
                true
            }
        }
    }

    pub fn does_object_exist_in_database(object_id: &str) -> bool {
        OWNERSHIP_BY_OBJECT_ID.contains_key(object_id)
    }

    pub fn get_ownership_information(object_id: &str) -> Option<u16> {
        OWNERSHIP_BY_OBJECT_ID.get(object_id).map(|v| *v)
    }

    pub fn print_ownership_database() {
        BNL::log("Current Ownership Database:");
        let _guard = LOCK_OBJECT.lock();
        for entry in OWNERSHIP_BY_OBJECT_ID.iter() {
            BNL::log(format!("Ownership ID: {}, Owner ID: {}", entry.key(), entry.value()));
        }
    }

    /// Removes all ownership of a specific player and notifies all clients.
    pub fn remove_player_ownership(player_id: i32) {
        let _guard = LOCK_OBJECT.lock();
        let objects_to_remove: Vec<String> = OWNERSHIP_BY_OBJECT_ID
            .iter()
            .filter(|entry| i32::from(*entry.value()) == player_id)
            .map(|entry| entry.key().clone())
            .collect();
        if objects_to_remove.is_empty() {
            return;
        }
        let mut message = OwnershipTransferMessage::default();
        let mut writer = NetworkServer::rent_writer();
        let peers = NetworkServer::peer_snapshot();
        for ownership_id in &objects_to_remove {
            if let Some((_, owner_id)) = OWNERSHIP_BY_OBJECT_ID.remove(ownership_id) {
                Self::note_owned(owner_id, -1);
                writer.reset();
                message.player_id_message = PlayerIdMessage::new(owner_id);
                message.ownership_id = ownership_id.clone();
                if message.serialize(&mut writer).is_ok() {
                    NetworkServer::broadcast_message_to_clients(
                        &writer,
                        BasisNetworkCommons::REMOVE_CURRENT_OWNER_REQUEST_CHANNEL,
                        &peers,
                        DeliveryMethod::ReliableOrdered,
                    );
                }
            }
        }
        NetworkServer::return_writer(writer);
        BNL::log(format!("Player {player_id}'s ownership removed from {} objects.", objects_to_remove.len()));
    }

    /// Drops every record. Used when the server stops and by tests.
    pub fn reset() {
        OWNERSHIP_BY_OBJECT_ID.clear();
        OWNED_COUNT.clear();
    }
}
