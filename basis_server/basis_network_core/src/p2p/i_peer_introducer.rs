use std::net::SocketAddr;

use crate::transport::basis_network_shell::NetManagerRef;

/// What one side of a pair told the server about itself: the internal and external UDP
/// endpoints LiteNetLib's NAT punch worked from, plus (for the iroh transport) the serialized
/// `EndpointAddr` the other side should dial. The two views coexist so an introducer written
/// for either transport reads the same struct.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerIntroduction {
    pub internal: Option<SocketAddr>,
    pub external: Option<SocketAddr>,
    /// Postcard/JSON-serialized iroh `EndpointAddr` (see transport::iroh_network_impl).
    pub iroh_addr: Vec<u8>,
}

pub trait IPeerIntroducer: Send + Sync {
    fn initialize(&self, active_manager: &NetManagerRef) -> bool;
    fn introduce(&self, a: &PeerIntroduction, b: &PeerIntroduction, token: &str);
    fn is_pair_offloaded(&self, peer_id_a: i32, peer_id_b: i32) -> bool;
    fn shutdown(&self);
}
