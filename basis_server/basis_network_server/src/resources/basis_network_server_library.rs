//! Port of `Resources/BasisNetworkServerLibrary.cs`.
//!
//! Wire format on `SERVER_LIBRARY_CHANNEL`: `[u16 rawLen][u16 compressedLen][bytes payload]`.
//! If `compressedLen == 0` the payload is the raw `ServerLibraryMessage` bytes. Otherwise the
//! payload is LZ4-encoded and decompresses to `rawLen` bytes.
//!
//! The wire payload is cached and only rebuilt when the admin mutates the library. Per-peer joins
//! just copy the cached bytes into a pooled writer.

use basis_network_core::{BasisNetworkCommons, DeliveryMethod, NetDataWriter, NetPeerRef};
use parking_lot::Mutex;

use crate::NetworkServer;
use crate::networking::BasisDefaultLibraryLoader;

static CACHED_WIRE: Mutex<Vec<u8>> = Mutex::new(Vec::new());

pub struct BasisNetworkServerLibrary;

impl BasisNetworkServerLibrary {
    pub fn send_library_to_peer(peer: &NetPeerRef) {
        let wire = {
            let mut cache = CACHED_WIRE.lock();
            if cache.is_empty() {
                *cache = Self::build_wire();
            }
            cache.clone()
        };
        if wire.is_empty() {
            return;
        }
        let mut writer = NetworkServer::rent_writer();
        writer.put_bytes(&wire);
        NetworkServer::try_send(peer, &writer, BasisNetworkCommons::SERVER_LIBRARY_CHANNEL, DeliveryMethod::ReliableOrdered);
        NetworkServer::return_writer(writer);
    }

    pub fn broadcast_library_to_all() {
        // Library mutated — rebuild cache before broadcasting.
        let wire = {
            let mut cache = CACHED_WIRE.lock();
            *cache = Self::build_wire();
            cache.clone()
        };
        if wire.is_empty() {
            return;
        }
        let mut writer = NetworkServer::rent_writer();
        writer.put_bytes(&wire);
        NetworkServer::broadcast_message_to_clients(
            &writer,
            BasisNetworkCommons::SERVER_LIBRARY_CHANNEL,
            &NetworkServer::peer_snapshot(),
            DeliveryMethod::ReliableOrdered,
        );
        NetworkServer::return_writer(writer);
    }

    /// Forces the next send to rebuild the cache.
    pub fn invalidate_cache() {
        CACHED_WIRE.lock().clear();
    }

    /// The current wire bytes, or empty when there is no library or it does not fit the u16
    /// length fields.
    pub fn build_wire() -> Vec<u8> {
        let loaded = BasisDefaultLibraryLoader::loaded_items();
        // Serialize the items the way ServerLibraryMessage does, byte for byte.
        let mut raw = NetDataWriter::new();
        raw.put_ushort(u16::try_from(loaded.len()).unwrap_or(u16::MAX));
        for item in &loaded {
            raw.put_byte(item.mode);
            if raw.put_string(&item.url).is_err() || raw.put_string(&item.password).is_err() {
                return Vec::new();
            }
        }
        let raw = raw.copy_data();
        let raw_len = raw.len();
        if raw_len == 0 || raw_len > usize::from(u16::MAX) {
            return Vec::new();
        }
        let compressed = lz4_flex::block::compress(&raw);
        let use_compressed = !compressed.is_empty() && compressed.len() < raw_len && compressed.len() <= usize::from(u16::MAX);
        let payload = if use_compressed { &compressed } else { &raw };
        let mut wire = Vec::with_capacity(4 + payload.len());
        wire.extend_from_slice(&(raw_len as u16).to_le_bytes());
        wire.extend_from_slice(&(if use_compressed { compressed.len() as u16 } else { 0 }).to_le_bytes());
        wire.extend_from_slice(payload);
        wire
    }
}
