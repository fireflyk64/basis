//! Port of `Messaging/BasisNetworkMessageProcessor.cs`: the per-packet entry point.

use std::sync::LazyLock;

use basis_network_core::statistics::basis_network_statistics::BasisNetworkStatistics;
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetPacketReader, NetPeerRef};
use dashmap::DashMap;

use crate::NetworkServer;
use crate::messaging::BasisServerMessageRegistry;
use crate::security::BasisPlayerModeration;

static PEER_ERROR_COUNTS: LazyLock<DashMap<i32, i32>> = LazyLock::new(DashMap::new);

pub struct BasisNetworkMessageProcessor;

impl BasisNetworkMessageProcessor {
    const MAX_ERRORS_BEFORE_WARNING: i32 = 50;
    /// Protocol errors tolerated from one peer before it is dropped.
    pub const MAX_ERRORS_BEFORE_DISCONNECT: i32 = 500;

    pub fn clear_peer_errors(peer_id: i32) {
        PEER_ERROR_COUNTS.remove(&peer_id);
    }

    pub fn peer_error_count(peer_id: i32) -> i32 {
        PEER_ERROR_COUNTS.get(&peer_id).map(|c| *c).unwrap_or(0)
    }

    fn bump_errors(peer_id: i32) -> i32 {
        let mut entry = PEER_ERROR_COUNTS.entry(peer_id).or_insert(0);
        *entry += 1;
        *entry
    }

    pub fn process_message(peer: &NetPeerRef, reader: NetPacketReader, channel: u8, delivery_method: DeliveryMethod) {
        BasisNetworkStatistics::record_inbound(channel, reader.available_bytes());
        if channel != BasisNetworkCommons::AUTH_IDENTITY_CHANNEL && !NetworkServer::is_authenticated_peer(peer) {
            let pre_auth_errors = Self::bump_errors(peer.id());
            if pre_auth_errors <= 5 || pre_auth_errors % 100 == 0 {
                BNL::log_error(format!(
                    "Pre-auth message on channel {channel} from peer {} before authentication (error #{pre_auth_errors}).",
                    peer.id()
                ));
            }
            return;
        }

        let available = reader.available_bytes();
        // A handler that panics is a server bug, never a reason to lose the transport thread:
        // it is counted against the peer like the C# exception path and the loop goes on.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(handler) = BasisServerMessageRegistry::resolve_core(channel) {
                handler(peer, reader, channel, delivery_method);
                Ok(())
            } else if BasisNetworkCommons::is_plugin_channel(channel) {
                if BasisServerMessageRegistry::dispatch_plugin(peer, reader, channel, delivery_method) { Ok(()) } else { Err("plugin id") }
            } else {
                Err("channel")
            }
        }));
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(kind)) => Self::handle_unknown(peer, available, channel, kind),
            Err(panic) => {
                let message = panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "non-string panic payload".to_string());
                let error_count = Self::bump_errors(peer.id());
                if error_count <= 5 || error_count % 100 == 0 {
                    BNL::log_error(format!(
                        "[Error] Panic in process_message (error #{error_count})\nPeer: {}, Channel: {channel}, Delivery: {delivery_method:?}\nMessage: {message}",
                        peer.id()
                    ));
                }
                Self::handle_error_escalation(peer, error_count);
            }
        }
    }

    fn handle_unknown(peer: &NetPeerRef, available_bytes: usize, channel: u8, kind: &str) {
        let error_count = Self::bump_errors(peer.id());
        if error_count <= 5 || error_count % 100 == 0 {
            BNL::log_error(format!(
                "Unknown {kind}: {channel} ({available_bytes} bytes remaining) from peer {} (error #{error_count})",
                peer.id()
            ));
        }
        Self::handle_error_escalation(peer, error_count);
    }

    /// Warns once at the warning threshold and disconnects at the hard limit. The counter must
    /// not be cleared on warning, or the limit can never be exceeded.
    fn handle_error_escalation(peer: &NetPeerRef, error_count: i32) {
        if error_count == Self::MAX_ERRORS_BEFORE_WARNING {
            BNL::log_error(format!(
                "Peer {} has reached {error_count} protocol errors. The server has detected an issue with this client or its connection.",
                peer.id()
            ));
            BasisPlayerModeration::send_back_message(peer, "The server has detected an issue with your client or connection. You may experience problems.");
        } else if error_count >= Self::MAX_ERRORS_BEFORE_DISCONNECT {
            BNL::log_error(format!("Peer {} exceeded {} protocol errors; disconnecting.", peer.id(), Self::MAX_ERRORS_BEFORE_DISCONNECT));
            PEER_ERROR_COUNTS.remove(&peer.id());
            peer.disconnect();
        }
    }

    /// Drops every peer's error count. Used when the server stops and by tests.
    pub fn reset() {
        PEER_ERROR_COUNTS.clear();
    }
}
