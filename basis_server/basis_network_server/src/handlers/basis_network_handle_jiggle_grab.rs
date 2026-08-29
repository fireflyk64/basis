//! Port of `Handlers/BasisNetworkHandleJiggleGrab.cs`: relays jiggle grab lifecycle events.
//!
//! The server never inspects avatar data: it rewrites the payload with the authenticated sender
//! id, rate limits per peer, and fans out. Start (also the periodic keepalive) is
//! relevance-filtered against the reduction system's player positions so a thousand concurrent
//! grabs don't broadcast instance-wide; Stop and Deny are rare and broadcast so state can never
//! leak.

use std::sync::LazyLock;
use std::time::Instant;

use basis_network_core::mathematics::Vector3;
use basis_network_core::{BasisNetworkCommons, DeliveryMethod, NetDataWriter, NetPacketReader, NetPeerRef, NetResult};
use dashmap::DashMap;
use parking_lot::Mutex;

use crate::NetworkServer;
use crate::reduction::BasisServerReductionSystemEvents;

struct TokenBucket {
    tokens: f32,
    last_refill: Instant,
}

static BUCKETS: LazyLock<DashMap<i32, Mutex<TokenBucket>>> = LazyLock::new(DashMap::new);

pub struct BasisNetworkHandleJiggleGrab;

impl BasisNetworkHandleJiggleGrab {
    pub const RELEVANCE_DISTANCE: f32 = 64.0;
    const RELEVANCE_DISTANCE_SQUARED: f32 = Self::RELEVANCE_DISTANCE * Self::RELEVANCE_DISTANCE;
    pub const TOKENS_PER_SECOND: f32 = 8.0;
    pub const TOKEN_BURST: f32 = 16.0;
    const MAX_TRACKED_PEERS: usize = 4096;

    fn try_consume_token(peer: &NetPeerRef) -> bool {
        if BUCKETS.len() > Self::MAX_TRACKED_PEERS {
            BUCKETS.clear();
        }
        let bucket = BUCKETS.entry(peer.id()).or_insert_with(|| Mutex::new(TokenBucket { tokens: Self::TOKEN_BURST, last_refill: Instant::now() }));
        let mut bucket = bucket.lock();
        let now = Instant::now();
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f32();
        bucket.last_refill = now;
        bucket.tokens = Self::TOKEN_BURST.min(bucket.tokens + elapsed * Self::TOKENS_PER_SECOND);
        if bucket.tokens < 1.0 {
            return false;
        }
        bucket.tokens -= 1.0;
        true
    }

    pub fn handle_event(mut reader: NetPacketReader, peer: &NetPeerRef, event_type: u8) {
        let _ = Self::try_handle(&mut reader, peer, event_type);
    }

    fn try_handle(reader: &mut NetPacketReader, peer: &NetPeerRef, event_type: u8) -> NetResult<()> {
        let op = reader.get_byte()?;
        match op {
            BasisNetworkCommons::JIGGLE_GRAB_OP_START => {
                let target_id = reader.get_ushort()?;
                let rig_index = reader.get_byte()?;
                let point_index = reader.get_ushort()?;
                let hand = reader.get_byte()?;
                let bone_name_hash = reader.get_uint()?;
                let offset_x = reader.get_ushort()?;
                let offset_y = reader.get_ushort()?;
                let offset_z = reader.get_ushort()?;
                if !Self::try_consume_token(peer) {
                    return Ok(());
                }
                let mut writer = NetworkServer::rent_writer();
                writer.put_byte(event_type);
                writer.put_byte(op);
                writer.put_ushort(peer.id() as u16);
                writer.put_ushort(target_id);
                writer.put_byte(rig_index);
                writer.put_ushort(point_index);
                writer.put_byte(hand);
                writer.put_uint(bone_name_hash);
                writer.put_ushort(offset_x);
                writer.put_ushort(offset_y);
                writer.put_ushort(offset_z);
                Self::send_start_filtered(&writer, peer, target_id);
                NetworkServer::return_writer(writer);
            }
            BasisNetworkCommons::JIGGLE_GRAB_OP_STOP => {
                let target_id = reader.get_ushort()?;
                let rig_index = reader.get_byte()?;
                let point_index = reader.get_ushort()?;
                if !Self::try_consume_token(peer) {
                    return Ok(());
                }
                let mut writer = NetworkServer::rent_writer();
                writer.put_byte(event_type);
                writer.put_byte(op);
                writer.put_ushort(peer.id() as u16);
                writer.put_ushort(target_id);
                writer.put_byte(rig_index);
                writer.put_ushort(point_index);
                NetworkServer::broadcast_message_to_clients_excluding(
                    &writer,
                    BasisNetworkCommons::EVENTS_CHANNEL,
                    peer,
                    &NetworkServer::peer_snapshot(),
                    DeliveryMethod::ReliableOrdered,
                );
                NetworkServer::return_writer(writer);
            }
            BasisNetworkCommons::JIGGLE_GRAB_OP_DENY => {
                let grabber_id = reader.get_ushort()?;
                if !Self::try_consume_token(peer) {
                    return Ok(());
                }
                let mut writer = NetworkServer::rent_writer();
                writer.put_byte(event_type);
                writer.put_byte(op);
                writer.put_ushort(peer.id() as u16);
                writer.put_ushort(grabber_id);
                NetworkServer::broadcast_message_to_clients_excluding(
                    &writer,
                    BasisNetworkCommons::EVENTS_CHANNEL,
                    peer,
                    &NetworkServer::peer_snapshot(),
                    DeliveryMethod::ReliableOrdered,
                );
                NetworkServer::return_writer(writer);
            }
            _ => {}
        }
        Ok(())
    }

    /// Sends a Start to every peer near the grab target (the reduction system's live positions),
    /// always including the target itself. Missing position state fails open to a full broadcast
    /// — correctness beats bandwidth here.
    fn send_start_filtered(writer: &NetDataWriter, sender: &NetPeerRef, target_id: u16) {
        let peers = NetworkServer::peer_snapshot();
        let Some(target_position) = BasisServerReductionSystemEvents::try_get_active_position(i32::from(target_id)) else {
            NetworkServer::broadcast_message_to_clients_excluding(writer, BasisNetworkCommons::EVENTS_CHANNEL, sender, &peers, DeliveryMethod::ReliableOrdered);
            return;
        };
        for recipient in peers.iter() {
            if recipient.id() == sender.id() {
                continue;
            }
            if recipient.id() as u16 != target_id
                && let Some(recipient_position) = BasisServerReductionSystemEvents::try_get_active_position(recipient.id())
                && Self::distance_squared(recipient_position, target_position) > Self::RELEVANCE_DISTANCE_SQUARED
            {
                continue;
            }
            NetworkServer::try_send(recipient, writer, BasisNetworkCommons::EVENTS_CHANNEL, DeliveryMethod::ReliableOrdered);
        }
    }

    fn distance_squared(a: Vector3, b: Vector3) -> f32 {
        let dx = a.x - b.x;
        let dy = a.y - b.y;
        let dz = a.z - b.z;
        dx * dx + dy * dy + dz * dz
    }

    pub fn reset() {
        BUCKETS.clear();
    }
}
