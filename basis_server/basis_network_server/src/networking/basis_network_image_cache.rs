//! Port of `Networking/BasisNetworkImageCache.cs`.
//!
//! Keeps the bytes of every shared image in server RAM so the rest of the room can be given a
//! picture the sharer only sent to the players standing near it, instead of the sharer having to
//! send it again to every arrival.
//!
//! The cache is deliberately dumb about what an image *is*: it retains the client's own scene
//! payloads verbatim and replays them stamped with the original owner's player id. A receiving
//! client therefore sees exactly the OpSpawn/OpChunk stream it would have got from the owner.
//!
//! What it hands a player first is an offer — the sharer's own spawn header, opcode swapped —
//! and nothing moves until that client measures the distance for itself and asks. The one thing
//! the cache reads out of a payload is where a picture has got to: the pose the room last saw is
//! kept and written over the header's before an offer or a replay goes out.
//!
//! Lifetime matches what clients already do with images: an entry is dropped on despawn and when
//! its owner disconnects.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};

use basis_network_core::SerializableBasis::{PlayerIdMessage, RemoteSceneDataMessage, ServerSceneDataMessage};
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetDataWriter, NetPeerRef};
use parking_lot::Mutex;

use crate::NetworkServer;
use crate::identity::BasisNetworkIDDatabase;
use crate::networking::{BasisImageBandwidthGovernor, PendingPayload};

/// The 16 raw GUID bytes as they sit on the wire. Kept raw rather than parsed: the id only ever
/// round-trips back into a payload, so endianness never matters.
pub type ImageId = [u8; 16];

type Bytes = Arc<[u8]>;

/// A cached image. Stills arrive as OpSpawn + OpChunk; an animated image adds OpAnimationSpawn +
/// OpAnimationChunk on top of the still that carries its poster frame.
struct CachedImage {
    owner_id: u16,
    sequence: i64,
    /// Who has been told this image exists. One offer per player, ever.
    offered: HashSet<u16>,
    /// Who has actually been sent the bytes. Separate from `offered` because a player may sit on
    /// an offer for as long as they like before walking close enough to want it, and because a
    /// repeated request must not buy a second copy.
    delivered: HashSet<u16>,
    spawn: Option<Bytes>,
    chunks: Vec<Option<Bytes>>,
    chunks_held: usize,
    /// Where the pose sits inside `spawn`. Walked once when the header is admitted.
    pose_offset: usize,
    /// The last pose the room was told about, or None while the picture has not moved since it
    /// was shared. Retained verbatim like every other payload; it carries scale, which a spawn
    /// header does not.
    transform: Option<Bytes>,
    animation_spawn: Option<Bytes>,
    animation_chunks: Vec<Option<Bytes>>,
    animation_chunks_held: usize,
    bytes: i64,
}

impl CachedImage {
    /// A still is servable once its spawn header and every chunk has landed.
    fn still_complete(&self) -> bool {
        self.spawn.is_some() && !self.chunks.is_empty() && self.chunks_held == self.chunks.len()
    }

    /// An animation is only replayed alongside a complete still. A half-received animation is
    /// simply not sent — the joiner still gets the still image rather than nothing.
    fn animation_complete(&self) -> bool {
        self.animation_spawn.is_some() && !self.animation_chunks.is_empty() && self.animation_chunks_held == self.animation_chunks.len()
    }
}

struct CacheState {
    images: HashMap<ImageId, CachedImage>,
    sequence: i64,
}

static STATE: LazyLock<Mutex<CacheState>> = LazyLock::new(|| Mutex::new(CacheState { images: HashMap::new(), sequence: 0 }));
static TOTAL_BYTES: AtomicI64 = AtomicI64::new(0);
/// Resolved NetworkID of the image manager, or -1 until a client has asked for one. Ids are
/// stable for the life of the database, so this is memoised rather than looked up by string on
/// every scene message.
static MANAGER_NET_ID: AtomicI32 = AtomicI32::new(-1);

pub struct BasisNetworkImageCache;

impl BasisNetworkImageCache {
    /// The image manager registers under this fixed string, so the server can resolve which
    /// dynamic NetworkID carries image traffic. Must match the client's
    /// `BasisImagePickupManager.FixedNetworkIdentifier`.
    pub const IMAGE_MANAGER_IDENTIFIER: &'static str = "BasisImagePickupManager";

    // Mirrored from BasisImagePickupManager. Wire protocol — changing either side is a break.
    const OP_SPAWN: u8 = 1;
    const OP_CHUNK: u8 = 2;
    const OP_TRANSFORM: u8 = 3;
    const OP_DESPAWN: u8 = 4;
    const OP_ANIMATION_SPAWN: u8 = 6;
    const OP_ANIMATION_CHUNK: u8 = 7;
    /// Server → owner: "I am holding this image" / "I am no longer holding it".
    const OP_SERVER_CACHE_STATE: u8 = 8;
    /// Server → client: "I am holding an image; here is its header, decide for yourself whether
    /// you want it."
    const OP_SERVER_CACHE_OFFER: u8 = 9;
    /// Client → server: "send me that one."
    const OP_SERVER_CACHE_REQUEST: u8 = 10;

    const OPCODE_BYTES: usize = 1;
    const GUID_BYTES: usize = 16;
    const HEADER_BYTES: usize = Self::OPCODE_BYTES + Self::GUID_BYTES;
    /// Position and rotation, seven floats, written last in a spawn header.
    const POSE_BYTES: usize = (3 + 4) * 4;
    /// opcode + guid + pose + scale. Fixed, so anything of another length is not one.
    const TRANSFORM_BYTES: usize = Self::HEADER_BYTES + Self::POSE_BYTES + 4;
    /// Ceiling on a single owner name, matching the client's own read guard.
    const MAX_OWNER_NAME_BYTES: usize = 1024;
    const BYTES_PER_MEGABYTE: i64 = 1024 * 1024;
    /// What one chunk slot costs before its bytes arrive; the C# charged one reference.
    const SLOT_BYTES: i64 = std::mem::size_of::<Option<Bytes>>() as i64;

    /// Bytes currently held. Diagnostics and tests.
    pub fn total_bytes() -> i64 {
        TOTAL_BYTES.load(Ordering::Relaxed)
    }

    /// Number of complete or in-flight images held.
    pub fn count() -> usize {
        STATE.lock().images.len()
    }

    /// How many held images are complete enough to hand to a joiner.
    pub fn servable_count() -> usize {
        STATE.lock().images.values().filter(|e| e.still_complete()).count()
    }

    /// Bytes currently held on behalf of one player.
    pub fn bytes_held_for(owner_id: u16) -> i64 {
        Self::owner_bytes(&STATE.lock(), owner_id)
    }

    fn enabled() -> bool {
        NetworkServer::configuration().is_some_and(|c| c.image_cache_enabled)
    }

    fn max_bytes() -> i64 {
        let configured = NetworkServer::configuration().map(|c| c.image_cache_max_megabytes).unwrap_or(0);
        if configured > 0 { i64::from(configured) * Self::BYTES_PER_MEGABYTE } else { 0 }
    }

    /// Every owner is guaranteed this much before fair-share division applies, so a busy
    /// instance cannot shrink each person's allowance to something that fits no image at all.
    fn minimum_owner_bytes() -> i64 {
        let configured = NetworkServer::configuration().map(|c| c.image_cache_minimum_per_owner_megabytes).unwrap_or(0);
        if configured > 0 { i64::from(configured) * Self::BYTES_PER_MEGABYTE } else { 0 }
    }

    pub fn reset() {
        let mut state = STATE.lock();
        state.images.clear();
        state.sequence = 0;
        TOTAL_BYTES.store(0, Ordering::Relaxed);
        MANAGER_NET_ID.store(-1, Ordering::Relaxed);
    }

    /// True when this scene message is image traffic. Called for every scene message, so it is a
    /// memoised integer compare after the first resolve.
    pub fn is_image_traffic(message_index: u16) -> bool {
        let known = MANAGER_NET_ID.load(Ordering::Relaxed);
        if known >= 0 {
            return i32::from(message_index) == known;
        }
        if let Some(resolved) = BasisNetworkIDDatabase::ushort_network_database().get(Self::IMAGE_MANAGER_IDENTIFIER).map(|v| *v) {
            MANAGER_NET_ID.store(i32::from(resolved), Ordering::Relaxed);
            return message_index == resolved;
        }
        false
    }

    fn manager_net_id() -> Option<u16> {
        u16::try_from(MANAGER_NET_ID.load(Ordering::Relaxed)).ok()
    }

    /// Feeds one relayed image payload to the cache. The caller keeps relaying exactly as before
    /// — this only observes, so a cache that rejects or misparses a message can never stop the
    /// live send. `payload` is copied where retained.
    pub fn observe(sender_id: u16, payload: &[u8], recipients: Option<&[u16]>, recipients_size: usize) {
        if !Self::enabled() || payload.len() < Self::HEADER_BYTES {
            return;
        }
        match payload[0] {
            Self::OP_SPAWN => Self::observe_spawn(sender_id, payload, recipients, recipients_size),
            Self::OP_CHUNK => Self::observe_chunk(sender_id, payload, false),
            Self::OP_TRANSFORM => Self::observe_transform(payload),
            Self::OP_SERVER_CACHE_REQUEST => Self::observe_request(sender_id, payload),
            Self::OP_ANIMATION_SPAWN => Self::observe_animation_spawn(sender_id, payload),
            Self::OP_ANIMATION_CHUNK => Self::observe_chunk(sender_id, payload, true),
            Self::OP_DESPAWN => {
                Self::remove(Self::read_guid(payload), sender_id, true);
            }
            _ => {}
        }
    }

    fn observe_spawn(sender_id: u16, payload: &[u8], recipients: Option<&[u16]>, recipients_size: usize) {
        let id = Self::read_guid(payload);
        let Some((total_chunks, pose_offset)) = Self::try_read_spawn_header(payload) else {
            return;
        };
        if total_chunks == 0 || pose_offset + Self::POSE_BYTES > payload.len() {
            return;
        }

        let mut state = STATE.lock();
        if state.images.contains_key(&id) {
            // Already tracked. A join re-send repeats the spawn header; keep the first copy.
            return;
        }
        // Charge the chunk-array backbone, not just the header. total_chunks is client-supplied
        // and the slot vector costs total_chunks slots; accounting only the payload let a client
        // register an enormous total_chunks (an unbounded allocation) while the cap saw a few
        // bytes. With the backbone charged, an implausible count trips `cost > cap` inside
        // try_reserve and is refused before anything is allocated.
        let cost = payload.len() as i64 + (total_chunks as i64).saturating_mul(Self::SLOT_BYTES);
        if !Self::try_reserve(&mut state, sender_id, cost, Some(&id)) {
            return;
        }
        state.sequence += 1;
        let mut entry = CachedImage {
            owner_id: sender_id,
            sequence: state.sequence,
            offered: HashSet::new(),
            delivered: HashSet::new(),
            spawn: Some(Bytes::from(payload)),
            chunks: vec![None; total_chunks],
            chunks_held: 0,
            pose_offset,
            transform: None,
            animation_spawn: None,
            animation_chunks: Vec::new(),
            animation_chunks_held: 0,
            bytes: cost,
        };
        Self::seed_already_held(&mut entry, recipients, recipients_size);
        state.images.insert(id, entry);
        TOTAL_BYTES.fetch_add(cost, Ordering::Relaxed);
    }

    fn observe_animation_spawn(sender_id: u16, payload: &[u8]) {
        // opcode + guid + format(1) + totalBytes(4) + totalChunks(4) + epoch(8)
        const CHUNK_COUNT_OFFSET: usize = BasisNetworkImageCache::HEADER_BYTES + 1 + 4;
        let Some(total_chunks) = Self::read_i32(payload, CHUNK_COUNT_OFFSET).and_then(|n| usize::try_from(n).ok()) else {
            return;
        };
        if total_chunks == 0 {
            return;
        }
        let id = Self::read_guid(payload);
        let mut state = STATE.lock();
        let cost = payload.len() as i64 + (total_chunks as i64).saturating_mul(Self::SLOT_BYTES);
        {
            let Some(entry) = state.images.get(&id) else {
                return;
            };
            if entry.owner_id != sender_id || entry.animation_spawn.is_some() {
                return;
            }
        }
        // Charge the animation chunk-array backbone too — same unbounded-allocation guard as the
        // still spawn above.
        if !Self::try_reserve(&mut state, sender_id, cost, Some(&id)) {
            return;
        }
        if let Some(entry) = state.images.get_mut(&id) {
            entry.animation_spawn = Some(Bytes::from(payload));
            entry.animation_chunks = vec![None; total_chunks];
            entry.bytes += cost;
            TOTAL_BYTES.fetch_add(cost, Ordering::Relaxed);
        }
    }

    /// Remembers where a picture has got to. Taken from whoever sent it rather than from the
    /// owner alone, because control of a card passes to whoever picks it up. That is no wider a
    /// trust surface than it appears: the relay has already handed these exact bytes to the whole
    /// room, and all the cache does is keep what everybody has already been told.
    fn observe_transform(payload: &[u8]) {
        if payload.len() != Self::TRANSFORM_BYTES {
            return;
        }
        let id = Self::read_guid(payload);
        let mut state = STATE.lock();
        let Some(entry) = state.images.get(&id) else {
            return;
        };
        let owner_id = entry.owner_id;
        let first_pose = entry.transform.is_none();
        if first_pose {
            // Charged once. Every later pose overwrites a buffer of the same size, so a card being
            // dragged around the room costs the buffer nothing after the first update.
            if !Self::try_reserve(&mut state, owner_id, Self::TRANSFORM_BYTES as i64, Some(&id)) {
                return;
            }
        }
        if let Some(entry) = state.images.get_mut(&id) {
            if first_pose {
                entry.bytes += Self::TRANSFORM_BYTES as i64;
                TOTAL_BYTES.fetch_add(Self::TRANSFORM_BYTES as i64, Ordering::Relaxed);
            }
            entry.transform = Some(Bytes::from(payload));
        }
    }

    fn observe_chunk(sender_id: u16, payload: &[u8], animation: bool) {
        // opcode + guid + chunkIndex(4) + length(4) + bytes
        const CHUNK_INDEX_OFFSET: usize = BasisNetworkImageCache::HEADER_BYTES;
        if payload.len() < CHUNK_INDEX_OFFSET + 8 {
            return;
        }
        let Some(chunk_index) = Self::read_i32(payload, CHUNK_INDEX_OFFSET).and_then(|n| usize::try_from(n).ok()) else {
            return;
        };
        let id = Self::read_guid(payload);

        let became_servable = {
            let mut state = STATE.lock();
            {
                let Some(entry) = state.images.get(&id) else {
                    return;
                };
                if entry.owner_id != sender_id {
                    return;
                }
                let slots = if animation { &entry.animation_chunks } else { &entry.chunks };
                if chunk_index >= slots.len() || slots[chunk_index].is_some() {
                    return;
                }
            }
            let cost = payload.len() as i64;
            if !Self::try_reserve(&mut state, sender_id, cost, Some(&id)) {
                return;
            }
            let Some(entry) = state.images.get_mut(&id) else {
                return;
            };
            let slots = if animation { &mut entry.animation_chunks } else { &mut entry.chunks };
            slots[chunk_index] = Some(Bytes::from(payload));
            if animation {
                entry.animation_chunks_held += 1;
            } else {
                entry.chunks_held += 1;
            }
            entry.bytes += cost;
            TOTAL_BYTES.fetch_add(cost, Ordering::Relaxed);
            !animation && entry.still_complete()
        };

        if became_servable {
            Self::notify_owner(sender_id, &id, true);
            Self::offer_to_room(&id);
        }
    }

    /// Makes room for `cost` on behalf of `owner_id`. Fairness is per owner: an owner over their
    /// share evicts their OWN oldest images and never anyone else's, so one person filling the
    /// buffer cannot push out everybody else's pictures.
    fn try_reserve(state: &mut CacheState, owner_id: u16, cost: i64, exclude: Option<&ImageId>) -> bool {
        let cap = Self::max_bytes();
        if cap <= 0 || cost <= 0 || cost > cap {
            return false;
        }
        let share = Self::owner_share(state, owner_id, cap);
        if cost > share {
            // No amount of evicting this owner's other images makes a single oversized one fit.
            return false;
        }
        while Self::owner_bytes(state, owner_id) + cost > share {
            if !Self::evict_oldest_owned_by(state, owner_id, exclude) {
                return false;
            }
        }
        while TOTAL_BYTES.load(Ordering::Relaxed) + cost > cap {
            if !Self::evict_oldest_of_heaviest_owner(state, exclude) {
                return false;
            }
        }
        true
    }

    /// Each distinct owner gets an equal slice of the buffer, never less than the configured
    /// minimum. The owner being admitted is counted even when they hold nothing yet.
    fn owner_share(state: &CacheState, owner_id: u16, cap: i64) -> i64 {
        let mut owners: HashSet<u16> = state.images.values().map(|e| e.owner_id).collect();
        owners.insert(owner_id);
        let share = cap / (owners.len().max(1) as i64);
        let floor = cap.min(Self::minimum_owner_bytes());
        share.max(floor)
    }

    fn owner_bytes(state: &CacheState, owner_id: u16) -> i64 {
        state.images.values().filter(|e| e.owner_id == owner_id).map(|e| e.bytes).sum()
    }

    fn evict_oldest_owned_by(state: &mut CacheState, owner_id: u16, exclude: Option<&ImageId>) -> bool {
        let oldest = state
            .images
            .iter()
            .filter(|(id, e)| e.owner_id == owner_id && exclude != Some(*id))
            .min_by_key(|(_, e)| e.sequence)
            .map(|(id, _)| *id);
        oldest.is_some_and(|id| Self::drop_locked(state, &id))
    }

    fn evict_oldest_of_heaviest_owner(state: &mut CacheState, exclude: Option<&ImageId>) -> bool {
        let mut by_owner: HashMap<u16, i64> = HashMap::new();
        for entry in state.images.values() {
            *by_owner.entry(entry.owner_id).or_insert(0) += entry.bytes;
        }
        let heaviest = by_owner.iter().max_by_key(|(_, bytes)| **bytes).map(|(owner, _)| *owner);
        heaviest.is_some_and(|owner| Self::evict_oldest_owned_by(state, owner, exclude))
    }

    fn drop_locked(state: &mut CacheState, id: &ImageId) -> bool {
        let Some(entry) = state.images.remove(id) else {
            return false;
        };
        TOTAL_BYTES.fetch_sub(entry.bytes, Ordering::Relaxed);
        // Tell the owner they are back on the hook for this one. Without it an evicted image
        // would silently stop reaching new arrivals: the owner still believes we hold it and
        // skips re-sending, and we no longer have anything to send.
        if entry.still_complete() {
            Self::notify_owner(entry.owner_id, id, false);
        }
        true
    }

    /// Tells one player whether the server is holding a given image of theirs. Best effort: a
    /// peer that has already gone simply is not there to tell.
    fn notify_owner(owner_id: u16, id: &ImageId, held: bool) {
        let Some(manager_net_id) = Self::manager_net_id() else {
            return;
        };
        let Some(owner) = NetworkServer::authenticated_peers().get(&i32::from(owner_id)).map(|p| p.value().clone()) else {
            return;
        };
        let mut payload = vec![0u8; Self::HEADER_BYTES + 1];
        payload[0] = Self::OP_SERVER_CACHE_STATE;
        payload[Self::OPCODE_BYTES..Self::HEADER_BYTES].copy_from_slice(id);
        payload[Self::HEADER_BYTES] = u8::from(held);

        let mut writer = NetworkServer::rent_writer();
        Self::send_payload(&owner, &mut writer, manager_net_id, owner_id, &payload);
        NetworkServer::return_writer(writer);
    }

    /// Drops a cached image. `owner_only` restricts it to the player who shared it, which is what
    /// an OpDespawn off the wire gets: anyone may ask, only the owner's word removes the copy.
    pub fn remove(id: ImageId, requester_id: u16, owner_only: bool) -> bool {
        let mut state = STATE.lock();
        let Some(entry) = state.images.get(&id) else {
            return false;
        };
        if owner_only && entry.owner_id != requester_id {
            return false;
        }
        Self::drop_locked(&mut state, &id)
    }

    /// Drops everything a departing player shared. Clients already destroy that player's images
    /// on disconnect, so keeping them cached would hand a joiner pictures nobody else can see.
    pub fn remove_player_images(peer_id: i32) {
        let Ok(owner_id) = u16::try_from(peer_id) else {
            return;
        };
        let mut state = STATE.lock();
        let doomed: Vec<ImageId> = state.images.iter().filter(|(_, e)| e.owner_id == owner_id).map(|(id, _)| *id).collect();
        for id in &doomed {
            Self::drop_locked(&mut state, id);
        }
        for entry in state.images.values_mut() {
            entry.offered.remove(&owner_id);
            entry.delivered.remove(&owner_id);
        }
    }

    /// Tells an arriving peer what the room is holding, in the order it was shared, and stops
    /// there. Each offer is the sharer's own spawn header — tens of bytes — so a joiner arriving
    /// into an instance full of pictures pays for a catalogue rather than a gallery.
    pub fn offer_cached_images_to_peer(new_connection: &NetPeerRef) {
        if !Self::enabled() {
            return;
        }
        let Some(manager_net_id) = Self::manager_net_id() else {
            return;
        };
        let Ok(recipient) = u16::try_from(new_connection.id()) else {
            return;
        };
        let offers: Vec<Vec<u8>> = {
            let mut state = STATE.lock();
            let mut ordered: Vec<ImageId> = state.images.keys().copied().collect();
            ordered.sort_by_key(|id| state.images.get(id).map(|e| e.sequence).unwrap_or(i64::MAX));
            let mut offers = Vec::new();
            for id in ordered {
                let Some(entry) = state.images.get_mut(&id) else {
                    continue;
                };
                if !Self::should_offer(entry, recipient) {
                    continue;
                }
                entry.offered.insert(recipient);
                offers.push(Self::build_offer(entry));
            }
            offers
        };
        Self::send_offers(new_connection, manager_net_id, &offers);
    }

    /// Tells everyone already in the room about an image that has just finished arriving. The
    /// sharer only sent it to the players it considered close enough, so without this the rest
    /// of the room would never learn the picture exists.
    fn offer_to_room(id: &ImageId) {
        if !Self::enabled() {
            return;
        }
        let Some(manager_net_id) = Self::manager_net_id() else {
            return;
        };
        for peer in NetworkServer::peer_snapshot().iter() {
            let Ok(recipient) = u16::try_from(peer.id()) else {
                continue;
            };
            let offer = {
                let mut state = STATE.lock();
                let Some(entry) = state.images.get_mut(id) else {
                    continue;
                };
                if !Self::should_offer(entry, recipient) {
                    continue;
                }
                entry.offered.insert(recipient);
                Self::build_offer(entry)
            };
            let mut writer = NetworkServer::rent_writer();
            Self::send_payload(peer, &mut writer, manager_net_id, recipient, &offer);
            NetworkServer::return_writer(writer);
        }
    }

    fn should_offer(entry: &CachedImage, recipient: u16) -> bool {
        entry.still_complete() && entry.owner_id != recipient && !entry.offered.contains(&recipient) && !entry.delivered.contains(&recipient)
    }

    /// An offer is the spawn header with one byte changed. The position inside it is what the
    /// client measures its distance against, so it has to say where the picture is now.
    fn build_offer(entry: &CachedImage) -> Vec<u8> {
        let mut offer = Self::build_spawn(entry);
        if let Some(first) = offer.first_mut() {
            *first = Self::OP_SERVER_CACHE_OFFER;
        }
        offer
    }

    /// The retained spawn header with the latest pose written over the one it was shared at.
    /// Copying rather than mutating keeps the retained header exactly as the sharer wrote it, and
    /// a replay already queued for somebody else keeps the bytes it was handed.
    fn build_spawn(entry: &CachedImage) -> Vec<u8> {
        let mut spawn: Vec<u8> = entry.spawn.as_deref().unwrap_or(&[]).to_vec();
        if let Some(transform) = entry.transform.as_deref()
            && entry.pose_offset > 0
            && entry.pose_offset + Self::POSE_BYTES <= spawn.len()
            && transform.len() >= Self::HEADER_BYTES + Self::POSE_BYTES
        {
            spawn[entry.pose_offset..entry.pose_offset + Self::POSE_BYTES]
                .copy_from_slice(&transform[Self::HEADER_BYTES..Self::HEADER_BYTES + Self::POSE_BYTES]);
        }
        spawn
    }

    /// Offers go out unmetered and stamped with the recipient's own id rather than the sharer's.
    /// They are a handful of bytes each, and the client only trusts an offer that arrives under
    /// its own id — the relay never lets one client forge that.
    fn send_offers(peer: &NetPeerRef, manager_net_id: u16, offers: &[Vec<u8>]) {
        if offers.is_empty() {
            return;
        }
        let peer_id = peer.id() as u16;
        let mut writer = NetworkServer::rent_writer();
        for offer in offers {
            Self::send_payload(peer, &mut writer, manager_net_id, peer_id, offer);
        }
        NetworkServer::return_writer(writer);
        BNL::log(format!("Image cache offered {} image(s) to peer {}.", offers.len(), peer.id()));
    }

    /// A client asking for one of the images it was offered. This is the only thing that moves
    /// image bytes out of the cache, and it moves them once per player per image.
    fn observe_request(sender_id: u16, payload: &[u8]) {
        if payload.len() < Self::HEADER_BYTES {
            return;
        }
        Self::serve_requested_image(sender_id, Self::read_guid(payload));
    }

    pub fn serve_requested_image(requester_id: u16, id: ImageId) {
        if !Self::enabled() {
            return;
        }
        let Some(manager_net_id) = Self::manager_net_id() else {
            return;
        };
        let Some(peer) = NetworkServer::authenticated_peers().get(&i32::from(requester_id)).map(|p| p.value().clone()) else {
            return;
        };

        // Flatten in the order the room was built, so the pump can meter the stream without
        // knowing anything about images. Ordering matters on the wire: a chunk before its spawn
        // header is discarded by the receiver.
        let queued: Vec<PendingPayload> = {
            let mut state = STATE.lock();
            let Some(entry) = state.images.get_mut(&id) else {
                return;
            };
            if !entry.still_complete() || entry.owner_id == requester_id {
                return;
            }
            if !entry.delivered.insert(requester_id) {
                return;
            }
            entry.offered.insert(requester_id);

            let owner = entry.owner_id;
            let mut queued = vec![PendingPayload::new(owner, Self::build_spawn(entry))];
            if let Some(transform) = &entry.transform {
                // Ahead of the chunks rather than after them: the receiver raises its card off
                // the header, so pose and scale land while the picture is still loading.
                queued.push(PendingPayload { owner_id: owner, payload: transform.clone() });
            }
            queued.extend(entry.chunks.iter().flatten().map(|chunk| PendingPayload { owner_id: owner, payload: chunk.clone() }));
            if entry.animation_complete() {
                if let Some(spawn) = &entry.animation_spawn {
                    queued.push(PendingPayload { owner_id: owner, payload: spawn.clone() });
                }
                queued.extend(entry.animation_chunks.iter().flatten().map(|chunk| PendingPayload { owner_id: owner, payload: chunk.clone() }));
            }
            queued
        };
        if queued.is_empty() {
            return;
        }

        // Paced when the operator has set a download rate, inline when they have not. Sending
        // inline is the historical behaviour and stays reachable deliberately: a small instance
        // on a fast LAN has nothing to gain from metering, and 0 should mean "as fast as it will
        // go" rather than some hidden default.
        BasisImageBandwidthGovernor::set_send_payload(Some(Arc::new(Self::replay_single_payload)));
        let queued_count = queued.len();
        if BasisImageBandwidthGovernor::enqueue_replay(&peer, queued.clone()) {
            BNL::log(format!("Image cache queued {queued_count} payload(s) for requesting peer {requester_id} (paced)."));
            return;
        }

        let mut writer = NetworkServer::rent_writer();
        let mut sent = 0;
        for pending in &queued {
            sent += Self::send_payload(&peer, &mut writer, manager_net_id, pending.owner_id, &pending.payload);
        }
        NetworkServer::return_writer(writer);
        if sent > 0 {
            BNL::log(format!("Image cache served {sent} payload(s) to requesting peer {requester_id}."));
        }
    }

    /// Pump callback: one metered payload, rented writer and all. Resolves the manager id at send
    /// time rather than capturing it, because a replay now spans many milliseconds and the id
    /// can be reset by a despawn of the whole manager in between.
    fn replay_single_payload(peer: &NetPeerRef, owner_id: u16, payload: &[u8]) {
        let Some(manager_net_id) = Self::manager_net_id() else {
            return;
        };
        let mut writer = NetworkServer::rent_writer();
        Self::send_payload(peer, &mut writer, manager_net_id, owner_id, payload);
        NetworkServer::return_writer(writer);
    }

    fn send_payload(peer: &NetPeerRef, writer: &mut NetDataWriter, manager_net_id: u16, owner_id: u16, payload: &[u8]) -> usize {
        let mut message = ServerSceneDataMessage {
            scene_data_message: RemoteSceneDataMessage { message_index: manager_net_id, payload: Some(payload.to_vec()), payload_length: payload.len() },
            player_id_message: PlayerIdMessage::new(owner_id),
        };
        writer.reset();
        if message.serialize(writer).is_err() {
            return 0;
        }
        NetworkServer::try_send(peer, writer, BasisNetworkCommons::DIRECT_SCENE_SERVER_CHANNEL, DeliveryMethod::ReliableOrdered);
        1
    }

    fn read_guid(payload: &[u8]) -> ImageId {
        let mut raw = [0u8; 16];
        if payload.len() >= Self::HEADER_BYTES {
            raw.copy_from_slice(&payload[Self::OPCODE_BYTES..Self::HEADER_BYTES]);
        }
        raw
    }

    fn read_i32(payload: &[u8], offset: usize) -> Option<i32> {
        payload.get(offset..offset + 4).and_then(|b| b.try_into().ok()).map(i32::from_le_bytes)
    }

    /// Reads totalChunks out of an OpSpawn header, and where the pose that follows it begins.
    /// The owner name in front of both is a BinaryWriter string — a 7-bit encoded byte length
    /// then UTF8 — so the fields after it sit at a variable offset and have to be walked to.
    fn try_read_spawn_header(payload: &[u8]) -> Option<(usize, usize)> {
        let mut offset = Self::HEADER_BYTES + 2; // opcode + guid + ushort ownerId
        Self::try_skip_wire_string(payload, &mut offset)?;
        // width, height, totalBytes, totalChunks
        offset += 4 + 4 + 4;
        let total_chunks = Self::read_i32(payload, offset)?;
        let total_chunks = usize::try_from(total_chunks).ok()?;
        Some((total_chunks, offset + 4))
    }

    /// Marks whoever the sharer already sent this image to as holding it. The relay knows
    /// precisely who that was, and without seeding it here the cache would offer the same
    /// picture back to everyone who was standing nearby when it was shared. An untargeted share
    /// went to the whole room, so everyone counts.
    fn seed_already_held(entry: &mut CachedImage, recipients: Option<&[u16]>, recipients_size: usize) {
        if let Some(recipients) = recipients
            && recipients_size > 0
        {
            for recipient in recipients.iter().take(recipients_size) {
                entry.offered.insert(*recipient);
                entry.delivered.insert(*recipient);
            }
            return;
        }
        for peer in NetworkServer::authenticated_peers().iter() {
            if let Ok(peer_id) = u16::try_from(*peer.key()) {
                entry.offered.insert(peer_id);
                entry.delivered.insert(peer_id);
            }
        }
    }

    fn try_skip_wire_string(payload: &[u8], offset: &mut usize) -> Option<()> {
        let mut length: usize = 0;
        let mut shift = 0;
        loop {
            if *offset >= payload.len() || shift > 4 * 7 {
                return None;
            }
            let piece = payload[*offset];
            *offset += 1;
            length |= usize::from(piece & 0x7F) << shift;
            if piece & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        if length > Self::MAX_OWNER_NAME_BYTES || *offset + length > payload.len() {
            return None;
        }
        *offset += length;
        Some(())
    }
}
