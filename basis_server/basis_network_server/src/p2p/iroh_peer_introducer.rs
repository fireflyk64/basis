//! Port of `P2P/LNLPeerIntroducer.cs` for the iroh stack. LiteNetLib punched NAT holes with a
//! module of its own; iroh endpoints hole-punch themselves once each side knows the other's
//! `EndpointAddr`, so introducing a pair is a matter of handing each peer the other's address.

use basis_network_core::p2p::{IPeerIntroducer, PeerIntroduction};
use basis_network_core::transport::basis_network_shell::NetManagerRef;

use crate::p2p::BasisServerP2PBroker;

pub struct IrohPeerIntroducer;

impl IPeerIntroducer for IrohPeerIntroducer {
    fn initialize(&self, _active_manager: &NetManagerRef) -> bool {
        BasisServerP2PBroker::initialize();
        true
    }

    fn introduce(&self, a: &PeerIntroduction, b: &PeerIntroduction, token: &str) {
        BasisServerP2PBroker::introduce(a, b, token);
    }

    fn is_pair_offloaded(&self, peer_id_a: i32, peer_id_b: i32) -> bool {
        BasisServerP2PBroker::is_p2p_offloaded(peer_id_a, peer_id_b)
    }

    fn shutdown(&self) {}
}
