//! Port of `LiteNetLib/ConnectionRequest.cs`: a pending inbound connection, decided exactly
//! once by `accept` or `reject`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Weak};

use basis_error::{BasisError, BasisResult, ErrorCode};
use parking_lot::Mutex;

use crate::io::{NetDataReader, NetDataWriter};
use crate::transport::basis_network_shell::{ConnectionRequest, NetPeerRef};

use super::internal_packets::NetConnectRequestPacket;
use super::net_manager::ManagerInner;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ConnectionRequestResult {
    Accept,
    Reject,
    RejectForce,
}

const UNDECIDED: u8 = 0;
const ACCEPTED: u8 = 1;
const REJECTED: u8 = 2;

pub struct LnlConnectionRequest {
    manager: Weak<ManagerInner>,
    remote: SocketAddr,
    internal: Mutex<NetConnectRequestPacket>,
    decided: AtomicU8,
    accepted: Mutex<Option<NetPeerRef>>,
}

impl LnlConnectionRequest {
    pub(super) fn new(manager: &Arc<ManagerInner>, remote: SocketAddr, packet: NetConnectRequestPacket) -> Arc<Self> {
        Arc::new(Self {
            manager: Arc::downgrade(manager),
            remote,
            internal: Mutex::new(packet),
            decided: AtomicU8::new(UNDECIDED),
            accepted: Mutex::new(None),
        })
    }

    /// A repeat of the connect request (the client resends until answered): a newer one
    /// replaces what the handler will see; an older or identical one is ignored.
    pub(super) fn update_request(&self, connect_request: NetConnectRequestPacket) {
        let mut internal = self.internal.lock();
        // old request
        if connect_request.connection_time < internal.connection_time {
            return;
        }
        if connect_request.connection_time == internal.connection_time && connect_request.connection_number == internal.connection_number {
            return;
        }
        *internal = connect_request;
    }

    pub(super) fn internal_packet(&self) -> NetConnectRequestPacket {
        self.internal.lock().clone()
    }

    fn manager(&self) -> BasisResult<Arc<ManagerInner>> {
        self.manager.upgrade().ok_or_else(|| BasisError::permanent(ErrorCode::Conflict, "the LiteNetLib transport has been stopped"))
    }
}

impl ConnectionRequest for LnlConnectionRequest {
    fn data(&self) -> NetDataReader {
        NetDataReader::from_slice(&self.internal.lock().data)
    }

    fn remote_end_point(&self) -> SocketAddr {
        self.remote
    }

    fn accept(&self) -> BasisResult<NetPeerRef> {
        if let Err(current) = self.decided.compare_exchange(UNDECIDED, ACCEPTED, Ordering::SeqCst, Ordering::SeqCst) {
            return match (current, self.accepted.lock().clone()) {
                (ACCEPTED, Some(peer)) => Ok(peer),
                (ACCEPTED, None) => Err(BasisError::permanent(
                    ErrorCode::Conflict,
                    format!("connection request from {} is being accepted on another thread", self.remote),
                )),
                _ => Err(BasisError::permanent(ErrorCode::Conflict, format!("connection request from {} was already rejected", self.remote))),
            };
        }
        let manager = self.manager()?;
        let peer = manager.on_connection_solved(self, ConnectionRequestResult::Accept, &[]).ok_or_else(|| {
            BasisError::permanent(ErrorCode::Transport, format!("the connection from {} could not be admitted: the transport is stopping", self.remote))
        })?;
        let peer_ref: NetPeerRef = Arc::new(super::LnlNetPeer::new(peer));
        *self.accepted.lock() = Some(peer_ref.clone());
        Ok(peer_ref)
    }

    fn reject(&self, w: &NetDataWriter) -> BasisResult<()> {
        if let Err(current) = self.decided.compare_exchange(UNDECIDED, REJECTED, Ordering::SeqCst, Ordering::SeqCst) {
            return if current == REJECTED {
                Ok(())
            } else {
                Err(BasisError::permanent(ErrorCode::Conflict, format!("connection request from {} was already accepted", self.remote)))
            };
        }
        let manager = self.manager()?;
        manager.on_connection_solved(self, ConnectionRequestResult::Reject, w.as_read_only_span());
        Ok(())
    }
}
