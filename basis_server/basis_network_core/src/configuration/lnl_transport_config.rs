//! The LiteNetLib transport sidecar. The Rust server ships the iroh transport, but this stays: it
//! is the file every deployed server already has, its NAT/queue knobs are read by the P2P broker
//! and population scaling, and the API-compatible LiteNetLib-protocol transport planned for the
//! C# clients will read the rest.

use crate::BNL;
use crate::basis_xml_config;

basis_xml_config! {
    pub struct LNLTransportConfig ("LNLTransportConfig", LNLTransportConfig::CURRENT_CONFIG_VERSION) {
        /// Schema version stamped into the file; 0 = pre-versioning, upgraded on load.
        pub config_version: i32 = 0 => "ConfigVersion" [Int],
        pub use_native_sockets: bool = true => "UseNativeSockets" [Bool],
        pub nat_punch_enabled: bool = true => "NatPunchEnabled" [Bool],
        pub nat_port_prediction_range: i32 = 32 => "NatPortPredictionRange" [Int],
        pub ping_interval: i32 = 1500 => "PingInterval" [Int],
        pub disconnect_timeout: i32 = 30000 => "DisconnectTimeout" [Int],
        pub simulate_packet_loss: bool = false => "SimulatePacketLoss" [Bool],
        pub simulate_latency: bool = false => "SimulateLatency" [Bool],
        pub simulation_packet_loss_chance: i32 = 10 => "SimulationPacketLossChance" [Int],
        pub simulation_min_latency: i32 = 50 => "SimulationMinLatency" [Int],
        pub simulation_max_latency: i32 = 150 => "SimulationMaxLatency" [Int],
        pub reconnect_delay: i32 = 500 => "ReconnectDelay" [Int],
        pub max_connect_attempts: i32 = 10 => "MaxConnectAttempts" [Int],
        pub reuse_addresss: bool = false => "ReuseAddresss" [Bool],
        pub dont_route: bool = false => "DontRoute" [Bool],
        pub i_pv6_enabled: bool = true => "IPv6Enabled" [Bool],
        pub mtu_override: i32 = 0 => "MtuOverride" [Int],
        pub mtu_discovery: bool = true => "MtuDiscovery" [Bool],
        pub disconnect_on_unreachable: bool = false => "DisconnectOnUnreachable" [Bool],
        pub allow_peer_address_change: bool = true => "AllowPeerAddressChange" [Bool],
        pub multi_socket_count: i32 = 1 => "MultiSocketCount" [Int],
        pub max_send_sockets: i32 = 0 => "MaxSendSockets" [Int],
        pub packet_pool_size_per_peer: i32 = 48 => "PacketPoolSizePerPeer" [Int],
        pub packet_pool_size_max: i32 = 0 => "PacketPoolSizeMax" [Int],
        pub merge_hold_ms: f32 = 3.0 => "MergeHoldMs" [Float],
        pub compact_merged: bool = true => "CompactMerged" [Bool],
        pub peer_update_parallelism: i32 = 0 => "PeerUpdateParallelism" [Int],
        pub peer_update_peers_per_worker: i32 = 0 => "PeerUpdatePeersPerWorker" [Int],
        pub max_unreliable_queue_per_peer: i32 = 0 => "MaxUnreliableQueuePerPeer" [Int],
        pub max_priority_unreliable_queue_per_peer: i32 = 0 => "MaxPriorityUnreliableQueuePerPeer" [Int],
    }
}
impl LNLTransportConfig {
    /// Bump to force existing files to be rewritten; newly-added fields are healed automatically on load.
    pub const CURRENT_CONFIG_VERSION: i32 = 10;

    /// Values written by version 7 and earlier that version 8 replaces with auto-scaling.
    const LEGACY_MAX_UNRELIABLE_QUEUE_PER_PEER: i32 = 256;
    const LEGACY_PACKET_POOL_SIZE_MAX: i32 = 262144;

    /// Version 8 turned two fixed ceilings into auto-scaled ones. Both were written explicitly
    /// into every existing file, so the normal "add missing settings" upgrade would have left
    /// every deployed server on the old values. Only the exact shipped defaults are retired.
    pub fn migrate_from(&mut self, loaded_version: i32) {
        if loaded_version >= 8 {
            return;
        }
        if self.max_unreliable_queue_per_peer == Self::LEGACY_MAX_UNRELIABLE_QUEUE_PER_PEER {
            self.max_unreliable_queue_per_peer = 0;
            BNL::log("[Config] MaxUnreliableQueuePerPeer was the old fixed 256, which sheds heavily past ~1000 players; switching it to automatic sizing.");
        }
        if self.packet_pool_size_max == Self::LEGACY_PACKET_POOL_SIZE_MAX {
            self.packet_pool_size_max = 0;
            BNL::log("[Config] PacketPoolSizeMax was the old fixed 262144, which caps the pool from ~5400 peers upward; switching it to automatic sizing.");
        }
    }
}
