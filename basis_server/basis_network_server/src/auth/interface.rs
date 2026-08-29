//! Port of `Auth/Interface.cs`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use basis_network_core::NetDataReader;
use basis_network_core::configuration::Configuration;
use basis_network_core::{ConnectionRequest, NetPeerRef};

/// Class used to see if we can authenticate (password correct).
pub trait IAuth: Send + Sync {
    fn is_authenticated(&self, bytes_msg: &[u8]) -> bool;
}

/// The class we use to get the user's identity; the UUID of a player will become this.
pub trait IAuthIdentity: Send + Sync {
    /// `data` is the connect payload positioned just past the auth bytes (the C# shared one
    /// reader across the whole handshake).
    fn process_connection(&self, configuration: &Configuration, connection_request: &Arc<dyn ConnectionRequest>, data: NetDataReader, net_peer: &NetPeerRef);
    fn de_initialize(&self);
    fn remove_connection(&self, net_peer: i32);
    /// Removes the entry for `net_peer` only when it still belongs to `expected`.
    fn remove_connection_expected(&self, net_peer: i32, expected: &NetPeerRef) -> bool;
    /// The C# `NetIDToUUID(peer, out uuid)`.
    fn net_id_to_uuid(&self, peer: &NetPeerRef) -> Option<String>;
    /// The C# `UUIDToNetID(uuid, out peer)`.
    fn uuid_to_net_id(&self, uuid: &str) -> Option<i32>;
}

static HAS_FILE_SUPPORT: AtomicBool = AtomicBool::new(false);

/// The C# `IAuthIdentity.HasFileSupport` static.
pub struct IAuthIdentitySupport;

impl IAuthIdentitySupport {
    pub fn has_file_support() -> bool {
        HAS_FILE_SUPPORT.load(Ordering::Acquire)
    }

    pub fn set_has_file_support(value: bool) {
        HAS_FILE_SUPPORT.store(value, Ordering::Release);
    }
}
