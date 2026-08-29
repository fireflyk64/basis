//! Port of `Networking/BasisNetworkingGeneric.cs`: the scene and avatar relay.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Instant;

use basis_network_core::SerializableBasis::{
    AvatarDataMessage, PlayerIdMessage, RemoteAvatarDataMessage, RemoteSceneDataMessage, SceneDataMessage, ServerAvatarDataMessage,
    ServerSceneDataMessage,
};
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetDataWriter, NetPacketReader, NetPeerRef};
use dashmap::DashMap;
use parking_lot::Mutex;

use crate::NetworkServer;
use crate::networking::{BasisImageBandwidthGovernor, BasisNetworkImageCache};

struct SceneEgressBucket {
    tokens: f64,
    last: Instant,
}

static MISSING_PEER_COUNT: AtomicI64 = AtomicI64::new(0);
static MISSING_PEER_NEXT_REPORT: Mutex<Option<Instant>> = Mutex::new(None);

// ── Opt-in non-image scene-egress backstop ─────────────────────────────────────────────────
// Per-sender token bucket on the bytes this relay fans out, charged the same way the image
// governor charges (payload × recipients). Disabled unless an operator sets
// MaxSceneRelayMegabitsPerSecondPerPlayer, so the default hot path is a single config read.
static SCENE_EGRESS: LazyLock<DashMap<u16, Mutex<SceneEgressBucket>>> = LazyLock::new(DashMap::new);

thread_local! {
    static TARGETED_CLIENTS: RefCell<Vec<NetPeerRef>> = const { RefCell::new(Vec::new()) };
    static SEEN_RECIPIENTS: RefCell<HashSet<u16>> = RefCell::new(HashSet::new());
}

pub struct BasisNetworkingGeneric;

impl BasisNetworkingGeneric {
    const MISSING_PEER_REPORT_INTERVAL_SECONDS: u64 = 10;
    const SCENE_MEGABITS_TO_BYTES: f64 = 125_000.0;
    const SCENE_BURST_SECONDS: f64 = 2.0;

    fn report_missing_peer() {
        MISSING_PEER_COUNT.fetch_add(1, Ordering::Relaxed);
        let now = Instant::now();
        let mut next = MISSING_PEER_NEXT_REPORT.lock();
        if next.is_some_and(|n| now < n) {
            return;
        }
        *next = Some(now + std::time::Duration::from_secs(Self::MISSING_PEER_REPORT_INTERVAL_SECONDS));
        drop(next);
        let dropped = MISSING_PEER_COUNT.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            BNL::log(format!(
                "Missing Peer! dropped {dropped} targeted message(s) for peers that were not authenticated in the last {}s.",
                Self::MISSING_PEER_REPORT_INTERVAL_SECONDS
            ));
        }
    }

    fn scene_egress_allowed(sender_id: u16, bytes: i64) -> bool {
        let megabits = NetworkServer::configuration().map(|c| c.max_scene_relay_megabits_per_second_per_player).unwrap_or(0);
        if megabits <= 0 || bytes <= 0 {
            return true; // disabled, or nothing to charge
        }
        let rate_per_second = f64::from(megabits) * Self::SCENE_MEGABITS_TO_BYTES;
        let bucket = SCENE_EGRESS
            .entry(sender_id)
            .or_insert_with(|| Mutex::new(SceneEgressBucket { tokens: rate_per_second * Self::SCENE_BURST_SECONDS, last: Instant::now() }));
        let mut bucket = bucket.lock();
        let now = Instant::now();
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        if elapsed > 0.0 {
            bucket.last = now;
            let ceiling = rate_per_second * Self::SCENE_BURST_SECONDS;
            bucket.tokens = ceiling.min(bucket.tokens + rate_per_second * elapsed);
        }
        // Gate on having credit rather than on the whole charge fitting, and let the bucket go
        // negative: a single wide fan-out can exceed the burst, and demanding it fit would stall
        // that sender forever. The long-run average is still exactly the budget.
        if bucket.tokens <= 0.0 {
            return false;
        }
        bucket.tokens -= bytes as f64;
        true
    }

    /// Drops a departed peer's scene-egress bucket so a recycled id starts clean.
    pub fn remove_peer_scene_egress(peer_id: i32) {
        if let Ok(id) = u16::try_from(peer_id) {
            SCENE_EGRESS.remove(&id);
        }
    }

    /// Resolves the recipient list into peers (deduplicated) and broadcasts to them, or to the
    /// whole snapshot minus the sender when the list is empty.
    fn relay(writer: &NetDataWriter, channel: u8, sender: &NetPeerRef, recipients: Option<&[u16]>, recipients_size: usize, delivery: DeliveryMethod) {
        if recipients_size != 0
            && let Some(recipients) = recipients
        {
            TARGETED_CLIENTS.with(|targeted| {
                SEEN_RECIPIENTS.with(|seen| {
                    let mut targeted = targeted.borrow_mut();
                    let mut seen = seen.borrow_mut();
                    targeted.clear();
                    seen.clear();
                    for recipient in recipients.iter().take(recipients_size) {
                        if !seen.insert(*recipient) {
                            continue;
                        }
                        match NetworkServer::authenticated_peers().get(&i32::from(*recipient)) {
                            Some(client) => targeted.push(client.value().clone()),
                            None => Self::report_missing_peer(),
                        }
                    }
                    if !targeted.is_empty() {
                        NetworkServer::broadcast_message_to_clients(writer, channel, &targeted, delivery);
                    }
                    targeted.clear();
                });
            });
        } else {
            NetworkServer::broadcast_message_to_clients_excluding(writer, channel, sender, &NetworkServer::peer_snapshot(), delivery);
        }
    }

    pub fn handle_scene(mut reader: NetPacketReader, delivery_method: DeliveryMethod, sender: &NetPeerRef, broadcast_channel: u8) {
        let mut scene_data_message = SceneDataMessage::default();
        if scene_data_message.deserialize(&mut reader).is_err() {
            return;
        }
        let payload = scene_data_message.payload.take().unwrap_or_default();
        let payload_length = payload.len();
        let recipients_size = usize::from(scene_data_message.recipients_size);

        // Observe only — the relay below is untouched, so a cache miss, a rejection or a
        // malformed payload can never interfere with the live send.
        let is_image_traffic = BasisNetworkImageCache::is_image_traffic(scene_data_message.message_index);
        if is_image_traffic {
            BasisNetworkImageCache::observe(sender.id() as u16, &payload, scene_data_message.recipients.as_deref(), recipients_size);
        }

        // Server-side floor under the client's own pacing. The sharer decides how to spend its
        // budget — only it knows how the fan-out splits between relayed and direct peers — but a
        // client that ignores the budget entirely must not be able to spend the server's egress
        // on our behalf. Charged on fan-out, because that is what the relay actually costs.
        //
        // The untargeted branch broadcasts to the snapshot minus the sender, so the fan-out is
        // one less than its length.
        let fan_out = if recipients_size != 0 { recipients_size } else { NetworkServer::peer_snapshot().len().saturating_sub(1) };
        let egress_bytes = (payload_length as i64) * (fan_out.max(1) as i64);

        if is_image_traffic {
            // Image traffic has its own governor (advertised budget + per-owner buckets).
            if !BasisImageBandwidthGovernor::try_consume_egress(sender.id() as u16, egress_bytes) {
                return;
            }
        } else if !Self::scene_egress_allowed(sender.id() as u16, egress_bytes) {
            // Everything else on this channel is interactive scene state measured in tens of
            // bytes, so this backstop is OFF by default (MaxSceneRelayMegabitsPerSecondPerPlayer
            // == 0). An operator can set a per-player ceiling to cap a modified client that
            // broadcasts arbitrary scene payloads to the whole room.
            return;
        }

        let mut server_scene_data_message = ServerSceneDataMessage {
            scene_data_message: RemoteSceneDataMessage { message_index: scene_data_message.message_index, payload: Some(payload), payload_length },
            player_id_message: PlayerIdMessage::new(sender.id() as u16),
        };

        let mut writer = NetworkServer::rent_writer();
        if server_scene_data_message.serialize(&mut writer).is_ok() {
            Self::relay(&writer, broadcast_channel, sender, scene_data_message.recipients.as_deref(), recipients_size, delivery_method);
        }
        NetworkServer::return_writer(writer);
        server_scene_data_message.scene_data_message.release();
    }

    pub fn handle_scene_default(reader: NetPacketReader, delivery_method: DeliveryMethod, sender: &NetPeerRef) {
        Self::handle_scene(reader, delivery_method, sender, BasisNetworkCommons::SCENE_CHANNEL);
    }

    pub fn handle_avatar(mut reader: NetPacketReader, delivery_method: DeliveryMethod, sender: &NetPeerRef, broadcast_channel: u8) {
        let mut avatar_data_message = AvatarDataMessage::default();
        if avatar_data_message.deserialize(&mut reader).is_err() {
            return;
        }
        let recipients_size = usize::from(avatar_data_message.recipients_size);
        let mut server_avatar_data_message = ServerAvatarDataMessage {
            avatar_data_message: RemoteAvatarDataMessage {
                message_index: avatar_data_message.message_index,
                payload: avatar_data_message.payload.take(),
                player_id_message: avatar_data_message.player_id_message,
                avatar_link_index: avatar_data_message.avatar_link_index,
            },
            player_id_message: PlayerIdMessage::new(sender.id() as u16),
        };
        let mut writer = NetworkServer::rent_writer();
        if server_avatar_data_message.serialize(&mut writer).is_ok() {
            Self::relay(&writer, broadcast_channel, sender, avatar_data_message.recipients.as_deref(), recipients_size, delivery_method);
        }
        NetworkServer::return_writer(writer);
    }

    pub fn handle_avatar_default(reader: NetPacketReader, delivery_method: DeliveryMethod, sender: &NetPeerRef) {
        Self::handle_avatar(reader, delivery_method, sender, BasisNetworkCommons::AVATAR_CHANNEL);
    }

    /// Drops the per-sender scene-egress buckets. Used when the server stops and by tests.
    pub fn reset() {
        SCENE_EGRESS.clear();
        MISSING_PEER_COUNT.store(0, Ordering::Relaxed);
        *MISSING_PEER_NEXT_REPORT.lock() = None;
    }
}
