//! Transport-encryption tests: the X25519 + HKDF handshake and the per-endpoint
//! ChaCha20-Poly1305 packet layer. Covers full two-sided key agreement, encrypt/decrypt round
//! trips across payload sizes, wire-format layout, tampering/truncation/replay behaviour,
//! wrong-key and malformed-handshake rejection, and session (endpoint) lifecycle.

use std::net::SocketAddr;

use basis_crypto::{BasisAeadCipher, BasisHkdf, BasisX25519};
use basis_network_core::encryption::{BasisCryptoHandshake, BasisCryptoLayer};
use basis_server_tests::support::delta_test_support::TestRng;

fn client_address() -> SocketAddr {
    "192.0.2.10:41000".parse().expect("addr")
}

fn server_address() -> SocketAddr {
    "192.0.2.20:42000".parse().expect("addr")
}

// Low five header bits mirror the transport's packet properties.
const HEADER_UNRELIABLE: u8 = 0x00;
const HEADER_CHANNELED: u8 = 0x01;
const HEADER_MERGED: u8 = 0x0C;
const HEADER_COMPACT_MERGED: u8 = 0x12;

fn sequential_key(seed: u8) -> Vec<u8> {
    (0..BasisCryptoHandshake::KEY_SIZE).map(|i| seed.wrapping_add(i as u8)).collect()
}

fn build_packet(header: u8, payload_size: usize) -> Vec<u8> {
    let mut packet = vec![0u8; 1 + payload_size];
    packet[0] = header;
    for i in 0..payload_size {
        packet[1 + i] = (7 + i * 31) as u8;
    }
    packet
}

/// Runs a packet through the outbound path exactly as the transport would: the buffer has
/// `OVERHEAD` spare bytes after the packet.
fn seal_at(layer: &BasisCryptoLayer, remote: SocketAddr, packet: &[u8], offset: usize) -> Vec<u8> {
    let mut buffer = vec![0u8; offset + packet.len() + BasisCryptoLayer::OVERHEAD];
    buffer[offset..offset + packet.len()].copy_from_slice(packet);
    let length = layer.process_out_bound_packet(remote, &mut buffer, offset, packet.len());
    buffer[offset..offset + length].to_vec()
}

fn seal(layer: &BasisCryptoLayer, remote: SocketAddr, packet: &[u8]) -> Vec<u8> {
    seal_at(layer, remote, packet, 0)
}

/// Runs a received datagram through the inbound path; returns the mutated buffer and resulting
/// length (0 means the layer dropped the packet).
fn open_raw(layer: &BasisCryptoLayer, remote: SocketAddr, wire: &[u8]) -> (Vec<u8>, usize) {
    let mut buffer = wire.to_vec();
    let length = layer.process_inbound_packet(remote, &mut buffer, wire.len());
    (buffer, length)
}

fn open(layer: &BasisCryptoLayer, remote: SocketAddr, wire: &[u8]) -> Option<Vec<u8>> {
    let (buffer, length) = open_raw(layer, remote, wire);
    if length == 0 { None } else { Some(buffer[..length].to_vec()) }
}

/// Little-endian nonce counter carried in the last 8 bytes of an encrypted datagram.
fn read_counter(wire: &[u8]) -> i64 {
    let mut value = 0u64;
    for i in 0..BasisCryptoLayer::COUNTER_SIZE {
        value |= u64::from(wire[wire.len() - BasisCryptoLayer::COUNTER_SIZE + i]) << (8 * i);
    }
    value as i64
}

/// Mirror of the handshake's public-key ordering (unsigned lexicographic, length tiebreak).
fn compare_keys(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    a.cmp(b)
}

fn new_fixed_key_pair(initial_send_counter: i64) -> (BasisCryptoLayer, BasisCryptoLayer) {
    let key_ab = sequential_key(0xA0);
    let key_ba = sequential_key(0x0B);
    let client = BasisCryptoLayer::new();
    let server = BasisCryptoLayer::new();
    client.set_endpoint_keys(server_address(), &key_ab, &key_ba, initial_send_counter).expect("client keys");
    server.set_endpoint_keys(client_address(), &key_ba, &key_ab, initial_send_counter).expect("server keys");
    (client, server)
}

fn new_handshake_pair() -> (BasisCryptoLayer, BasisCryptoLayer) {
    let (client_private, client_public) = BasisCryptoHandshake::generate_key_pair();
    let (server_private, server_public) = BasisCryptoHandshake::generate_key_pair();
    let (client_send, client_recv) = BasisCryptoHandshake::derive_peer_keys(&client_private, &client_public, &server_public).expect("client derive");
    let (server_send, server_recv) = BasisCryptoHandshake::derive_peer_keys(&server_private, &server_public, &client_public).expect("server derive");
    let client = BasisCryptoLayer::new();
    let server = BasisCryptoLayer::new();
    client.set_endpoint_keys(server_address(), &client_send, &client_recv, 0).expect("client keys");
    server.set_endpoint_keys(client_address(), &server_send, &server_recv, 0).expect("server keys");
    (client, server)
}

// ------------------------------------------------------------- handshake

#[test]
fn generate_key_pair_produces_distinct_well_formed_pairs() {
    let (private_key, public_key) = BasisCryptoHandshake::generate_key_pair();
    assert_eq!(private_key.len(), BasisCryptoHandshake::PRIVATE_KEY_SIZE);
    assert_eq!(public_key.len(), BasisCryptoHandshake::PUBLIC_KEY_SIZE);
    assert_eq!(BasisX25519::derive_public_key(&private_key).expect("derive"), public_key);

    let (second_private, second_public) = BasisCryptoHandshake::generate_key_pair();
    assert_ne!(private_key, second_private);
    assert_ne!(public_key, second_public);
}

#[test]
fn derive_peer_keys_both_sides_derive_complementary_directional_keys() {
    let (client_private, client_public) = BasisCryptoHandshake::generate_key_pair();
    let (server_private, server_public) = BasisCryptoHandshake::generate_key_pair();

    let (client_send, client_recv) = BasisCryptoHandshake::derive_peer_keys(&client_private, &client_public, &server_public).expect("client");
    let (server_send, server_recv) = BasisCryptoHandshake::derive_peer_keys(&server_private, &server_public, &client_public).expect("server");

    assert_eq!(client_send.len(), BasisCryptoHandshake::KEY_SIZE);
    assert_eq!(client_recv.len(), BasisCryptoHandshake::KEY_SIZE);
    // Each side's send key is the other side's receive key.
    assert_eq!(client_send, server_recv);
    assert_eq!(client_recv, server_send);
    // Directions use independent keys.
    assert_ne!(client_send, client_recv);
}

#[test]
fn derive_peer_keys_is_deterministic() {
    let (private_key, public_key) = BasisCryptoHandshake::generate_key_pair();
    let (_, peer_public) = BasisCryptoHandshake::generate_key_pair();
    let first = BasisCryptoHandshake::derive_peer_keys(&private_key, &public_key, &peer_public).expect("first");
    let second = BasisCryptoHandshake::derive_peer_keys(&private_key, &public_key, &peer_public).expect("second");
    assert_eq!(first, second);
}

#[test]
fn derive_peer_keys_matches_documented_hkdf_construction() {
    let (private_a, public_a) = BasisCryptoHandshake::generate_key_pair();
    let (private_b, public_b) = BasisCryptoHandshake::generate_key_pair();

    // Recompute the spec by hand: ECDH secret, transcript salt = lowPub || highPub, HKDF-SHA256
    // with the two directional info strings; the lower public key is "A".
    let shared = BasisX25519::agree(&private_a, &public_b).expect("agree");
    let a_is_low = compare_keys(&public_a, &public_b) == std::cmp::Ordering::Less;
    let (low_public, high_public) = if a_is_low { (&public_a, &public_b) } else { (&public_b, &public_a) };
    let mut salt = low_public.clone();
    salt.extend_from_slice(high_public);
    let key_low_to_high = BasisHkdf::derive_key(&shared, &salt, b"basis-crypto-v1-ab", BasisCryptoHandshake::KEY_SIZE).expect("hkdf");
    let key_high_to_low = BasisHkdf::derive_key(&shared, &salt, b"basis-crypto-v1-ba", BasisCryptoHandshake::KEY_SIZE).expect("hkdf");

    let (send_a, recv_a) = BasisCryptoHandshake::derive_peer_keys(&private_a, &public_a, &public_b).expect("a");
    let (send_b, recv_b) = BasisCryptoHandshake::derive_peer_keys(&private_b, &public_b, &public_a).expect("b");

    assert_eq!(send_a, if a_is_low { key_low_to_high.clone() } else { key_high_to_low.clone() });
    assert_eq!(recv_a, if a_is_low { key_high_to_low.clone() } else { key_low_to_high.clone() });
    assert_eq!(send_b, if a_is_low { key_high_to_low } else { key_low_to_high.clone() });
    assert_eq!(recv_b, if a_is_low { key_low_to_high } else { send_a.clone() });
}

#[test]
fn derive_peer_keys_identical_public_keys_is_an_error() {
    let (private_key, public_key) = BasisCryptoHandshake::generate_key_pair();
    assert!(BasisCryptoHandshake::derive_peer_keys(&private_key, &public_key, &public_key).is_err());
}

#[test]
fn derive_peer_keys_all_zero_peer_public_is_an_error() {
    // The all-zero point yields an all-zero X25519 shared secret, which the agreement rejects; the
    // handshake must surface that as a clean error.
    let (private_key, public_key) = BasisCryptoHandshake::generate_key_pair();
    let zero_public = vec![0u8; BasisCryptoHandshake::PUBLIC_KEY_SIZE];
    assert!(BasisCryptoHandshake::derive_peer_keys(&private_key, &public_key, &zero_public).is_err());
}

#[test]
fn derive_peer_keys_undersized_peer_public_is_an_error() {
    let (private_key, public_key) = BasisCryptoHandshake::generate_key_pair();
    for size in [0usize, 1, 16, 31] {
        let malformed: Vec<u8> = (0..size).map(|i| (i + 1) as u8).collect();
        assert!(BasisCryptoHandshake::derive_peer_keys(&private_key, &public_key, &malformed).is_err(), "size {size}");
    }
}

#[test]
fn derive_peer_keys_oversized_peer_public_does_not_panic() {
    let (private_key, public_key) = BasisCryptoHandshake::generate_key_pair();
    for size in [33usize, 64] {
        let oversized: Vec<u8> = (0..size).map(|i| (0x40 + i) as u8).collect();
        // Success or failure is acceptable for garbage; a panic is not.
        if let Ok((send, recv)) = BasisCryptoHandshake::derive_peer_keys(&private_key, &public_key, &oversized) {
            assert_eq!(send.len(), BasisCryptoHandshake::KEY_SIZE);
            assert_eq!(recv.len(), BasisCryptoHandshake::KEY_SIZE);
        }
    }
}

#[test]
fn derive_peer_keys_undersized_private_key_is_an_error() {
    let (full_private, public_key) = BasisCryptoHandshake::generate_key_pair();
    let (_, peer_public) = BasisCryptoHandshake::generate_key_pair();
    assert!(BasisCryptoHandshake::derive_peer_keys(&full_private[..16], &public_key, &peer_public).is_err());
}

#[test]
fn derive_peer_keys_different_peers_produce_different_keys() {
    let (private_a, public_a) = BasisCryptoHandshake::generate_key_pair();
    let (_, public_b) = BasisCryptoHandshake::generate_key_pair();
    let (_, public_c) = BasisCryptoHandshake::generate_key_pair();
    let (send_ab, recv_ab) = BasisCryptoHandshake::derive_peer_keys(&private_a, &public_a, &public_b).expect("ab");
    let (send_ac, recv_ac) = BasisCryptoHandshake::derive_peer_keys(&private_a, &public_a, &public_c).expect("ac");
    assert_ne!(send_ab, send_ac);
    assert_ne!(recv_ab, recv_ac);
}

// ------------------------------------------------- layer: round-trip & format

#[test]
fn round_trip_recovers_exact_packet_across_payload_sizes() {
    for payload_size in [0usize, 1, 2, 15, 16, 32, 512, 1200, 16384] {
        let (client, server) = new_fixed_key_pair(0);
        let packet = build_packet(HEADER_UNRELIABLE, payload_size);
        let wire = seal(&client, server_address(), &packet);
        assert_eq!(wire.len(), packet.len() + BasisCryptoLayer::OVERHEAD, "size {payload_size}");
        assert_eq!(open(&server, client_address(), &wire), Some(packet), "size {payload_size}");
    }
}

#[test]
fn full_handshake_encrypted_traffic_flows_both_directions() {
    let (client, server) = new_handshake_pair();
    for payload_size in [0usize, 3, 200, 1200] {
        let request = build_packet(HEADER_CHANNELED, payload_size);
        assert_eq!(open(&server, client_address(), &seal(&client, server_address(), &request)), Some(request));
        let response = build_packet(HEADER_MERGED, payload_size);
        assert_eq!(open(&client, server_address(), &seal(&server, client_address(), &response)), Some(response));
    }
}

#[test]
fn outbound_adds_exact_overhead_keeps_header_cleartext_hides_payload() {
    assert_eq!(BasisCryptoLayer::OVERHEAD, BasisAeadCipher::TAG_SIZE + BasisCryptoLayer::COUNTER_SIZE);
    assert_eq!(BasisCryptoLayer::new().extra_packet_size_for_layer(), BasisCryptoLayer::OVERHEAD);

    let (client, server) = new_fixed_key_pair(0);
    let packet = build_packet(HEADER_CHANNELED, 64);
    let wire = seal(&client, server_address(), &packet);

    assert_eq!(wire.len(), packet.len() + BasisCryptoLayer::OVERHEAD);
    assert_eq!(wire[0], HEADER_CHANNELED);
    assert_ne!(&wire[1..65], &packet[1..]);
    assert_eq!(read_counter(&wire), 1);
    assert_eq!(open(&server, client_address(), &wire), Some(packet));
}

#[test]
fn outbound_trailer_is_little_endian_send_counter() {
    const INITIAL_COUNTER: i64 = 0x0102030405060708;
    let client = BasisCryptoLayer::new();
    client.set_endpoint_keys(server_address(), &sequential_key(0x2C), &sequential_key(0x9D), INITIAL_COUNTER).expect("keys");

    let packet = build_packet(HEADER_UNRELIABLE, 4);
    let wire1 = seal(&client, server_address(), &packet);
    let trailer = &wire1[wire1.len() - BasisCryptoLayer::COUNTER_SIZE..];
    assert_eq!(trailer, &[0x09, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
    assert_eq!(read_counter(&wire1), INITIAL_COUNTER + 1);

    let wire2 = seal(&client, server_address(), &packet);
    assert_eq!(read_counter(&wire2), INITIAL_COUNTER + 2);
}

#[test]
fn outbound_same_plaintext_produces_different_ciphertexts() {
    let (client, server) = new_fixed_key_pair(0);
    let packet = build_packet(HEADER_UNRELIABLE, 32);
    let wire1 = seal(&client, server_address(), &packet);
    let wire2 = seal(&client, server_address(), &packet);
    assert_ne!(&wire1[1..33], &wire2[1..33]);
    assert_eq!(open(&server, client_address(), &wire1), Some(packet.clone()));
    assert_eq!(open(&server, client_address(), &wire2), Some(packet));
}

#[test]
fn outbound_is_deterministic_for_same_keys_and_counter() {
    let key_ab = sequential_key(0x3A);
    let key_ba = sequential_key(0xD4);
    let first = BasisCryptoLayer::new();
    let second = BasisCryptoLayer::new();
    first.set_endpoint_keys(server_address(), &key_ab, &key_ba, 0).expect("keys");
    second.set_endpoint_keys(server_address(), &key_ab, &key_ba, 0).expect("keys");

    let packet = build_packet(HEADER_MERGED, 48);
    assert_eq!(seal(&first, server_address(), &packet), seal(&second, server_address(), &packet));

    let offset_counter = BasisCryptoLayer::new();
    offset_counter.set_endpoint_keys(server_address(), &key_ab, &key_ba, 5).expect("keys");
    assert_ne!(seal(&second, server_address(), &packet), seal(&offset_counter, server_address(), &packet));
}

#[test]
fn outbound_respects_non_zero_offset() {
    let (client, server) = new_fixed_key_pair(0);
    let packet = build_packet(HEADER_UNRELIABLE, 24);
    const OFFSET: usize = 5;
    let mut buffer = vec![0u8; OFFSET + packet.len() + BasisCryptoLayer::OVERHEAD];
    buffer[..OFFSET].fill(0xAA);
    buffer[OFFSET..OFFSET + packet.len()].copy_from_slice(&packet);

    let length = client.process_out_bound_packet(server_address(), &mut buffer, OFFSET, packet.len());

    assert_eq!(length, packet.len() + BasisCryptoLayer::OVERHEAD);
    assert!(buffer[..OFFSET].iter().all(|b| *b == 0xAA));
    let wire = buffer[OFFSET..OFFSET + length].to_vec();
    assert_eq!(open(&server, client_address(), &wire), Some(packet));
}

#[test]
fn wire_format_layer_output_opens_with_raw_aead_cipher() {
    let key_ab = sequential_key(0x21);
    let client = BasisCryptoLayer::new();
    client.set_endpoint_keys(server_address(), &key_ab, &sequential_key(0x91), 0).expect("keys");

    let packet = build_packet(HEADER_MERGED, 32);
    let wire = seal(&client, server_address(), &packet);

    // Documented layout: [header][ciphertext][16B tag][8B LE counter]; nonce = counter bytes
    // zero-padded to 12; AAD = header byte.
    let mut nonce = vec![0u8; BasisAeadCipher::NONCE_SIZE];
    nonce[..BasisCryptoLayer::COUNTER_SIZE].copy_from_slice(&wire[wire.len() - BasisCryptoLayer::COUNTER_SIZE..]);
    let mut body = wire[1..33].to_vec();
    let tag = &wire[33..33 + BasisAeadCipher::TAG_SIZE];

    let cipher = BasisAeadCipher::new(&key_ab).expect("cipher");
    cipher.open(&nonce, wire[0], &mut body, tag).expect("open");
    assert_eq!(body, packet[1..].to_vec());
}

#[test]
fn wire_format_raw_aead_constructed_datagram_accepted_by_inbound() {
    let recv_key = sequential_key(0x55);
    let server = BasisCryptoLayer::new();
    server.set_endpoint_keys(client_address(), &sequential_key(0x66), &recv_key, 0).expect("keys");

    let packet = build_packet(HEADER_CHANNELED, 16);
    const COUNTER: u8 = 7;
    let mut wire = vec![0u8; packet.len() + BasisCryptoLayer::OVERHEAD];
    wire[..packet.len()].copy_from_slice(&packet);
    let mut nonce = vec![0u8; BasisAeadCipher::NONCE_SIZE];
    nonce[0] = COUNTER;
    {
        let cipher = BasisAeadCipher::new(&recv_key).expect("cipher");
        let (head, tail) = wire.split_at_mut(packet.len());
        cipher.seal(&nonce, packet[0], &mut head[1..], &mut tail[..BasisAeadCipher::TAG_SIZE]).expect("seal");
    }
    wire[packet.len() + BasisAeadCipher::TAG_SIZE] = COUNTER;

    assert_eq!(open(&server, client_address(), &wire), Some(packet));
}

// ------------------------------------------------------ tampering & replay

#[test]
fn inbound_any_flipped_byte_is_dropped_without_plaintext_leak() {
    let (client, server) = new_fixed_key_pair(0);
    let packet = build_packet(HEADER_UNRELIABLE, 8);
    let wire = seal(&client, server_address(), &packet);
    assert_eq!(wire.len(), packet.len() + BasisCryptoLayer::OVERHEAD);

    // Exhaustive over ciphertext, tag and counter positions (header handled separately).
    for position in 1..wire.len() {
        let mut tampered = wire.clone();
        tampered[position] ^= 0x01;
        let (buffer, length) = open_raw(&server, client_address(), &tampered);
        assert_eq!(length, 0, "tampered byte at {position} was not rejected");
        assert_ne!(&buffer[1..9], &packet[1..], "plaintext leaked for tamper at {position}");
    }

    // Failed decrypts must not poison the session for the genuine datagram.
    assert_eq!(open(&server, client_address(), &wire), Some(packet));
}

#[test]
fn inbound_header_bit_flip_same_property_is_dropped() {
    let (client, server) = new_fixed_key_pair(0);
    let mut wire = seal(&client, server_address(), &build_packet(HEADER_UNRELIABLE, 16));
    // High header bits are outside the property mask, but the whole header byte is
    // authenticated as AAD, so the flip must break the tag.
    wire[0] ^= 0x80;
    let (_, length) = open_raw(&server, client_address(), &wire);
    assert_eq!(length, 0);
}

#[test]
fn inbound_header_morphed_to_non_encryptable_bypasses_decryption() {
    let (client, server) = new_fixed_key_pair(0);
    let packet = build_packet(HEADER_UNRELIABLE, 16);
    let mut wire = seal(&client, server_address(), &packet);
    // Property 0x02 is not an encryptable property, so the layer passes the datagram through
    // untouched (still ciphertext) for the transport to vet.
    wire[0] ^= 0x02;
    let (buffer, length) = open_raw(&server, client_address(), &wire);
    assert_eq!(length, wire.len());
    assert_eq!(buffer, wire);
    assert_ne!(&buffer[1..17], &packet[1..]);
}

#[test]
fn inbound_replayed_datagram_is_accepted_again_no_replay_window() {
    let (client, server) = new_fixed_key_pair(0);
    let packet = build_packet(HEADER_CHANNELED, 64);
    let wire = seal(&client, server_address(), &packet);
    // Pins current behaviour: the layer carries the nonce in the datagram and keeps no inbound
    // sequence state, so byte-identical replays decrypt again.
    assert_eq!(open(&server, client_address(), &wire), Some(packet.clone()));
    assert_eq!(open(&server, client_address(), &wire), Some(packet));
}

#[test]
fn inbound_out_of_order_delivery_succeeds() {
    let (client, server) = new_fixed_key_pair(0);
    let first = build_packet(HEADER_UNRELIABLE, 10);
    let second = build_packet(HEADER_UNRELIABLE, 20);
    let wire1 = seal(&client, server_address(), &first);
    let wire2 = seal(&client, server_address(), &second);
    assert_eq!(open(&server, client_address(), &wire2), Some(second));
    assert_eq!(open(&server, client_address(), &wire1), Some(first));
}

#[test]
fn inbound_wrong_keys_dropped_without_plaintext_leak() {
    let (client, _) = new_fixed_key_pair(0);
    let stranger = BasisCryptoLayer::new();
    stranger.set_endpoint_keys(client_address(), &sequential_key(0x5A), &sequential_key(0xC3), 0).expect("keys");

    let packet = build_packet(HEADER_UNRELIABLE, 64);
    let wire = seal(&client, server_address(), &packet);
    let (buffer, length) = open_raw(&stranger, client_address(), &wire);
    assert_eq!(length, 0);
    assert_ne!(&buffer[1..65], &packet[1..]);
}

#[test]
fn inbound_swapped_directional_keys_dropped() {
    let key_ab = sequential_key(0x60);
    let key_ba = sequential_key(0xE7);
    let client = BasisCryptoLayer::new();
    client.set_endpoint_keys(server_address(), &key_ab, &key_ba, 0).expect("keys");
    // Misconfigured peer installs the same orientation instead of the mirror.
    let mirrored = BasisCryptoLayer::new();
    mirrored.set_endpoint_keys(client_address(), &key_ab, &key_ba, 0).expect("keys");

    let wire = seal(&client, server_address(), &build_packet(HEADER_CHANNELED, 32));
    let (_, length) = open_raw(&mirrored, client_address(), &wire);
    assert_eq!(length, 0);
}

#[test]
fn inbound_truncated_datagram_is_dropped() {
    let (client, server) = new_fixed_key_pair(0);
    let wire = seal(&client, server_address(), &build_packet(HEADER_UNRELIABLE, 32));
    for truncated_length in [1usize, 2, 10, 24, 25, 40, 56] {
        assert!(truncated_length < wire.len());
        let (_, length) = open_raw(&server, client_address(), &wire[..truncated_length]);
        assert_eq!(length, 0, "truncated to {truncated_length}");
    }
}

#[test]
fn inbound_garbage_datagram_is_dropped() {
    let (_, server) = new_fixed_key_pair(0);

    let mut junk = TestRng::new(1234).bytes(100);
    junk[0] = HEADER_UNRELIABLE;
    let (_, length) = open_raw(&server, client_address(), &junk);
    assert_eq!(length, 0);

    // Minimum length that reaches the AEAD: zero payload, garbage tag/counter.
    let mut minimal = TestRng::new(5678).bytes(1 + BasisCryptoLayer::OVERHEAD);
    minimal[0] = HEADER_CHANNELED;
    let (_, length) = open_raw(&server, client_address(), &minimal);
    assert_eq!(length, 0);
}

#[test]
fn inbound_uses_length_not_buffer_size() {
    // The transport hands the layer a reused oversized receive buffer.
    let (client, server) = new_fixed_key_pair(0);
    let packet = build_packet(HEADER_UNRELIABLE, 32);
    let wire = seal(&client, server_address(), &packet);

    let mut oversized = vec![0u8; 2048];
    oversized[..wire.len()].copy_from_slice(&wire);
    let length = server.process_inbound_packet(client_address(), &mut oversized, wire.len());
    assert_eq!(length, packet.len());
    assert_eq!(&oversized[..length], &packet[..]);
}

// ------------------------------------------------- sessions & endpoints

#[test]
fn no_session_packets_pass_through_unmodified() {
    let layer = BasisCryptoLayer::new();
    let packet = build_packet(HEADER_UNRELIABLE, 32);
    assert_eq!(seal(&layer, server_address(), &packet), packet);
    let (buffer, length) = open_raw(&layer, client_address(), &packet);
    assert_eq!(length, packet.len());
    assert_eq!(buffer, packet);
}

#[test]
fn non_encryptable_properties_bypass_encryption_even_with_session() {
    for header in [0x02u8, 0x03, 0x05, 0x1F] {
        let (client, server) = new_fixed_key_pair(0);
        let packet = build_packet(header, 32);
        assert_eq!(seal(&client, server_address(), &packet), packet, "header {header:#x}");
        let (buffer, length) = open_raw(&server, client_address(), &packet);
        assert_eq!(length, packet.len());
        assert_eq!(buffer, packet);
    }
}

#[test]
fn encryptable_properties_are_encrypted_including_masked_header_bits() {
    for header in [HEADER_UNRELIABLE, HEADER_CHANNELED, HEADER_MERGED, HEADER_COMPACT_MERGED, 0xE1, 0x8C] {
        let (client, server) = new_fixed_key_pair(0);
        let packet = build_packet(header, 40);
        let wire = seal(&client, server_address(), &packet);
        assert_eq!(wire.len(), packet.len() + BasisCryptoLayer::OVERHEAD, "header {header:#x}");
        assert_eq!(wire[0], header);
        assert_ne!(&wire[1..41], &packet[1..]);
        assert_eq!(open(&server, client_address(), &wire), Some(packet));
    }
}

#[test]
fn endpoints_match_by_address_and_port_not_by_instance() {
    let key_ab = sequential_key(0x12);
    let key_ba = sequential_key(0x77);
    let client = BasisCryptoLayer::new();
    let server = BasisCryptoLayer::new();
    let loopback_7777: SocketAddr = "127.0.0.1:7777".parse().expect("addr");
    let loopback_7778: SocketAddr = "127.0.0.1:7778".parse().expect("addr");
    client.set_endpoint_keys(loopback_7777, &key_ab, &key_ba, 0).expect("keys");
    server.set_endpoint_keys(loopback_7777, &key_ba, &key_ab, 0).expect("keys");

    assert!(client.has_endpoint("127.0.0.1:7777".parse().expect("addr")));
    assert!(!client.has_endpoint(loopback_7778));

    let packet = build_packet(HEADER_UNRELIABLE, 16);
    let wire = seal(&client, loopback_7777, &packet);
    assert_eq!(wire.len(), packet.len() + BasisCryptoLayer::OVERHEAD);
    assert_eq!(open(&server, loopback_7777, &wire), Some(packet.clone()));

    // A different port is a different session: passthrough.
    assert_eq!(seal(&client, loopback_7778, &packet), packet);
}

#[test]
fn remove_endpoint_reverts_to_passthrough_and_keyed_peer_drops_cleartext() {
    let (client, server) = new_fixed_key_pair(0);
    let packet = build_packet(HEADER_UNRELIABLE, 32);
    assert_eq!(open(&server, client_address(), &seal(&client, server_address(), &packet)), Some(packet.clone()));

    client.remove_endpoint(server_address());
    assert!(!client.has_endpoint(server_address()));
    assert_eq!(client.session_count(), 0);

    let cleartext = seal(&client, server_address(), &packet);
    assert_eq!(cleartext, packet);

    // The still-keyed side refuses unauthenticated traffic instead of parsing it.
    let (_, length) = open_raw(&server, client_address(), &cleartext);
    assert_eq!(length, 0);
}

#[test]
fn remap_endpoint_moves_session_and_keeps_counter() {
    let (client, server) = new_fixed_key_pair(0);
    let packet = build_packet(HEADER_UNRELIABLE, 16);
    let wire1 = seal(&client, server_address(), &packet);
    assert_eq!(read_counter(&wire1), 1);

    let moved: SocketAddr = "198.51.100.5:45678".parse().expect("addr");
    client.remap_endpoint(server_address(), moved);
    assert!(!client.has_endpoint(server_address()));
    assert!(client.has_endpoint(moved));
    assert_eq!(client.session_count(), 1);

    let wire2 = seal(&client, moved, &packet);
    assert_eq!(read_counter(&wire2), 2);
    assert_eq!(open(&server, client_address(), &wire2), Some(packet.clone()));

    // The old endpoint no longer encrypts.
    assert_eq!(seal(&client, server_address(), &packet), packet);
}

#[test]
fn reinstall_same_keys_default_counter_reuses_nonces() {
    let key_ab = sequential_key(0x40);
    let key_ba = sequential_key(0x8E);
    let client = BasisCryptoLayer::new();
    client.set_endpoint_keys(server_address(), &key_ab, &key_ba, 0).expect("keys");
    let packet = build_packet(HEADER_UNRELIABLE, 32);

    let wire1 = seal(&client, server_address(), &packet);
    let wire2 = seal(&client, server_address(), &packet);
    assert_ne!(wire1, wire2);

    // Pins the documented hazard: reinstalling the same keys with the default initial counter
    // restarts the nonce sequence, reproducing wire1 exactly.
    client.set_endpoint_keys(server_address(), &key_ab, &key_ba, 0).expect("keys");
    let wire3 = seal(&client, server_address(), &packet);
    assert_eq!(wire1, wire3);

    // Passing a fresh initial counter is the documented mitigation.
    client.set_endpoint_keys(server_address(), &key_ab, &key_ba, 1000).expect("keys");
    let wire4 = seal(&client, server_address(), &packet);
    assert_eq!(read_counter(&wire4), 1001);
}

#[test]
fn session_count_tracks_install_replace_remove() {
    let layer = BasisCryptoLayer::new();
    assert_eq!(layer.session_count(), 0);

    layer.set_endpoint_keys(client_address(), &sequential_key(1), &sequential_key(2), 0).expect("keys");
    assert_eq!(layer.session_count(), 1);
    layer.set_endpoint_keys(server_address(), &sequential_key(3), &sequential_key(4), 0).expect("keys");
    assert_eq!(layer.session_count(), 2);
    layer.set_endpoint_keys(client_address(), &sequential_key(5), &sequential_key(6), 0).expect("keys");
    assert_eq!(layer.session_count(), 2);

    layer.remove_endpoint(client_address());
    assert_eq!(layer.session_count(), 1);
    layer.remove_endpoint(client_address());
    assert_eq!(layer.session_count(), 1);
    layer.remove_endpoint(server_address());
    assert_eq!(layer.session_count(), 0);
}

#[test]
fn set_endpoint_keys_rejects_wrong_sized_keys() {
    let layer = BasisCryptoLayer::new();
    assert!(layer.set_endpoint_keys(server_address(), &[0u8; 16], &sequential_key(0x01), 0).is_err());
    assert!(layer.set_endpoint_keys(server_address(), &sequential_key(0x01), &[0u8; 33], 0).is_err());
    assert!(layer.set_endpoint_keys(server_address(), &[], &sequential_key(0x01), 0).is_err());
    assert!(!layer.has_endpoint(server_address()));
    assert_eq!(layer.session_count(), 0);
}

#[test]
fn concurrent_seals_claim_unique_counters_all_decrypt() {
    let (client, server) = new_fixed_key_pair(0);
    const PACKET_COUNT: usize = 256;
    let template = build_packet(HEADER_UNRELIABLE, 48);

    let wires: Vec<Vec<u8>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let client = &client;
                let template = &template;
                scope.spawn(move || (0..PACKET_COUNT / 8).map(|_| seal(client, server_address(), template)).collect::<Vec<_>>())
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().expect("sealer")).collect()
    });

    let mut counters: Vec<i64> = wires.iter().map(|w| read_counter(w)).collect();
    counters.sort_unstable();
    let expected: Vec<i64> = (1..=PACKET_COUNT as i64).collect();
    assert_eq!(counters, expected);

    for wire in &wires {
        assert_eq!(open(&server, client_address(), wire), Some(template.clone()));
    }
}
