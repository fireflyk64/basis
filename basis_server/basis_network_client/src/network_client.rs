//! Port of `NetworkClient.cs`: one client connection to a Basis server.

use std::sync::Arc;

use basis_error::{BasisError, BasisResult, ErrorCode, ResultExt};
use basis_network_core::SerializableBasis::{BytesMessage, ReadyMessage};
use basis_network_core::configuration::Configuration;
use basis_network_core::transport::basis_network_shell::NetManagerRef;
use basis_network_core::transport::basis_network_stack_registry::BasisNetworkStackRegistry;
use basis_network_core::{BNL, BasisNetworkVersion, EventBasedNetListener, NetDataWriter, NetPeerRef};
use parking_lot::Mutex;

/// The transport plus the peer that represents the server.
pub struct NetworkClient {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    client: Option<NetManagerRef>,
    listener: Option<Arc<EventBasedNetListener>>,
    peer: Option<NetPeerRef>,
    is_in_use: bool,
}

impl Default for NetworkClient {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkClient {
    pub fn new() -> Self {
        Self { inner: Mutex::new(Inner::default()) }
    }

    /// Starts the transport and connects to `target` (an `address[:port]` or an iroh endpoint id
    /// for the iroh stack), presenting the protocol version, the auth bytes and the ready
    /// message as the connect payload. A second call before `shutdown` is a
    /// [`Conflict`](ErrorCode::Conflict) error, where the C# logged "Call Shutdown First!".
    ///
    /// The iroh transport runs its own event loop, so the C# `manualMode` has no equivalent:
    /// events arrive on the listener from the transport runtime.
    pub fn start_client(
        &self,
        target: &str,
        port: u16,
        ready_message: &mut ReadyMessage,
        authentication_message: &[u8],
        configuration: &Configuration,
    ) -> BasisResult<NetPeerRef> {
        let mut inner = self.inner.lock();
        if inner.is_in_use {
            return Err(BasisError::permanent(ErrorCode::Conflict, "Call Shutdown First!"));
        }
        let listener = EventBasedNetListener::new();
        let client = BasisNetworkStackRegistry::create(&configuration.network_stack_id, listener.clone(), configuration).ok_or_else(|| {
            BasisError::permanent(ErrorCode::Transport, format!("network stack '{}' could not be created", configuration.network_stack_id))
        })?;
        client.start_default().context("starting the client transport")?;

        let mut writer = NetDataWriter::with_capacity(12);
        // This is the only time we don't put a key first: the version leads the connect payload.
        writer.put_ushort(BasisNetworkVersion::server_version());
        BytesMessage.serialize(&mut writer, authentication_message).context("writing the auth bytes")?;
        ready_message.serialize(&mut writer).context("writing the ready message")?;
        let peer = match client.connect(target, port, &writer) {
            Ok(peer) => peer,
            Err(e) => {
                client.stop();
                return Err(e).with_context(|| format!("connecting to {target}:{port}"));
            }
        };
        inner.client = Some(client);
        inner.listener = Some(listener);
        inner.peer = Some(peer.clone());
        inner.is_in_use = true;
        Ok(peer)
    }

    pub fn listener(&self) -> Option<Arc<EventBasedNetListener>> {
        self.inner.lock().listener.clone()
    }

    pub fn client(&self) -> Option<NetManagerRef> {
        self.inner.lock().client.clone()
    }

    pub fn peer(&self) -> Option<NetPeerRef> {
        self.inner.lock().peer.clone()
    }

    pub fn is_in_use(&self) -> bool {
        self.inner.lock().is_in_use
    }

    /// The C# manual-mode pump. The iroh transport delivers events itself; kept so callers
    /// written against the C# shape still compile.
    pub fn poll(&self) {}

    pub fn update(&self, _elapsed_milliseconds: f32) {}

    /// Tells the server we are leaving and closes the transport.
    pub fn disconnect(&self) {
        BNL::log("Client Called Disconnect from server");
        self.notify_server_of_departure();
        self.shutdown();
        BNL::log("Worker thread stopped.");
    }

    /// Tells the server this client is leaving, and does nothing else. Cheap: one datagram.
    pub fn notify_server_of_departure(&self) {
        let peer = {
            let mut inner = self.inner.lock();
            inner.is_in_use = false;
            inner.peer.clone()
        };
        if let Some(peer) = peer {
            peer.disconnect();
        }
    }

    /// Closes the socket and joins the transport's tasks.
    pub fn shutdown(&self) {
        let client = {
            let mut inner = self.inner.lock();
            inner.is_in_use = false;
            inner.client.take()
        };
        if let Some(client) = client {
            client.stop();
        }
        let mut inner = self.inner.lock();
        inner.peer = None;
        inner.listener = None;
    }
}
