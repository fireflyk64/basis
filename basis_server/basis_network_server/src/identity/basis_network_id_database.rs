//! Port of `Identity/BasisNetworkIDDatabase.cs`.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use basis_error::{BasisResult, ResultExt};
use basis_network_core::BNL;
use basis_network_core::SerializableBasis::{NetIDMessage, ServerNetIDMessage, UshortUniqueIDMessage};
use basis_network_core::{BasisNetworkCommons, DeliveryMethod, NetPeerRef};
use dashmap::DashMap;

use crate::NetworkServer;

pub struct BasisNetworkIDDatabase;

/// Object unique string id → network id.
static USHORT_NETWORK_DATABASE: LazyLock<DashMap<String, u16>> = LazyLock::new(DashMap::new);
/// Start at -1 so the first increment becomes 0.
static COUNTER: AtomicI32 = AtomicI32::new(-1);
static EXHAUSTED_LOGGED: AtomicBool = AtomicBool::new(false);
/// How many ids each connected peer has been assigned this session. The shared ushort space is
/// only reclaimed when the instance empties, so without a per-peer ceiling one client can register
/// 65,536 distinct strings and permanently lock everyone else out of registering any networked
/// object. Entries are never removed individually so this count only grows during a peer's
/// session and is dropped on disconnect — it cannot drift.
static PER_PEER_ASSIGNED_COUNT: LazyLock<DashMap<i32, i32>> = LazyLock::new(DashMap::new);
/// Peers we have already warned about hitting the cap, so a client that keeps requesting after
/// the limit cannot turn one reject into a log flood.
static PER_PEER_CAP_WARNED: LazyLock<DashMap<i32, ()>> = LazyLock::new(DashMap::new);

impl BasisNetworkIDDatabase {
    const DEFAULT_MAX_NETWORK_IDS_PER_PLAYER: i32 = 32768;

    /// The C# `UshortNetworkDatabase` field.
    pub fn ushort_network_database() -> &'static DashMap<String, u16> {
        &USHORT_NETWORK_DATABASE
    }

    fn resolve_max_ids_per_player() -> i32 {
        let configured = NetworkServer::configuration().map(|c| c.max_network_ids_per_player).unwrap_or(0);
        if configured > 0 { configured } else { Self::DEFAULT_MAX_NETWORK_IDS_PER_PLAYER }
    }

    /// Drops a departed peer's per-session assignment count. The ids themselves persist until the
    /// instance empties (`reset`); this only frees the throttling counter.
    pub fn remove_peer(peer_id: i32) {
        PER_PEER_ASSIGNED_COUNT.remove(&peer_id);
        PER_PEER_CAP_WARNED.remove(&peer_id);
    }

    pub fn add_or_find_network_id(net_peer: &NetPeerRef, unique_string_id: &str) -> BasisResult<()> {
        if let Some(value) = USHORT_NETWORK_DATABASE.get(unique_string_id).map(|v| *v) {
            // We already know about it, let's just give it back to that player.
            let mut snim = ServerNetIDMessage {
                net_id_message: NetIDMessage { player_id: unique_string_id.to_string() },
                ushort_unique_id_message: UshortUniqueIDMessage { unique_id_ushort: value },
            };
            let mut writer = NetworkServer::rent_writer();
            let result = snim.serialize(&mut writer).context("serializing an existing net id");
            if result.is_ok() {
                NetworkServer::try_send(net_peer, &writer, BasisNetworkCommons::NET_ID_ASSIGN_CHANNEL, DeliveryMethod::ReliableOrdered);
            }
            NetworkServer::return_writer(writer);
            result?;
            BNL::log(format!("Sent existing NetID ({value}) for {unique_string_id} to peer {}", net_peer.id()));
            return Ok(());
        }

        // Per-peer cap: stop one client consuming the shared id space and locking everyone else
        // out. The count only grows during a session and is cleared on disconnect, so it cannot
        // drift into a false reject.
        let per_peer_cap = Self::resolve_max_ids_per_player();
        let assigned = PER_PEER_ASSIGNED_COUNT.get(&net_peer.id()).map(|c| *c).unwrap_or(0);
        if assigned >= per_peer_cap {
            if PER_PEER_CAP_WARNED.insert(net_peer.id(), ()).is_none() {
                BNL::log_error(format!(
                    "Peer {} reached the per-player network-id limit ({per_peer_cap}); dropping registration for {unique_string_id} and further ids this session.",
                    net_peer.id()
                ));
            }
            return Ok(());
        }

        BNL::log(format!("No existing ID found for {unique_string_id}. Assigning a new ID."));

        // Generate a new unique ushort ID (thread-safe increment).
        let new_counter = COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
        let Ok(new_id) = u16::try_from(new_counter) else {
            COUNTER.fetch_sub(1, Ordering::SeqCst); // Roll back
            // Log-and-drop, never throw: ids arrive per client message, so at the ceiling an error
            // per request became an exception storm through the message processor. The requester
            // simply gets no assignment.
            if !EXHAUSTED_LOGGED.swap(true, Ordering::SeqCst) {
                BNL::log_error(format!(
                    "NetID space exhausted ({} ids assigned since the server was last empty); dropping request for {unique_string_id}.",
                    u16::MAX
                ));
            }
            return Ok(());
        };

        USHORT_NETWORK_DATABASE.insert(unique_string_id.to_string(), new_id);
        *PER_PEER_ASSIGNED_COUNT.entry(net_peer.id()).or_insert(0) += 1;
        BNL::log(format!("New ID {new_id} assigned to {unique_string_id}"));

        // Notify the requesting peer and broadcast to others.
        let mut suima = ServerNetIDMessage {
            net_id_message: NetIDMessage { player_id: unique_string_id.to_string() },
            ushort_unique_id_message: UshortUniqueIDMessage { unique_id_ushort: new_id },
        };
        let mut writer = NetworkServer::rent_writer();
        let result = suima.serialize(&mut writer).context("serializing a new net id");
        if result.is_ok() {
            NetworkServer::broadcast_message_to_clients(
                &writer,
                BasisNetworkCommons::NET_ID_ASSIGN_CHANNEL,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        }
        NetworkServer::return_writer(writer);
        result?;
        BNL::log(format!("Broadcasted new ID ({new_id}) for {unique_string_id} to all connected peers."));
        Ok(())
    }

    /// The C# `GetAllNetworkID(out list)`: `None` when the database is empty.
    pub fn get_all_network_id() -> Option<Vec<ServerNetIDMessage>> {
        let messages: Vec<ServerNetIDMessage> = USHORT_NETWORK_DATABASE
            .iter()
            .map(|pair| ServerNetIDMessage {
                net_id_message: NetIDMessage { player_id: pair.key().clone() },
                ushort_unique_id_message: UshortUniqueIDMessage { unique_id_ushort: *pair.value() },
            })
            .collect();
        if messages.is_empty() { None } else { Some(messages) }
    }

    pub fn remove_ushort_network_id(net_id: u16) {
        BNL::log(format!("Attempting to remove NetID: {net_id}"));
        let key = USHORT_NETWORK_DATABASE.iter().find(|pair| *pair.value() == net_id).map(|pair| pair.key().clone());
        match key {
            Some(key) => {
                if USHORT_NETWORK_DATABASE.remove(&key).is_some() {
                    BNL::log(format!("Successfully removed NetID: {net_id} associated with UniqueStringID: {key}"));
                } else {
                    BNL::log(format!("Failed to remove NetID: {net_id} (concurrent operation may have interfered)"));
                }
            }
            None => BNL::log(format!("NetID {net_id} not found in the database.")),
        }
    }

    pub fn reset() {
        BNL::log("Resetting BasisNetworkIDDatabase...");
        USHORT_NETWORK_DATABASE.clear();
        PER_PEER_ASSIGNED_COUNT.clear();
        PER_PEER_CAP_WARNED.clear();
        COUNTER.store(-1, Ordering::SeqCst);
        EXHAUSTED_LOGGED.store(false, Ordering::SeqCst);
        BNL::log("Database reset complete. Counter set to -1.");
    }
}
