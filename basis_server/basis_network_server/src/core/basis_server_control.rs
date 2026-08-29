//! Port of `Core/BasisServerControl.cs`: the operator-facing control surface the REST API and
//! console drive.

use std::sync::Arc;
use std::time::Duration;

use basis_network_core::SerializableBasis::{AdminRequest, AdminRequestMode, LocalLoadResource, UnLoadResource};
use basis_network_core::transport::iroh_network_impl::IrohRuntime;
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod};
use tokio_util::sync::CancellationToken;

use crate::NetworkServer;
use crate::reduction::BasisServerReductionSystemEvents;
use crate::resources::{BasisNetworkPreloadResourceManagement, BasisNetworkResourceManagement};
use crate::security::{BasisPlayerModeration, PermissionIntegration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LoadStrategy {
    Immediate = 0,
    Synchronized = 2,
    Predownload = 3,
}

impl LoadStrategy {
    pub fn from_byte(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Immediate),
            2 => Some(Self::Synchronized),
            3 => Some(Self::Predownload),
            _ => None,
        }
    }

    pub fn as_byte(self) -> u8 {
        self as u8
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorldLoadParams {
    pub url: String,
    pub password: String,
    pub persistent: bool,
    pub strategy: LoadStrategy,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SwitchWorldParams {
    pub url: String,
    pub password: String,
    pub persistent: bool,
    pub announce_message: String,
    pub delay: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorldInfo {
    pub net_id: String,
    pub url: String,
    pub persistent: bool,
    pub admin_locked: bool,
    pub strategy: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerInfo {
    pub net_id: i32,
    pub uuid: String,
    pub display_name: String,
    pub platform: String,
    pub position: Option<[f32; 3]>,
}

pub trait IServerControl: Send + Sync {
    fn announce_all(&self, message: &str);
    fn announce_player(&self, uuid: &str, message: &str) -> bool;
    fn load_world(&self, p: &WorldLoadParams) -> String;
    fn unload_world(&self, net_id: &str) -> bool;
    fn clear_all_worlds(&self) -> i32;
    fn list_worlds(&self) -> Vec<WorldInfo>;
    fn list_players(&self) -> Vec<PlayerInfo>;
    fn switch_world(&self, p: &SwitchWorldParams, cancellation: CancellationToken) -> String;
}

pub struct BasisServerControl;

impl BasisServerControl {
    pub fn shared() -> Arc<dyn IServerControl> {
        Arc::new(BasisServerControl)
    }

    fn build_resource(url: &str, password: &str, persistent: bool, strategy: LoadStrategy) -> LocalLoadResource {
        LocalLoadResource {
            loaded_net_id: uuid::Uuid::new_v4().simple().to_string(),
            mode: 1,
            combined_url: url.to_string(),
            unlock_password: password.to_string(),
            uuid_of_creator: "server".to_string(),
            is_admin_locked: true,
            persist: persistent,
            load_strategy: strategy.as_byte(),
            ..Default::default()
        }
    }
}

impl IServerControl for BasisServerControl {
    fn announce_all(&self, message: &str) {
        let mut writer = NetworkServer::rent_writer();
        let written = AdminRequest::default().serialize(&mut writer, AdminRequestMode::MessageAll).and_then(|_| writer.put_string(message));
        if written.is_ok() {
            NetworkServer::broadcast_message_to_clients(
                &writer,
                BasisNetworkCommons::ADMIN_CHANNEL,
                &NetworkServer::peer_snapshot(),
                DeliveryMethod::ReliableOrdered,
            );
        }
        NetworkServer::return_writer(writer);
        BNL::log(format!("[Control] Announced to all: {message}"));
    }

    fn announce_player(&self, uuid: &str, message: &str) -> bool {
        let Some(peer) = NetworkServer::uuid_to_net_id(uuid).and_then(|id| NetworkServer::authenticated_peers().get(&id).map(|p| p.value().clone()))
        else {
            return false;
        };
        BasisPlayerModeration::send_back_message(&peer, message);
        BNL::log(format!("[Control] Announced to {uuid}: {message}"));
        true
    }

    fn load_world(&self, p: &WorldLoadParams) -> String {
        let resource = Self::build_resource(&p.url, &p.password, p.persistent, p.strategy);
        let net_id = resource.loaded_net_id.clone();
        match p.strategy {
            LoadStrategy::Synchronized => BasisNetworkPreloadResourceManagement::start_synchronized_load(resource),
            LoadStrategy::Predownload => BasisNetworkResourceManagement::predownload_resource(resource),
            LoadStrategy::Immediate => BasisNetworkResourceManagement::load_resource(resource),
        }
        BNL::log(format!("[Control] Load world: {} strategy={:?} netId={net_id}", p.url, p.strategy));
        net_id
    }

    fn unload_world(&self, net_id: &str) -> bool {
        BasisNetworkResourceManagement::unload_resource_server(&mut UnLoadResource { loaded_net_id: net_id.to_string(), mode: 1 })
    }

    fn clear_all_worlds(&self) -> i32 {
        let peers = NetworkServer::peer_snapshot();
        let scenes: Vec<LocalLoadResource> = BasisNetworkResourceManagement::ushort_network_database()
            .iter()
            .filter(|e| e.value().mode == 1)
            .map(|e| e.value().clone())
            .collect();
        let mut count = 0;
        let mut writer = NetworkServer::rent_writer();
        for scene in scenes {
            if let Some((_, removed)) = BasisNetworkResourceManagement::ushort_network_database().remove(&scene.loaded_net_id) {
                BasisNetworkResourceManagement::note_resource_removed(&removed.uuid_of_creator);
            }
            let mut unload = UnLoadResource { loaded_net_id: scene.loaded_net_id.clone(), mode: 1 };
            writer.reset();
            if unload.serialize(&mut writer).is_ok() {
                NetworkServer::broadcast_message_to_clients(&writer, BasisNetworkCommons::UNLOAD_RESOURCE_CHANNEL, &peers, DeliveryMethod::ReliableOrdered);
            }
            count += 1;
        }
        NetworkServer::return_writer(writer);

        // Reset after removal so any synchronized load that slipped in during the loop has its
        // session cleared rather than left pending.
        BasisNetworkPreloadResourceManagement::reset();

        let mut clear_writer = NetworkServer::rent_writer();
        if AdminRequest::default().serialize(&mut clear_writer, AdminRequestMode::ClearAllScenes).is_ok() {
            NetworkServer::broadcast_message_to_clients(&clear_writer, BasisNetworkCommons::ADMIN_CHANNEL, &peers, DeliveryMethod::ReliableOrdered);
        }
        NetworkServer::return_writer(clear_writer);
        BNL::log(format!("[Control] ClearAllWorlds: unloaded {count} scene(s)"));
        count
    }

    fn list_worlds(&self) -> Vec<WorldInfo> {
        let mut worlds: Vec<WorldInfo> = BasisNetworkResourceManagement::ushort_network_database()
            .iter()
            .filter(|e| e.value().mode == 1)
            .map(|e| {
                let r = e.value();
                WorldInfo {
                    net_id: r.loaded_net_id.clone(),
                    url: r.combined_url.clone(),
                    persistent: r.persist,
                    admin_locked: r.is_admin_locked,
                    strategy: r.load_strategy,
                }
            })
            .collect();
        worlds.sort_by(|a, b| a.net_id.cmp(&b.net_id));
        worlds
    }

    fn list_players(&self) -> Vec<PlayerInfo> {
        let mut result = Vec::new();
        for entry in NetworkServer::authenticated_peers().iter() {
            let net_id = *entry.key();
            let uuid = NetworkServer::net_id_to_uuid(entry.value()).unwrap_or_default();
            let (display_name, platform) = if uuid.is_empty() {
                (String::new(), String::new())
            } else {
                PermissionIntegration::try_get_player_meta(&uuid).map(|m| (m.player_display_name, m.player_platform)).unwrap_or_default()
            };
            let position = BasisServerReductionSystemEvents::try_get_active_position(net_id).map(|p| [p.x, p.y, p.z]);
            result.push(PlayerInfo { net_id, uuid, display_name, platform, position });
        }
        result.sort_by_key(|p| p.net_id);
        result
    }

    fn switch_world(&self, p: &SwitchWorldParams, cancellation: CancellationToken) -> String {
        let resource = Self::build_resource(&p.url, &p.password, p.persistent, LoadStrategy::Synchronized);
        let net_id = resource.loaded_net_id.clone();
        if !p.announce_message.is_empty() {
            self.announce_all(&p.announce_message);
        }
        if p.delay > 0 {
            let url = p.url.clone();
            let delay = Duration::from_secs(u64::try_from(p.delay).unwrap_or(0));
            let delayed_net_id = net_id.clone();
            IrohRuntime::spawn_detached(async move {
                tokio::select! {
                    _ = cancellation.cancelled() => {}
                    _ = tokio::time::sleep(delay) => {
                        BasisNetworkPreloadResourceManagement::start_synchronized_load(resource);
                        BNL::log(format!("[Control] Switch world started (post-delay): {url} netId={delayed_net_id}"));
                    }
                }
            });
        } else {
            BasisNetworkPreloadResourceManagement::start_synchronized_load(resource);
        }
        BNL::log(format!("[Control] Switch world queued: {} netId={net_id} delay={}s", p.url, p.delay));
        net_id
    }
}
