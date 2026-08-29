//! A `NetPeer` that records what was sent to it. The C# suites drove the server through
//! interface fakes; this is the Rust equivalent.

use std::any::Any;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use basis_network_core::transport::basis_network_shell::{DeliveryMethod, NetPeer, NetPeerRef, SendError};
use parking_lot::{Mutex, RwLock};

/// One recorded send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentPacket {
    pub channel: u8,
    pub delivery: DeliveryMethod,
    pub data: Vec<u8>,
}

pub struct FakePeer {
    id: i32,
    remote_id: AtomicI32,
    address: IpAddr,
    identity: u64,
    connected: AtomicBool,
    tag: RwLock<Option<Arc<dyn Any + Send + Sync>>>,
    pub sent: Mutex<Vec<SentPacket>>,
    pub disconnects: AtomicI32,
    /// When set, every send is refused with this error — the transport saying no.
    pub refuse_sends: Mutex<Option<SendError>>,
}

impl FakePeer {
    pub fn new(id: i32) -> Arc<Self> {
        Arc::new(Self {
            id,
            remote_id: AtomicI32::new(id),
            address: IpAddr::from([127, 0, 0, 1]),
            identity: 0x7000_0000_0000_0000 | id as u64,
            connected: AtomicBool::new(true),
            tag: RwLock::new(None),
            sent: Mutex::new(Vec::new()),
            disconnects: AtomicI32::new(0),
            refuse_sends: Mutex::new(None),
        })
    }

    pub fn with_address(id: i32, address: IpAddr) -> Arc<Self> {
        let peer = Self::new(id);
        // SAFETY-free: build a new value with the address set.
        Arc::new(Self { address, ..Arc::try_unwrap(peer).unwrap_or_else(|_| unreachable!()) })
    }

    pub fn as_ref(self: &Arc<Self>) -> NetPeerRef {
        self.clone()
    }

    pub fn sent_on(&self, channel: u8) -> Vec<SentPacket> {
        self.sent.lock().iter().filter(|p| p.channel == channel).cloned().collect()
    }

    pub fn sent_count(&self) -> usize {
        self.sent.lock().len()
    }

    pub fn clear_sent(&self) {
        self.sent.lock().clear();
    }

    pub fn set_remote_id(&self, id: i32) {
        self.remote_id.store(id, Ordering::Relaxed);
    }

    pub fn set_connected(&self, connected: bool) {
        self.connected.store(connected, Ordering::Relaxed);
    }
}

impl NetPeer for FakePeer {
    fn disconnect(&self) {
        self.disconnects.fetch_add(1, Ordering::Relaxed);
        self.connected.store(false, Ordering::Relaxed);
    }

    fn disconnect_with(&self, _data: &[u8]) {
        self.disconnect();
    }

    fn disconnect_force(&self) {
        self.disconnect();
    }

    fn send(&self, data: &[u8], channel_number: u8, delivery_method: DeliveryMethod) -> Result<(), SendError> {
        if let Some(err) = self.refuse_sends.lock().clone() {
            return Err(err);
        }
        self.sent.lock().push(SentPacket { channel: channel_number, delivery: delivery_method, data: data.to_vec() });
        Ok(())
    }

    fn send_unreliable_raw_merge(&self, data: &[u8], offset: usize, length: usize, channel_number: u8, patch_offset: i32, patch_value: u8) -> Result<(), SendError> {
        let end = offset.saturating_add(length);
        if end > data.len() {
            return Err(SendError::BadRange { offset, length, len: data.len() });
        }
        let mut copy = data[offset..end].to_vec();
        if patch_offset >= 0 && (patch_offset as usize) < copy.len() {
            copy[patch_offset as usize] = patch_value;
        }
        self.send(&copy, channel_number, DeliveryMethod::Unreliable)
    }

    fn get_packets_count_in_queue(&self, channel: u8, _delivery_method: DeliveryMethod) -> i32 {
        self.sent.lock().iter().filter(|p| p.channel == channel).count() as i32
    }

    fn id(&self) -> i32 {
        self.id
    }

    fn address(&self) -> IpAddr {
        self.address
    }

    fn remote_id(&self) -> i32 {
        self.remote_id.load(Ordering::Relaxed)
    }

    fn round_trip_time(&self) -> i32 {
        20
    }

    fn time_since_last_packet(&self) -> f32 {
        0.0
    }

    fn remote_time_delta(&self) -> i64 {
        0
    }

    fn mtu(&self) -> i32 {
        1200
    }

    fn tag(&self) -> Option<Arc<dyn Any + Send + Sync>> {
        self.tag.read().clone()
    }

    fn set_tag(&self, tag: Option<Arc<dyn Any + Send + Sync>>) {
        *self.tag.write() = tag;
    }

    fn identity(&self) -> u64 {
        self.identity
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
