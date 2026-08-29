//! The server-side image buffer: what it retains, when it hands images to a joiner, and the
//! per-owner fairness that stops one player's uploads crowding out everyone else's. These drive
//! `BasisNetworkImageCache::observe` with the same bytes a client puts on the wire, so the header
//! walking (including the variable-length owner name) is exercised for real.
//!
//! Mutates the server configuration and the network ID database, both process-wide statics.

use std::sync::Arc;

use basis_network_core::BasisNetworkCommons;
use basis_network_core::configuration::Configuration;
use basis_network_server::NetworkServer;
use basis_network_server::identity::basis_network_id_database::BasisNetworkIDDatabase;
use basis_network_server::networking::{BasisImageBandwidthGovernor, BasisNetworkImageCache, ImageId};
use basis_server_tests::support::fake_peer::SentPacket;
use basis_server_tests::support::FakePeer;
use serial_test::serial;

/// Chunk payload sized so a megabyte-granularity budget still exercises eviction.
const CHUNK_BYTES: usize = 64 * 1024;

/// Server to client offer, mirrored from the cache's wire protocol.
const OP_SERVER_CACHE_OFFER: u8 = 9;
const OP_SPAWN: u8 = 1;
const OP_CHUNK: u8 = 2;
const OP_TRANSFORM: u8 = 3;
const OP_DESPAWN: u8 = 4;
const OP_ANIMATION_SPAWN: u8 = 6;
const OP_ANIMATION_CHUNK: u8 = 7;

const MANAGER_NET_ID: u16 = 4242;

struct Fixture {
    previous: Option<Arc<Configuration>>,
    registered_peer_ids: Vec<i32>,
}

impl Fixture {
    fn new() -> Self {
        let previous = NetworkServer::configuration();
        NetworkServer::set_configuration(Configuration {
            image_cache_enabled: true,
            image_cache_max_megabytes: 4,
            image_cache_minimum_per_owner_megabytes: 0,
            // Replay inline, which is what 0 means. These tests are about WHAT the cache serves,
            // not how fast. The paced path has its own tests in the governor suite.
            image_share_download_megabits_per_second: 0,
            image_pickup_range_meters: 0.0,
            ..Configuration::default()
        });
        BasisNetworkImageCache::reset();
        BasisImageBandwidthGovernor::reset();
        BasisNetworkIDDatabase::ushort_network_database().insert(BasisNetworkImageCache::IMAGE_MANAGER_IDENTIFIER.to_string(), MANAGER_NET_ID);
        Self { previous, registered_peer_ids: Vec::new() }
    }

    fn configure(&self, edit: impl FnOnce(&mut Configuration)) {
        NetworkServer::update_configuration(edit);
    }

    /// Registers a stand-in peer the cache can reach through the authenticated-peer table,
    /// remembering it so teardown removes only what this fixture added.
    fn register_peer(&mut self, id: i32) -> Arc<FakePeer> {
        let peer = FakePeer::new(id);
        NetworkServer::authenticated_peers().insert(id, peer.as_ref());
        self.registered_peer_ids.push(id);
        peer
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        BasisNetworkImageCache::reset();
        BasisNetworkIDDatabase::ushort_network_database().remove(BasisNetworkImageCache::IMAGE_MANAGER_IDENTIFIER);
        match self.previous.take() {
            Some(previous) => NetworkServer::set_configuration((*previous).clone()),
            None => NetworkServer::clear_configuration(),
        }
        for id in &self.registered_peer_ids {
            NetworkServer::authenticated_peers().remove(id);
        }
    }
}

// ── wire helpers: byte-for-byte what BasisImagePickupManager encodes ──

/// A `BinaryWriter` stand-in: little-endian scalars and 7-bit length-prefixed UTF-8 strings.
#[derive(Default)]
struct BinaryWriter(Vec<u8>);

impl BinaryWriter {
    fn byte(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.0.extend_from_slice(v);
        self
    }
    fn u16(&mut self, v: u16) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }
    fn i32(&mut self, v: i32) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }
    fn i64(&mut self, v: i64) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }
    fn f32(&mut self, v: f32) -> &mut Self {
        self.bytes(&v.to_le_bytes())
    }
    fn string(&mut self, v: &str) -> &mut Self {
        let mut length = v.len();
        loop {
            let piece = (length & 0x7F) as u8;
            length >>= 7;
            if length == 0 {
                self.0.push(piece);
                break;
            }
            self.0.push(piece | 0x80);
        }
        self.bytes(v.as_bytes())
    }
    fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.0)
    }
}

fn new_id() -> ImageId {
    *uuid::Uuid::new_v4().as_bytes()
}

/// The image payload inside one recorded send. ServerSceneDataMessage puts the player id and the
/// message index in front of it and writes the payload raw, so the offset is fixed.
fn payload_of(sent: &SentPacket) -> Vec<u8> {
    const PAYLOAD_OFFSET: usize = 2 + 2;
    sent.data[PAYLOAD_OFFSET..].to_vec()
}

fn payload_opcode(sent: &SentPacket) -> u8 {
    payload_of(sent)[0]
}

fn encode_spawn(id: ImageId, owner_id: u16, owner_name: &str, total_chunks: i32, position: (f32, f32, f32)) -> Vec<u8> {
    let mut w = BinaryWriter::default();
    w.byte(OP_SPAWN).bytes(&id).u16(owner_id).string(owner_name).i32(64).i32(64).i32(total_chunks.wrapping_mul(16)).i32(total_chunks).f32(position.0).f32(position.1).f32(position.2);
    for _ in 0..4 {
        w.f32(0.0);
    }
    w.finish()
}

fn encode_chunk(id: ImageId, chunk_index: i32, payload_bytes: usize, opcode: u8) -> Vec<u8> {
    let mut w = BinaryWriter::default();
    w.byte(opcode).bytes(&id).i32(chunk_index).i32(payload_bytes as i32).bytes(&vec![0u8; payload_bytes]);
    w.finish()
}

fn encode_transform(id: ImageId, position: (f32, f32, f32), rotation: (f32, f32, f32, f32), scale: f32) -> Vec<u8> {
    let mut w = BinaryWriter::default();
    w.byte(OP_TRANSFORM).bytes(&id).f32(position.0).f32(position.1).f32(position.2).f32(rotation.0).f32(rotation.1).f32(rotation.2).f32(rotation.3).f32(scale);
    w.finish()
}

const IDENTITY: (f32, f32, f32, f32) = (0.0, 0.0, 0.0, 1.0);

/// Walks a spawn header or an offer the way the client does, so the pose is read past the
/// variable-length owner name rather than from an offset this fixture assumes.
fn read_spawn_pose(header: &[u8]) -> (f32, f32, f32, f32, f32, f32, f32) {
    let mut offset = 1 + 16 + 2;
    let mut length = 0usize;
    let mut shift = 0;
    loop {
        let piece = header[offset];
        offset += 1;
        length |= usize::from(piece & 0x7F) << shift;
        if piece & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    offset += length + 4 * 4;
    let mut next = || {
        let v = f32::from_le_bytes(header[offset..offset + 4].try_into().expect("f32"));
        offset += 4;
        v
    };
    (next(), next(), next(), next(), next(), next(), next())
}

fn encode_animation_spawn(id: ImageId, total_chunks: i32) -> Vec<u8> {
    let mut w = BinaryWriter::default();
    w.byte(OP_ANIMATION_SPAWN).bytes(&id).byte(2).i32(total_chunks.wrapping_mul(16)).i32(total_chunks).i64(0);
    w.finish()
}

fn encode_despawn(id: ImageId) -> Vec<u8> {
    let mut w = BinaryWriter::default();
    w.byte(OP_DESPAWN).bytes(&id);
    w.finish()
}

fn observe(sender: u16, payload: &[u8]) {
    // HandleScene reaches observe only through is_image_traffic, which is also what resolves the
    // manager's network id. Going through the same gate keeps the fixture honest about the order
    // the server does it in.
    BasisNetworkImageCache::is_image_traffic(MANAGER_NET_ID);
    BasisNetworkImageCache::observe(sender, payload, None, 0);
}

/// Observes a share the sharer aimed at a specific set of players.
fn observe_targeted(sender: u16, payload: &[u8], recipients: &[u16]) {
    BasisNetworkImageCache::is_image_traffic(MANAGER_NET_ID);
    BasisNetworkImageCache::observe(sender, payload, Some(recipients), recipients.len());
}

fn share_image_at(owner: u16, chunks: i32, chunk_bytes: usize, position: (f32, f32, f32)) -> ImageId {
    let id = new_id();
    observe(owner, &encode_spawn(id, owner, "Sharer", chunks, position));
    for index in 0..chunks {
        observe(owner, &encode_chunk(id, index, chunk_bytes, OP_CHUNK));
    }
    id
}

fn share_image(owner: u16, chunks: i32) -> ImageId {
    share_image_at(owner, chunks, CHUNK_BYTES, (0.0, 0.0, 0.0))
}

// ── identification ──

#[test]
#[serial(network_statics)]
fn is_image_traffic_matches_only_the_image_managers_network_id() {
    let _f = Fixture::new();
    assert!(BasisNetworkImageCache::is_image_traffic(MANAGER_NET_ID));
    assert!(!BasisNetworkImageCache::is_image_traffic(MANAGER_NET_ID + 1));
}

// ── retention ──

#[test]
#[serial(network_statics)]
fn a_fully_received_image_is_held_and_servable() {
    let _f = Fixture::new();
    share_image(7, 2);
    assert_eq!(BasisNetworkImageCache::count(), 1);
    assert_eq!(BasisNetworkImageCache::servable_count(), 1);
    assert!(BasisNetworkImageCache::total_bytes() > 0);
}

#[test]
#[serial(network_statics)]
fn an_image_missing_chunks_is_held_but_not_served() {
    let _f = Fixture::new();
    // A joiner must never be handed a half-received picture; it stays pending until complete.
    let id = new_id();
    observe(7, &encode_spawn(id, 7, "Sharer", 3, (0.0, 0.0, 0.0)));
    observe(7, &encode_chunk(id, 0, CHUNK_BYTES, OP_CHUNK));
    assert_eq!(BasisNetworkImageCache::count(), 1);
    assert_eq!(BasisNetworkImageCache::servable_count(), 0);
}

#[test]
#[serial(network_statics)]
fn net_id_zero_owner_is_cached_like_any_other_player() {
    let _f = Fixture::new();
    // Peer ids are handed out from zero up, so the first player to join is net id 0 and must not
    // be mistaken for a blank owner.
    share_image(0, 2);
    assert_eq!(BasisNetworkImageCache::servable_count(), 1);
    assert!(BasisNetworkImageCache::bytes_held_for(0) > 0);
}

#[test]
#[serial(network_statics)]
fn animation_payloads_are_held_alongside_the_still() {
    let _f = Fixture::new();
    let id = share_image(7, 2);
    let still_only = BasisNetworkImageCache::total_bytes();

    observe(7, &encode_animation_spawn(id, 2));
    observe(7, &encode_chunk(id, 0, CHUNK_BYTES, OP_ANIMATION_CHUNK));
    observe(7, &encode_chunk(id, 1, CHUNK_BYTES, OP_ANIMATION_CHUNK));

    assert!(BasisNetworkImageCache::total_bytes() > still_only);
    assert_eq!(BasisNetworkImageCache::count(), 1);
}

#[test]
#[serial(network_statics)]
fn a_repeated_spawn_header_does_not_double_count() {
    let _f = Fixture::new();
    let id = new_id();
    let spawn = encode_spawn(id, 7, "Sharer", 1, (0.0, 0.0, 0.0));
    observe(7, &spawn);
    let after_first = BasisNetworkImageCache::total_bytes();
    observe(7, &spawn);
    assert_eq!(BasisNetworkImageCache::total_bytes(), after_first);
    assert_eq!(BasisNetworkImageCache::count(), 1);
}

// ── removal ──

#[test]
#[serial(network_statics)]
fn the_owners_despawn_clears_the_server_copy() {
    let _f = Fixture::new();
    let id = share_image(7, 2);
    observe(7, &encode_despawn(id));
    assert_eq!(BasisNetworkImageCache::count(), 0);
    assert_eq!(BasisNetworkImageCache::total_bytes(), 0);
}

#[test]
#[serial(network_statics)]
fn a_despawn_from_somebody_else_leaves_the_server_copy_alone() {
    let _f = Fixture::new();
    // Anyone may ask; only the player who shared it removes the server's copy.
    let id = share_image(7, 2);
    observe(9, &encode_despawn(id));
    assert_eq!(BasisNetworkImageCache::count(), 1);
}

#[test]
#[serial(network_statics)]
fn remove_request_clears_regardless_of_requester_when_not_owner_gated() {
    let _f = Fixture::new();
    // The moderation path removes on someone else's behalf, so it opts out of the owner gate.
    let id = share_image(7, 2);
    assert!(BasisNetworkImageCache::remove(id, 9, false));
    assert_eq!(BasisNetworkImageCache::count(), 0);
}

#[test]
#[serial(network_statics)]
fn when_the_sharer_disconnects_their_images_are_dropped() {
    let _f = Fixture::new();
    share_image(7, 2);
    share_image(7, 2);
    share_image(9, 2);

    BasisNetworkImageCache::remove_player_images(7);

    assert_eq!(BasisNetworkImageCache::count(), 1);
    assert_eq!(BasisNetworkImageCache::bytes_held_for(7), 0);
    assert!(BasisNetworkImageCache::bytes_held_for(9) > 0);
}

// ── budget and fairness ──

#[test]
#[serial(network_statics)]
fn an_image_bigger_than_the_whole_buffer_is_not_cached() {
    let f = Fixture::new();
    f.configure(|c| c.image_cache_max_megabytes = 1);
    share_image_at(7, 2, 1024 * 1024, (0.0, 0.0, 0.0));
    assert_eq!(BasisNetworkImageCache::servable_count(), 0);
    assert!(BasisNetworkImageCache::total_bytes() <= 1024 * 1024);
}

#[test]
#[serial(network_statics)]
fn the_buffer_never_exceeds_its_cap() {
    let f = Fixture::new();
    f.configure(|c| c.image_cache_max_megabytes = 1);
    let cap = 1024 * 1024;
    for index in 0..40 {
        share_image((index % 4) as u16, 2);
        assert!(BasisNetworkImageCache::total_bytes() <= cap, "cache overran its cap after {} shares", index + 1);
    }
}

#[test]
#[serial(network_statics)]
fn one_player_flooding_the_buffer_cannot_evict_another_players_images() {
    let f = Fixture::new();
    // The fairness rule: an owner over their slice evicts their OWN oldest image. Without it,
    // whoever uploads most simply deletes everybody else's pictures from the cache.
    f.configure(|c| c.image_cache_max_megabytes = 1);

    share_image(1, 1);
    let quiet_owner_bytes = BasisNetworkImageCache::bytes_held_for(1);
    assert!(quiet_owner_bytes > 0);

    for _ in 0..30 {
        share_image(2, 2);
    }
    assert_eq!(BasisNetworkImageCache::bytes_held_for(1), quiet_owner_bytes);
}

#[test]
#[serial(network_statics)]
fn an_owner_over_their_share_loses_their_own_oldest_image_first() {
    let f = Fixture::new();
    f.configure(|c| c.image_cache_max_megabytes = 1);

    let oldest = share_image(5, 1);
    for _ in 0..30 {
        share_image(5, 1);
    }
    // The first image they shared is the first to go, and they still hold something.
    assert!(!BasisNetworkImageCache::remove(oldest, 5, true));
    assert!(BasisNetworkImageCache::bytes_held_for(5) > 0);
}

// ── disabled ──

#[test]
#[serial(network_statics)]
fn with_the_cache_off_nothing_is_retained() {
    let f = Fixture::new();
    f.configure(|c| c.image_cache_enabled = false);
    share_image(7, 2);
    assert_eq!(BasisNetworkImageCache::count(), 0);
    assert_eq!(BasisNetworkImageCache::total_bytes(), 0);
}

#[test]
#[serial(network_statics)]
fn with_a_zero_budget_nothing_is_retained() {
    let f = Fixture::new();
    f.configure(|c| c.image_cache_max_megabytes = 0);
    share_image(7, 2);
    assert_eq!(BasisNetworkImageCache::count(), 0);
}

// ── offer to a joiner, replay on request ──

/// Joining costs a catalogue, not a gallery. One offer per image and not a single chunk until the
/// client has decided the picture is close enough to be worth having.
#[test]
#[serial(network_statics)]
fn a_joiner_is_offered_each_image_and_sent_no_chunks() {
    let mut f = Fixture::new();
    share_image(7, 3);
    share_image(7, 3);

    let joiner = f.register_peer(9);
    BasisNetworkImageCache::offer_cached_images_to_peer(&joiner.as_ref());

    let sent = joiner.sent.lock();
    assert_eq!(sent.len(), 2);
    assert!(sent.iter().all(|s| payload_opcode(s) == OP_SERVER_CACHE_OFFER));
}

/// The offer is the sharer's own spawn header with one byte changed, so the position the client
/// needs rides along without the server ever reading it.
#[test]
#[serial(network_statics)]
fn an_offer_carries_the_sharers_spawn_header_verbatim_apart_from_the_opcode() {
    let mut f = Fixture::new();
    let id = share_image_at(7, 2, CHUNK_BYTES, (12.5, 0.0, -3.0));

    let joiner = f.register_peer(9);
    BasisNetworkImageCache::offer_cached_images_to_peer(&joiner.as_ref());

    let offer = payload_of(&joiner.sent.lock()[0]);
    let mut expected = encode_spawn(id, 7, "Sharer", 2, (12.5, 0.0, -3.0));
    expected[0] = OP_SERVER_CACHE_OFFER;
    assert_eq!(offer, expected);
}

#[test]
#[serial(network_statics)]
fn requesting_an_offered_image_sends_the_spawn_and_every_chunk() {
    let mut f = Fixture::new();
    let id = share_image(7, 3);

    let joiner = f.register_peer(9);
    BasisNetworkImageCache::offer_cached_images_to_peer(&joiner.as_ref());
    joiner.clear_sent();

    BasisNetworkImageCache::serve_requested_image(9, id);
    assert_eq!(joiner.sent_count(), 4);
}

#[test]
#[serial(network_statics)]
fn replayed_images_go_out_on_the_channel_the_image_manager_listens_on() {
    let mut f = Fixture::new();
    // The image pickup manager registers a *direct* scene handler, so its traffic reaches it only
    // on DIRECT_SCENE_SERVER_CHANNEL. Replaying on SCENE_CHANNEL lands in the other handler table,
    // where nothing is registered, and the joiner silently sees no image at all.
    let id = share_image(7, 2);

    let joiner = f.register_peer(9);
    BasisNetworkImageCache::offer_cached_images_to_peer(&joiner.as_ref());
    BasisNetworkImageCache::serve_requested_image(9, id);

    let sent = joiner.sent.lock();
    assert!(!sent.is_empty());
    assert!(sent.iter().all(|s| s.channel == BasisNetworkCommons::DIRECT_SCENE_SERVER_CHANNEL));
}

#[test]
#[serial(network_statics)]
fn an_incomplete_image_is_not_offered() {
    let mut f = Fixture::new();
    let id = new_id();
    observe(7, &encode_spawn(id, 7, "Sharer", 3, (0.0, 0.0, 0.0)));
    observe(7, &encode_chunk(id, 0, CHUNK_BYTES, OP_CHUNK));

    let joiner = f.register_peer(9);
    BasisNetworkImageCache::offer_cached_images_to_peer(&joiner.as_ref());
    assert_eq!(joiner.sent_count(), 0);
}

/// A request for something never offered — or never finished arriving — buys nothing. The cache
/// answers requests, it does not take instructions.
#[test]
#[serial(network_statics)]
fn requesting_an_incomplete_image_sends_nothing() {
    let mut f = Fixture::new();
    let id = new_id();
    observe(7, &encode_spawn(id, 7, "Sharer", 3, (0.0, 0.0, 0.0)));
    observe(7, &encode_chunk(id, 0, CHUNK_BYTES, OP_CHUNK));

    let joiner = f.register_peer(9);
    BasisNetworkImageCache::serve_requested_image(9, id);
    assert_eq!(joiner.sent_count(), 0);
}

#[test]
#[serial(network_statics)]
fn requesting_the_same_image_twice_sends_it_once() {
    let mut f = Fixture::new();
    let id = share_image(7, 2);

    let joiner = f.register_peer(9);
    BasisNetworkImageCache::serve_requested_image(9, id);
    assert!(joiner.sent_count() > 0);

    joiner.clear_sent();
    BasisNetworkImageCache::serve_requested_image(9, id);
    assert_eq!(joiner.sent_count(), 0);
}

#[test]
#[serial(network_statics)]
fn a_peer_is_offered_an_image_only_once() {
    let mut f = Fixture::new();
    share_image(7, 2);

    let joiner = f.register_peer(9);
    BasisNetworkImageCache::offer_cached_images_to_peer(&joiner.as_ref());
    assert!(joiner.sent_count() > 0);

    joiner.clear_sent();
    BasisNetworkImageCache::offer_cached_images_to_peer(&joiner.as_ref());
    assert_eq!(joiner.sent_count(), 0);
}

/// The sharer already sent this to peer 9, and the relay saw exactly who it was aimed at. Offering
/// it back would invite them to download a picture they are already holding.
#[test]
#[serial(network_statics)]
fn a_peer_the_sharer_already_targeted_is_not_offered() {
    let mut f = Fixture::new();
    let nearby = f.register_peer(9);

    let id = new_id();
    let targeted = [9u16];
    observe_targeted(7, &encode_spawn(id, 7, "Sharer", 2, (0.0, 0.0, 0.0)), &targeted);
    for index in 0..2 {
        observe_targeted(7, &encode_chunk(id, index, CHUNK_BYTES, OP_CHUNK), &targeted);
    }
    nearby.clear_sent();

    BasisNetworkImageCache::offer_cached_images_to_peer(&nearby.as_ref());
    assert_eq!(nearby.sent_count(), 0);
}

/// The other half: somebody the sharer decided was too far away is exactly who the cache exists
/// for, and they are told the moment the image finishes arriving rather than on their next join.
#[test]
#[serial(network_statics)]
fn a_peer_the_sharer_could_not_reach_is_offered_the_image_as_it_completes() {
    let mut f = Fixture::new();
    let latecomer = f.register_peer(11);

    let id = new_id();
    let targeted = [9u16];
    observe_targeted(7, &encode_spawn(id, 7, "Sharer", 2, (0.0, 0.0, 0.0)), &targeted);
    for index in 0..2 {
        observe_targeted(7, &encode_chunk(id, index, CHUNK_BYTES, OP_CHUNK), &targeted);
    }

    let sent = latecomer.sent.lock();
    assert_eq!(sent.len(), 1);
    assert_eq!(payload_opcode(&sent[0]), OP_SERVER_CACHE_OFFER);
}

#[test]
#[serial(network_statics)]
fn an_owner_is_never_offered_their_own_image() {
    let mut f = Fixture::new();
    let owner = f.register_peer(7);

    share_image(7, 2);
    owner.clear_sent();
    BasisNetworkImageCache::offer_cached_images_to_peer(&owner.as_ref());
    assert_eq!(owner.sent_count(), 0);
}

#[test]
#[serial(network_statics)]
fn with_the_cache_off_a_joiner_is_offered_nothing() {
    let mut f = Fixture::new();
    share_image(7, 2);
    f.configure(|c| c.image_cache_enabled = false);

    let joiner = f.register_peer(9);
    BasisNetworkImageCache::offer_cached_images_to_peer(&joiner.as_ref());
    assert_eq!(joiner.sent_count(), 0);
}

#[test]
#[serial(network_statics)]
fn the_owner_is_told_on_the_same_channel_when_their_image_becomes_servable() {
    let mut f = Fixture::new();
    // The owner only stops re-uploading to each arrival if this notice reaches its handler, which
    // is the direct table again.
    let owner = f.register_peer(7);
    share_image(7, 2);

    let sent = owner.sent.lock();
    assert!(!sent.is_empty());
    assert!(sent.iter().all(|s| s.channel == BasisNetworkCommons::DIRECT_SCENE_SERVER_CHANNEL));
}

#[test]
#[serial(network_statics)]
fn evicting_an_image_tells_its_owner_they_are_providing_it_again() {
    let mut f = Fixture::new();
    let owner = f.register_peer(5);
    f.configure(|c| c.image_cache_max_megabytes = 1);

    share_image(5, 1);
    let after_first_share = owner.sent_count();
    for _ in 0..30 {
        share_image(5, 1);
    }
    assert!(owner.sent_count() > after_first_share);
}

// ── where the picture actually is ──

/// A spawn header says where a picture was hung, and pictures get carried around. The offer is the
/// only thing a joiner measures its distance against, so a stale one both draws the card in the
/// wrong place and can decide a picture propped against the joiner is too far away to want.
#[test]
#[serial(network_statics)]
#[allow(clippy::approx_constant)] // the C# test data is 0.7071, a rotation of 90° about Y
fn an_offer_carries_where_the_image_is_now_not_where_it_was_spawned() {
    let mut f = Fixture::new();
    // Turned as well as moved: the position and the facing travel as one block.
    let id = share_image_at(7, 2, CHUNK_BYTES, (12.5, 0.0, -3.0));
    observe(7, &encode_transform(id, (1.0, 2.0, 3.0), (0.0, 0.7071, 0.0, 0.7071), 1.0));

    let joiner = f.register_peer(9);
    BasisNetworkImageCache::offer_cached_images_to_peer(&joiner.as_ref());

    assert_eq!(read_spawn_pose(&payload_of(&joiner.sent.lock()[0])), (1.0, 2.0, 3.0, 0.0, 0.7071, 0.0, 0.7071));
}

/// Control of a card passes to whoever picks it up, so the player who moved a picture is very often
/// not the player who shared it. Following only the owner would leave every borrowed card frozen
/// where it was put down.
#[test]
#[serial(network_statics)]
fn a_transform_from_whoever_picked_the_image_up_is_followed() {
    let mut f = Fixture::new();
    let id = share_image(7, 2);
    observe(11, &encode_transform(id, (4.0, 5.0, 6.0), (0.5, 0.0, 0.0, 0.5), 1.0));

    let joiner = f.register_peer(9);
    BasisNetworkImageCache::offer_cached_images_to_peer(&joiner.as_ref());

    assert_eq!(read_spawn_pose(&payload_of(&joiner.sent.lock()[0])), (4.0, 5.0, 6.0, 0.5, 0.0, 0.0, 0.5));
}

#[test]
#[serial(network_statics)]
fn requesting_an_image_that_moved_replays_the_pose_ahead_of_the_chunks() {
    let mut f = Fixture::new();
    // Ahead of the chunks because the receiver raises its card off the header: a transform arriving
    // after the last chunk would leave the card loading in the wrong place, and the transform is
    // also the only payload carrying scale.
    let id = share_image(7, 3);
    let moved = encode_transform(id, (1.0, 2.0, 3.0), (0.0, 0.0, 0.3827, 0.9239), 2.5);
    observe(7, &moved);

    let joiner = f.register_peer(9);
    BasisNetworkImageCache::offer_cached_images_to_peer(&joiner.as_ref());
    joiner.clear_sent();

    BasisNetworkImageCache::serve_requested_image(9, id);

    let sent = joiner.sent.lock();
    assert_eq!(sent.len(), 5);
    assert_eq!(payload_opcode(&sent[0]), OP_SPAWN);
    assert_eq!(read_spawn_pose(&payload_of(&sent[0])), (1.0, 2.0, 3.0, 0.0, 0.0, 0.3827, 0.9239));
    assert_eq!(payload_of(&sent[1]), moved);
    assert!(sent[2..].iter().all(|s| payload_opcode(s) == OP_CHUNK));
}

#[test]
#[serial(network_statics)]
fn repeated_transforms_are_charged_once() {
    let _f = Fixture::new();
    // A card being dragged across a room sends one of these several times a second; each has to
    // overwrite the last rather than accumulate, or moving a picture slowly evicts the room.
    let id = share_image(7, 2);
    let before_any_pose = BasisNetworkImageCache::total_bytes();

    observe(7, &encode_transform(id, (1.0, 0.0, 0.0), IDENTITY, 1.0));
    let after_first_pose = BasisNetworkImageCache::total_bytes();
    for step in 0..32 {
        observe(7, &encode_transform(id, (step as f32, 0.0, 0.0), IDENTITY, 1.0));
    }

    assert!(after_first_pose > before_any_pose);
    assert_eq!(BasisNetworkImageCache::total_bytes(), after_first_pose);
}

#[test]
#[serial(network_statics)]
fn a_transform_of_the_wrong_length_leaves_the_pose_alone() {
    let mut f = Fixture::new();
    let id = share_image_at(7, 2, CHUNK_BYTES, (12.5, 0.0, 0.0));
    let transform = encode_transform(id, (1.0, 2.0, 3.0), (0.0, 0.0, 0.0, 0.5), 1.0);
    observe(7, &transform[..30]);

    let joiner = f.register_peer(9);
    BasisNetworkImageCache::offer_cached_images_to_peer(&joiner.as_ref());

    assert_eq!(read_spawn_pose(&payload_of(&joiner.sent.lock()[0])), (12.5, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0));
}

#[test]
#[serial(network_statics)]
fn a_transform_for_an_image_the_cache_does_not_hold_is_ignored() {
    let _f = Fixture::new();
    observe(7, &encode_transform(new_id(), (1.0, 2.0, 3.0), IDENTITY, 1.0));
    assert_eq!(BasisNetworkImageCache::count(), 0);
    assert_eq!(BasisNetworkImageCache::total_bytes(), 0);
}

// ── malformed input ──

#[test]
#[serial(network_statics)]
fn malformed_payloads_are_ignored_without_panicking() {
    let _f = Fixture::new();
    observe(7, &[]);
    observe(7, &[OP_SPAWN]);
    observe(7, &[OP_CHUNK, 1, 2, 3]);
    observe(7, &[OP_TRANSFORM, 1, 2, 3]);

    let truncated_spawn = encode_spawn(new_id(), 7, "Sharer", 2, (0.0, 0.0, 0.0));
    observe(7, &truncated_spawn[..20]);

    // A spawn whose owner-name length prefix runs past the end of the payload.
    let mut runaway_name = vec![OP_SPAWN];
    runaway_name.extend_from_slice(&new_id());
    runaway_name.extend_from_slice(&7u16.to_le_bytes());
    runaway_name.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F]);
    observe(7, &runaway_name);

    // A spawn claiming an absurd chunk count must be refused before anything is allocated.
    let greedy = encode_spawn(new_id(), 7, "Sharer", i32::MAX, (0.0, 0.0, 0.0));
    observe(7, &greedy);
    let negative = encode_spawn(new_id(), 7, "Sharer", -1, (0.0, 0.0, 0.0));
    observe(7, &negative);

    // A chunk for a held image with an index past its declared count, and one whose declared
    // length exceeds the bytes actually present.
    let id = new_id();
    observe(7, &encode_spawn(id, 7, "Sharer", 1, (0.0, 0.0, 0.0)));
    observe(7, &encode_chunk(id, 5, 64, OP_CHUNK));
    observe(7, &encode_chunk(id, -1, 64, OP_CHUNK));
    let mut overlong = encode_chunk(id, 0, 64, OP_CHUNK);
    overlong[17..21].copy_from_slice(&i32::MAX.to_le_bytes());
    observe(7, &overlong);
    assert_eq!(BasisNetworkImageCache::servable_count(), 0);
    observe(7, &encode_despawn(id));

    assert_eq!(BasisNetworkImageCache::count(), 0);
}
