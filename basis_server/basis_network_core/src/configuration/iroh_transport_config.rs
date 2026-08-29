//! Sidecar for the iroh network stack (`config/transports/iroh.xml`).

use crate::basis_xml_config;

use super::ConfigMigration;

basis_xml_config! {
    pub struct IrohTransportConfig ("IrohTransportConfig", IrohTransportConfig::CURRENT_CONFIG_VERSION) {
        /// Schema version stamped into the file; 0 = pre-versioning, upgraded on load.
        pub config_version: i32 = 0 => "ConfigVersion" [Int],
        /// UDP port the iroh endpoint binds. 0 = SetPort when iroh is the only stack, SetPort + 1
        /// on the mixed stack (LiteNetLib keeps SetPort, the port every deployed client knows).
        pub port: u16 = 0 => "Port" [UShort],
        /// "default" (n0 relays), "disabled" (direct only) or "custom" (RelayUrls).
        pub relay_mode: String = "default".to_string() => "RelayMode" [Str],
        pub relay_urls: String = String::new() => "RelayUrls" [Str],
        pub secret_key_file: String = "iroh-secret.key".to_string() => "SecretKeyFile" [Str],
        pub publish_address: bool = false => "PublishAddress" [Bool],
        pub idle_timeout_ms: i32 = 30000 => "IdleTimeoutMs" [Int],
        pub keep_alive_interval_ms: i32 = 0 => "KeepAliveIntervalMs" [Int],
        pub max_datagram_queue_per_peer: i32 = 0 => "MaxDatagramQueuePerPeer" [Int],
        pub max_priority_datagram_queue_per_peer: i32 = 0 => "MaxPriorityDatagramQueuePerPeer" [Int],
        pub tokio_worker_threads: i32 = 0 => "TokioWorkerThreads" [Int],
    }
}

impl IrohTransportConfig {
    pub const CURRENT_CONFIG_VERSION: i32 = 2;

    pub fn relay_urls_list(&self) -> Vec<String> {
        self.relay_urls
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

impl ConfigMigration for IrohTransportConfig {
    fn migrate_from(&mut self, _loaded_version: i32) {}
}
