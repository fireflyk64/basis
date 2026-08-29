//! Port of `Resources/BasisNetworkPreloadResourceManagement.cs`: server-side tracking for
//! synchronized resource loads. Tracks which clients have reported readiness and triggers the
//! spawn signal when all are ready or the timeout expires.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use basis_network_core::SerializableBasis::{LocalLoadResource, SpawnPreloadedMessage, UnLoadResource};
use basis_network_core::transport::iroh_network_impl::IrohRuntime;
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetPeerRef};
use dashmap::DashMap;
use parking_lot::Mutex;
use tokio::task::AbortHandle;

use crate::NetworkServer;
use crate::resources::BasisNetworkResourceManagement;

pub struct SyncLoadSession {
    pub resource: LocalLoadResource,
    pub ready_peers: HashSet<i32>,
    pub failed_peers: HashSet<i32>,
    pub start_time: Instant,
    /// Total number of connected peers when this session started.
    pub total_peer_count: i32,
    timeout: Option<AbortHandle>,
}

impl SyncLoadSession {
    pub fn is_complete(&self) -> bool {
        (self.ready_peers.len() + self.failed_peers.len()) as i32 >= self.total_peer_count
    }

    fn cancel_timeout(&mut self) {
        if let Some(timeout) = self.timeout.take() {
            timeout.abort();
        }
    }
}

/// Active synchronized load sessions, keyed by LoadedNetID.
static ACTIVE_SESSIONS: LazyLock<DashMap<String, Arc<Mutex<SyncLoadSession>>>> = LazyLock::new(DashMap::new);

pub struct BasisNetworkPreloadResourceManagement;

impl BasisNetworkPreloadResourceManagement {
    /// Timeout for synchronized loads. After this duration the server sends the spawn signal
    /// regardless of how many clients have reported.
    pub const SYNCHRONIZED_TIMEOUT: Duration = Duration::from_secs(5 * 60);

    pub fn active_sessions() -> &'static DashMap<String, Arc<Mutex<SyncLoadSession>>> {
        &ACTIVE_SESSIONS
    }

    /// Bumps a session's expected peer count (a late joiner). True when the session exists.
    pub fn add_late_joiner(loaded_net_id: &str) -> bool {
        match ACTIVE_SESSIONS.get(loaded_net_id) {
            Some(session) => {
                session.lock().total_peer_count += 1;
                true
            }
            None => false,
        }
    }

    /// Called when the server receives a LoadResource with LoadStrategy = 2 (Synchronized).
    /// Broadcasts the preload request to all clients and starts tracking readiness.
    pub fn start_synchronized_load(mut resource: LocalLoadResource) {
        let net_id = resource.loaded_net_id.clone();
        if ACTIVE_SESSIONS.contains_key(&net_id) {
            BNL::log_error(format!("PreloadResourceManagement: Session already exists for {net_id}"));
            return;
        }
        if !BasisNetworkResourceManagement::can_creator_load_more(&resource.uuid_of_creator) {
            BNL::log_error(format!(
                "PreloadResourceManagement: Creator {} reached the per-player loaded-object limit; dropping synchronized load {net_id}.",
                resource.uuid_of_creator
            ));
            return;
        }

        let peer_snapshot = NetworkServer::peer_snapshot();
        let peer_count = peer_snapshot.len() as i32;
        let session = Arc::new(Mutex::new(SyncLoadSession {
            resource: resource.clone(),
            ready_peers: HashSet::new(),
            failed_peers: HashSet::new(),
            start_time: Instant::now(),
            total_peer_count: peer_count,
            timeout: None,
        }));
        match ACTIVE_SESSIONS.entry(net_id.clone()) {
            dashmap::mapref::entry::Entry::Occupied(_) => {
                BNL::log_error(format!("PreloadResourceManagement: Failed to add session for {net_id}"));
                return;
            }
            dashmap::mapref::entry::Entry::Vacant(slot) => {
                slot.insert(session.clone());
            }
        }
        BNL::log(format!("PreloadResourceManagement: Starting synchronized load for {net_id}, {peer_count} peers"));

        // Broadcast the load resource to all clients (they will see LoadStrategy = 2 and handle
        // it as a synchronized preload)
        let mut writer = NetworkServer::rent_writer();
        if resource.serialize(&mut writer).is_ok() {
            NetworkServer::broadcast_message_to_clients(&writer, BasisNetworkCommons::LOAD_RESOURCE_CHANNEL, &peer_snapshot, DeliveryMethod::ReliableOrdered);
        }
        NetworkServer::return_writer(writer);

        // Store in the main resource database too
        if BasisNetworkResourceManagement::try_add_to_database(resource.clone()) {
            BasisNetworkResourceManagement::note_resource_added(&resource.uuid_of_creator);
        }

        // No peers: complete immediately rather than waiting for the 5-minute timeout
        if peer_count == 0 {
            BNL::log(format!("PreloadResourceManagement: No peers connected, completing {net_id} immediately"));
            Self::broadcast_spawn_signal(&net_id);
            return;
        }

        // Start timeout task
        let timeout_net_id = net_id.clone();
        let timeout_session = session.clone();
        match IrohRuntime::spawn(async move {
            tokio::time::sleep(Self::SYNCHRONIZED_TIMEOUT).await;
            let (ready, failed, total) = {
                let s = timeout_session.lock();
                (s.ready_peers.len(), s.failed_peers.len(), s.total_peer_count)
            };
            BNL::log(format!("PreloadResourceManagement: Timeout reached for {timeout_net_id}. Ready: {ready}, Failed: {failed}, Total: {total}"));
            Self::broadcast_spawn_signal(&timeout_net_id);
        }) {
            Ok(handle) => session.lock().timeout = Some(handle.abort_handle()),
            Err(e) => {
                // Without a timer the session would wait forever on a peer that never answers.
                BNL::log_error(format!("PreloadResourceManagement: could not start the timeout for {net_id}: {e}; completing now"));
                Self::broadcast_spawn_signal(&net_id);
            }
        }
    }

    /// Called when the server receives a PreloadReady message from a client.
    pub fn handle_client_ready(loaded_net_id: &str, peer_id: i32, is_ready: bool) {
        let Some(session) = ACTIVE_SESSIONS.get(loaded_net_id).map(|s| s.clone()) else {
            BNL::log_error(format!("PreloadResourceManagement: Received ready from peer {peer_id} for unknown session {loaded_net_id}"));
            return;
        };
        let complete = {
            let mut s = session.lock();
            if is_ready {
                s.ready_peers.insert(peer_id);
                BNL::log(format!(
                    "PreloadResourceManagement: Peer {peer_id} ready for {loaded_net_id} ({}/{})",
                    s.ready_peers.len() + s.failed_peers.len(),
                    s.total_peer_count
                ));
            } else {
                s.failed_peers.insert(peer_id);
                BNL::log(format!(
                    "PreloadResourceManagement: Peer {peer_id} FAILED for {loaded_net_id} ({}/{})",
                    s.ready_peers.len() + s.failed_peers.len(),
                    s.total_peer_count
                ));
            }
            s.is_complete()
        };
        if complete {
            BNL::log(format!("PreloadResourceManagement: All peers reported for {loaded_net_id}, sending spawn signal"));
            Self::broadcast_spawn_signal(loaded_net_id);
        }
    }

    /// Broadcasts the spawn signal to all connected clients for a synchronized load. Also
    /// broadcasts unload messages for existing scenes through the normal unload path so the
    /// server tracking stays consistent.
    fn broadcast_spawn_signal(loaded_net_id: &str) {
        let Some((_, session)) = ACTIVE_SESSIONS.remove(loaded_net_id) else {
            return;
        };
        let mode = {
            let mut s = session.lock();
            s.cancel_timeout();
            s.resource.mode
        };
        let peer_snapshot = NetworkServer::peer_snapshot();

        // Only unload existing scenes when the synchronized resource is itself a scene. Props
        // (Mode == 0) should never cause scene unloads. Exclude loaded_net_id — that is the scene
        // being switched TO; removing it would race against the SpawnPreloaded signal.
        if mode == 1 {
            Self::unload_all_scene_resources(&peer_snapshot, Some(loaded_net_id));
        }

        let mut spawn_msg = SpawnPreloadedMessage { loaded_net_id: loaded_net_id.to_string() };
        let mut writer = NetworkServer::rent_writer();
        if spawn_msg.serialize(&mut writer).is_ok() {
            NetworkServer::broadcast_message_to_clients(&writer, BasisNetworkCommons::SPAWN_PRELOADED_CHANNEL, &peer_snapshot, DeliveryMethod::ReliableOrdered);
        }
        NetworkServer::return_writer(writer);
        BNL::log(format!("PreloadResourceManagement: Spawn signal sent for {loaded_net_id}"));
    }

    /// Unloads all scene-type resources (Mode == 1) from the server database and broadcasts
    /// unload messages to all clients through the normal unload channel.
    fn unload_all_scene_resources(peer_snapshot: &[NetPeerRef], exclude_net_id: Option<&str>) {
        let scene_resources: Vec<LocalLoadResource> = BasisNetworkResourceManagement::ushort_network_database()
            .iter()
            .filter(|e| e.value().mode == 1 && Some(e.value().loaded_net_id.as_str()) != exclude_net_id)
            .map(|e| e.value().clone())
            .collect();
        if scene_resources.is_empty() {
            return;
        }
        BNL::log(format!("PreloadResourceManagement: Unloading {} existing scene(s) before synchronized spawn", scene_resources.len()));

        let mut writer = NetworkServer::rent_writer();
        for scene in scene_resources {
            if BasisNetworkResourceManagement::ushort_network_database().remove(&scene.loaded_net_id).is_some() {
                BasisNetworkResourceManagement::note_resource_removed(&scene.uuid_of_creator);
            }
            let mut unload = UnLoadResource { loaded_net_id: scene.loaded_net_id.clone(), mode: 1 };
            writer.reset();
            if unload.serialize(&mut writer).is_ok() {
                NetworkServer::broadcast_message_to_clients(&writer, BasisNetworkCommons::UNLOAD_RESOURCE_CHANNEL, peer_snapshot, DeliveryMethod::ReliableOrdered);
            }
            BNL::log(format!("PreloadResourceManagement: Unloaded scene {}", scene.loaded_net_id));
        }
        NetworkServer::return_writer(writer);
    }

    /// Removes a disconnected peer from all active synchronized load sessions. Decrements the
    /// expected peer count and triggers the spawn signal if all remaining peers have already
    /// reported.
    pub fn remove_peer(peer_id: i32) {
        let mut completed: Vec<String> = Vec::new();
        let mut emptied: Vec<String> = Vec::new();
        for entry in ACTIVE_SESSIONS.iter() {
            let mut s = entry.value().lock();
            s.ready_peers.remove(&peer_id);
            s.failed_peers.remove(&peer_id);
            if s.total_peer_count > 0 {
                s.total_peer_count -= 1;
            }
            if s.total_peer_count <= 0 {
                // No peers left, just clean up
                s.cancel_timeout();
                emptied.push(entry.key().clone());
            } else if s.is_complete() {
                completed.push(entry.key().clone());
            }
        }
        for net_id in emptied {
            ACTIVE_SESSIONS.remove(&net_id);
        }
        for net_id in completed {
            if ACTIVE_SESSIONS.contains_key(&net_id) {
                BNL::log(format!("PreloadResourceManagement: All remaining peers reported for {net_id} after peer {peer_id} disconnected, sending spawn signal"));
                Self::broadcast_spawn_signal(&net_id);
            }
        }
    }

    /// Cleans up all active sessions. Called on server reset.
    pub fn reset() {
        for entry in ACTIVE_SESSIONS.iter() {
            entry.value().lock().cancel_timeout();
        }
        ACTIVE_SESSIONS.clear();
    }
}
