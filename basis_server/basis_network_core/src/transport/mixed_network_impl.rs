//! The mixed stack: one server that listens on iroh *and* on the LiteNetLib protocol at once,
//! so the existing C# clients and the new iroh clients share one world.
//!
//! The two managers are ordinary [`IrohNetManager`] and [`LnlNetManager`] instances raising
//! events on the same listener. What makes them one server rather than two:
//!
//! * **one peer-id space** — both draw from one [`PeerIdAllocator`], so a player id names one
//!   player whichever transport carries them, and everything above the transport (the
//!   authenticated-peer table, the reduction system, the P2P broker) works unchanged;
//! * **one identity space** — peer identities come from the process-wide counter, so
//!   [`peers_equal`](crate::transport::basis_network_shell::peers_equal) never confuses a
//!   legacy peer with an iroh one;
//! * **the server in the middle** — a legacy peer reports
//!   [`direct_link_capable`](NetPeer::direct_link_capable) = false, so the P2P broker declines
//!   any direct link that names it and the server keeps relaying between the two worlds.
//!
//! # Ports
//!
//! The LiteNetLib listener takes the configured `SetPort` (4296 by default), because that is
//! the port every deployed client and firewall rule already knows. iroh takes
//! `IrohTransportConfig.Port`, or `SetPort + 1` when that is 0, and its endpoint id plus that
//! port is the connection string new clients use. Starting on port 0 gives both an OS-picked
//! port.

use std::any::Any;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use basis_error::BasisResult;

use crate::configuration::{BasisTransportConfigStore, Configuration, IrohTransportConfig, LNLTransportConfig};
use crate::io::NetDataWriter;

use super::basis_network_shell::{EventBasedNetListener, NetManager, NetManagerRef, NetPeerRef, NetStatistics, PeerIdAllocator};
use super::basis_network_stack_registry::{BasisNetworkStackRegistry, ServerProbeResult};
use super::connection_target::{ConnectionTarget, ConnectionTargetKeys, IConnectionTargetParser};
use super::iroh_connection_target_parser::IrohConnectionTargetParser;
use super::iroh_network_impl::IrohNetManager;
use super::lnl_connection_target_parser::LNLConnectionTargetParser;
use super::lnl_network_impl::LnlNetManager;

pub struct MixedNetManager {
    iroh: IrohNetManager,
    lnl: LnlNetManager,
    ids: Arc<PeerIdAllocator>,
    iroh_port: u16,
}

impl MixedNetManager {
    /// The stack registry's factory: both transports from their sidecars, one id allocator.
    pub fn create(listener: Arc<EventBasedNetListener>, configuration: &Configuration) -> Option<NetManagerRef> {
        let iroh_config = BasisTransportConfigStore::get::<IrohTransportConfig>(BasisNetworkStackRegistry::IROH_ID);
        let lnl_config = BasisTransportConfigStore::get::<LNLTransportConfig>(BasisNetworkStackRegistry::LITE_NET_LIB_ID);
        Some(Arc::new(Self::new(listener, iroh_config, &lnl_config, configuration.enable_statistics)))
    }

    pub fn new(listener: Arc<EventBasedNetListener>, iroh_config: IrohTransportConfig, lnl_config: &LNLTransportConfig, enable_statistics: bool) -> Self {
        let ids = PeerIdAllocator::new();
        let iroh_port = iroh_config.port;
        let iroh = IrohNetManager::with_id_allocator(listener.clone(), iroh_config, enable_statistics, None, ids.clone());
        let lnl = LnlNetManager::with_id_allocator(listener, lnl_config, enable_statistics, ids.clone());
        Self { iroh, lnl, ids, iroh_port }
    }

    pub fn iroh(&self) -> &IrohNetManager {
        &self.iroh
    }

    pub fn lnl(&self) -> &LnlNetManager {
        &self.lnl
    }

    /// `<endpoint-id>@host:port` — what an iroh client connects with.
    pub fn connection_string(&self) -> String {
        self.iroh.connection_string()
    }

    /// The UDP port legacy LiteNetLib clients connect to.
    pub fn legacy_port(&self) -> u16 {
        self.lnl.local_port()
    }

    /// The port iroh binds for a given `SetPort`: the configured one, else the next port up
    /// (an OS-picked one when `SetPort` itself is 0).
    pub fn iroh_port_for(set_port: u16, configured: u16) -> u16 {
        if configured != 0 {
            configured
        } else if set_port == 0 {
            0
        } else {
            set_port.wrapping_add(1)
        }
    }

    /// Whether a connection string names an iroh endpoint (an endpoint id, with or without an
    /// `@host:port`) rather than a plain LiteNetLib `host:port`.
    pub fn is_iroh_target(target: &str) -> bool {
        let left = target.split('#').next().unwrap_or(target);
        left.contains('@') || IrohConnectionTargetParser::looks_like_endpoint_id(left)
    }

    /// Probes whichever stack the target names.
    pub async fn probe(target: ConnectionTarget, timeout_ms: i32) -> ServerProbeResult {
        if Self::is_iroh_target(&target.raw) || target.get(ConnectionTargetKeys::ENDPOINT_ID).is_some() {
            IrohNetManager::probe(target, timeout_ms).await
        } else {
            LnlNetManager::probe(target, timeout_ms).await
        }
    }
}

impl NetManager for MixedNetManager {
    fn start(&self, ipv4_address: IpAddr, ipv6_address: IpAddr, set_port: u16) -> BasisResult<()> {
        self.lnl.start(ipv4_address, ipv6_address, set_port).map_err(|e| e.context("starting the legacy (LiteNetLib) listener"))?;
        let iroh_port = Self::iroh_port_for(set_port, self.iroh_port);
        if let Err(e) = self.iroh.start(ipv4_address, ipv6_address, iroh_port) {
            // Half a server is worse than none: the caller sees one failure and nothing listening.
            self.lnl.stop();
            return Err(e.context("starting the iroh listener"));
        }
        Ok(())
    }

    fn stop(&self) {
        self.iroh.stop();
        self.lnl.stop();
        self.ids.reset();
    }

    fn connect(&self, target: &str, port: u16, writer: &NetDataWriter) -> BasisResult<NetPeerRef> {
        if Self::is_iroh_target(target) {
            self.iroh.connect(target, port, writer)
        } else {
            self.lnl.connect(target, port, writer)
        }
    }

    fn send_unconnected_message(&self, writer: &NetDataWriter, remote_end_point: SocketAddr) -> bool {
        // An iroh probe reply is keyed by the address its handler saw; anything else is a
        // LiteNetLib unconnected datagram.
        self.iroh.send_unconnected_message(writer, remote_end_point) || self.lnl.send_unconnected_message(writer, remote_end_point)
    }

    fn statistics(&self) -> NetStatistics {
        let a = self.iroh.statistics();
        let b = self.lnl.statistics();
        NetStatistics {
            packets_sent: a.packets_sent + b.packets_sent,
            packets_received: a.packets_received + b.packets_received,
            bytes_sent: a.bytes_sent + b.bytes_sent,
            bytes_received: a.bytes_received + b.bytes_received,
            packet_loss: a.packet_loss + b.packet_loss,
        }
    }

    fn connected_peers_count(&self) -> i32 {
        self.iroh.connected_peers_count().saturating_add(self.lnl.connected_peers_count())
    }

    fn unreliable_dropped(&self) -> i64 {
        self.iroh.unreliable_dropped().saturating_add(self.lnl.unreliable_dropped())
    }

    fn priority_unreliable_dropped(&self) -> i64 {
        self.iroh.priority_unreliable_dropped().saturating_add(self.lnl.priority_unreliable_dropped())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Parses a connection string for whichever stack it names.
#[derive(Clone, Copy, Debug, Default)]
pub struct MixedConnectionTargetParser;

impl IConnectionTargetParser for MixedConnectionTargetParser {
    fn parse(&self, target: &mut ConnectionTarget) {
        if MixedNetManager::is_iroh_target(&target.raw) {
            IrohConnectionTargetParser.parse(target);
        } else {
            LNLConnectionTargetParser.parse(target);
        }
    }

    fn format(&self, target: &ConnectionTarget) -> String {
        if target.get(ConnectionTargetKeys::ENDPOINT_ID).is_some() {
            IrohConnectionTargetParser.format(target)
        } else {
            LNLConnectionTargetParser.format(target)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iroh_port_follows_the_legacy_port_unless_configured() {
        assert_eq!(MixedNetManager::iroh_port_for(4296, 0), 4297);
        assert_eq!(MixedNetManager::iroh_port_for(4296, 5000), 5000);
        assert_eq!(MixedNetManager::iroh_port_for(0, 0), 0);
        assert_eq!(MixedNetManager::iroh_port_for(0, 7), 7);
    }

    #[test]
    fn targets_are_routed_by_shape() {
        assert!(!MixedNetManager::is_iroh_target("127.0.0.1:4296"));
        assert!(!MixedNetManager::is_iroh_target("example.com"));
        assert!(!MixedNetManager::is_iroh_target("[::1]:4296#pw"));
        assert!(MixedNetManager::is_iroh_target("ybndrfg8ejkmcpqxot1uwisza345h769ybndrfg8ejkmcpqxot1u@127.0.0.1:4297"));
        assert!(MixedNetManager::is_iroh_target("ybndrfg8ejkmcpqxot1uwisza345h769ybndrfg8ejkmcpqxot1u#pw"));
        assert!(MixedNetManager::is_iroh_target(&"a".repeat(64)));
    }
}
