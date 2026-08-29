use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, Ordering};

use basis_crypto::BasisAeadCipher;
use dashmap::DashMap;

/// Per-endpoint AEAD encryption applied at the datagram boundary. Each connection has its own
/// pair of ChaCha20-Poly1305 keys (one per direction) established by an X25519 handshake; see
/// [`super::BasisCryptoHandshake`].
///
/// In the C# server this was a LiteNetLib `PacketLayerBase` wrapping every UDP datagram of a
/// direct link. Over iroh the link is already TLS-encrypted, so the layer is kept as the pure
/// codec — the wire format the C# clients still speak on their own direct links, and what the
/// upcoming LiteNetLib-protocol transport plugs back into its packet path.
///
/// Only the user-data-bearing packet properties are encrypted (Unreliable, Channeled, Merged,
/// CompactMerged). Connection setup, NAT, MTU and out-of-band probe packets stay cleartext so the
/// handshake itself never depends on a key being present.
///
/// Wire layout of an encrypted datagram:
///   [byte 0 : LiteNetLib header (cleartext, authenticated as AAD)]
///   [bytes 1..n : ciphertext]
///   [16 bytes : Poly1305 tag]
///   [8 bytes  : little-endian nonce counter]
pub struct BasisCryptoLayer {
    sessions: DashMap<SocketAddr, Session>,
}

struct Session {
    send: BasisAeadCipher,
    recv: BasisAeadCipher,
    send_counter: AtomicI64,
}

impl Default for BasisCryptoLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl BasisCryptoLayer {
    pub const COUNTER_SIZE: usize = 8;
    pub const OVERHEAD: usize = BasisAeadCipher::TAG_SIZE + Self::COUNTER_SIZE;

    const PROPERTY_MASK: u8 = 0x1F;
    // Mirrors LiteNetLib.PacketProperty values used for user-data-bearing datagrams.
    const PROP_UNRELIABLE: u8 = 0;
    const PROP_CHANNELED: u8 = 1;
    const PROP_MERGED: u8 = 12;
    const PROP_COMPACT_MERGED: u8 = 18;

    pub fn new() -> Self {
        Self { sessions: DashMap::new() }
    }

    /// Extra bytes this layer adds to every encrypted datagram (LiteNetLib's `ExtraPacketSizeForLayer`).
    pub fn extra_packet_size_for_layer(&self) -> usize {
        Self::OVERHEAD
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// `initial_send_counter`: first nonce counter to use. Pass a value strictly greater than any
    /// counter previously used with these keys when re-installing the same keys for a reconnect,
    /// so a (key, nonce) pair is never reused.
    pub fn set_endpoint_keys(&self, endpoint: SocketAddr, send_key: &[u8], recv_key: &[u8], initial_send_counter: i64) {
        let session = Session {
            send: BasisAeadCipher::new(send_key),
            recv: BasisAeadCipher::new(recv_key),
            send_counter: AtomicI64::new(initial_send_counter),
        };
        self.sessions.insert(endpoint, session);
    }

    pub fn has_endpoint(&self, endpoint: SocketAddr) -> bool {
        self.sessions.contains_key(&endpoint)
    }

    pub fn remove_endpoint(&self, endpoint: SocketAddr) {
        self.sessions.remove(&endpoint);
    }

    pub fn remap_endpoint(&self, old_endpoint: SocketAddr, new_endpoint: SocketAddr) {
        if let Some((_, session)) = self.sessions.remove(&old_endpoint) {
            self.sessions.insert(new_endpoint, session);
        }
    }

    /// Encrypts `data[offset..offset+length]` in place, appending the tag and counter. Returns the
    /// new length (`length + OVERHEAD`), or `length` unchanged when the packet is not encryptable
    /// or the endpoint has no session. `data` must have `OVERHEAD` bytes of slack past `length`.
    pub fn process_out_bound_packet(&self, end_point: SocketAddr, data: &mut [u8], offset: usize, length: usize) -> usize {
        if length < 1 {
            return length;
        }
        let header = data[offset];
        if !Self::is_encryptable(header & Self::PROPERTY_MASK) {
            return length;
        }
        let Some(session) = self.sessions.get(&end_point) else {
            return length;
        };

        let counter = session.send_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let nonce = Self::write_counter(counter);

        let tag_offset = offset + length;
        let (payload, rest) = data[offset + 1..].split_at_mut(length - 1);
        session.send.seal(&nonce, header, payload, &mut rest[..BasisAeadCipher::TAG_SIZE]);
        Self::write_counter_bytes(data, tag_offset + BasisAeadCipher::TAG_SIZE, counter);
        length + Self::OVERHEAD
    }

    /// Decrypts `data[..length]` in place. Returns the plaintext length, or 0 when the packet must
    /// be dropped (too short, or authentication failed). A packet the layer does not encrypt is
    /// returned untouched.
    pub fn process_inbound_packet(&self, end_point: SocketAddr, data: &mut [u8], length: usize) -> usize {
        if length < 1 {
            return length;
        }
        let header = data[0];
        if !Self::is_encryptable(header & Self::PROPERTY_MASK) {
            return length;
        }
        let Some(session) = self.sessions.get(&end_point) else {
            return length;
        };
        if length < 1 + Self::OVERHEAD {
            return 0;
        }

        let tag_offset = length - Self::OVERHEAD;
        let counter_offset = length - Self::COUNTER_SIZE;
        let counter = Self::read_counter_bytes(data, counter_offset);
        let nonce = Self::write_counter(counter);

        let payload_length = tag_offset - 1;
        let (payload, rest) = data[1..].split_at_mut(payload_length);
        if !session.recv.open(&nonce, header, payload, &rest[..BasisAeadCipher::TAG_SIZE]) {
            return 0;
        }
        length - Self::OVERHEAD
    }

    fn is_encryptable(property: u8) -> bool {
        property == Self::PROP_UNRELIABLE
            || property == Self::PROP_CHANNELED
            || property == Self::PROP_MERGED
            || property == Self::PROP_COMPACT_MERGED
    }

    fn write_counter(counter: i64) -> [u8; BasisAeadCipher::NONCE_SIZE] {
        let mut nonce = [0u8; BasisAeadCipher::NONCE_SIZE];
        nonce[..8].copy_from_slice(&(counter as u64).to_le_bytes());
        nonce
    }

    fn write_counter_bytes(buffer: &mut [u8], offset: usize, counter: i64) {
        buffer[offset..offset + 8].copy_from_slice(&(counter as u64).to_le_bytes());
    }

    fn read_counter_bytes(buffer: &[u8], offset: usize) -> i64 {
        u64::from_le_bytes(buffer[offset..offset + 8].try_into().unwrap()) as i64
    }
}
