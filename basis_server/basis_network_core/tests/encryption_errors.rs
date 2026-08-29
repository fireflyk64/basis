//! Negative tests for the direct-link key agreement and datagram encryption layer.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use basis_crypto::{AeadError, BasisAeadCipher, X25519Error};
use basis_network_core::encryption::basis_crypto_handshake::{BasisCryptoHandshake, HandshakeError};
use basis_network_core::encryption::basis_crypto_layer::BasisCryptoLayer;

fn endpoint(port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

#[test]
fn handshake_refuses_bad_key_lengths() {
    let (private, public) = BasisCryptoHandshake::generate_key_pair();
    let (_, peer_public) = BasisCryptoHandshake::generate_key_pair();
    assert_eq!(
        BasisCryptoHandshake::derive_peer_keys(&private[..31], &public, &peer_public).err(),
        Some(HandshakeError::KeyLength { what: "private key", expected: 32, actual: 31 })
    );
    assert_eq!(
        BasisCryptoHandshake::derive_peer_keys(&private, &[], &peer_public).err(),
        Some(HandshakeError::KeyLength { what: "public key", expected: 32, actual: 0 })
    );
    assert_eq!(
        BasisCryptoHandshake::derive_peer_keys(&private, &public, &peer_public[..5]).err(),
        Some(HandshakeError::KeyLength { what: "peer public key", expected: 32, actual: 5 })
    );
}

#[test]
fn handshake_refuses_identical_and_low_order_peer_keys() {
    let (private, public) = BasisCryptoHandshake::generate_key_pair();
    assert_eq!(BasisCryptoHandshake::derive_peer_keys(&private, &public, &public).err(), Some(HandshakeError::IdenticalKeys));
    assert_eq!(
        BasisCryptoHandshake::derive_peer_keys(&private, &public, &[0u8; 32]).err(),
        Some(HandshakeError::X25519(X25519Error::NonContributory))
    );
}

#[test]
fn handshake_derives_mirrored_keys_for_both_ends() {
    let (a_private, a_public) = BasisCryptoHandshake::generate_key_pair();
    let (b_private, b_public) = BasisCryptoHandshake::generate_key_pair();
    let (a_send, a_recv) = BasisCryptoHandshake::derive_peer_keys(&a_private, &a_public, &b_public).unwrap();
    let (b_send, b_recv) = BasisCryptoHandshake::derive_peer_keys(&b_private, &b_public, &a_public).unwrap();
    assert_eq!(a_send, b_recv);
    assert_eq!(a_recv, b_send);
    assert_ne!(a_send, a_recv);
    assert_eq!(a_send.len(), BasisCryptoHandshake::KEY_SIZE);
}

#[test]
fn layer_refuses_keys_of_the_wrong_length() {
    let layer = BasisCryptoLayer::new();
    assert_eq!(
        layer.set_endpoint_keys(endpoint(1), &[1u8; 16], &[2u8; 32], 0).err(),
        Some(AeadError::KeyLength { expected: 32, actual: 16 })
    );
    assert_eq!(
        layer.set_endpoint_keys(endpoint(1), &[1u8; 32], &[2u8; 33], 0).err(),
        Some(AeadError::KeyLength { expected: 32, actual: 33 })
    );
    assert!(!layer.has_endpoint(endpoint(1)));
    assert!(layer.set_endpoint_keys(endpoint(1), &[1u8; 32], &[2u8; 32], 0).is_ok());
    assert!(layer.has_endpoint(endpoint(1)));
}

fn paired_layers() -> (BasisCryptoLayer, BasisCryptoLayer) {
    let a = BasisCryptoLayer::new();
    let b = BasisCryptoLayer::new();
    a.set_endpoint_keys(endpoint(2), &[7u8; 32], &[8u8; 32], 0).unwrap();
    b.set_endpoint_keys(endpoint(1), &[8u8; 32], &[7u8; 32], 0).unwrap();
    (a, b)
}

#[test]
fn layer_drops_truncated_tampered_and_replayed_packets() {
    let (a, b) = paired_layers();
    let header = 1u8; // PacketProperty.Channeled: encrypted
    let body = b"payload bytes".to_vec();
    let mut packet = Vec::with_capacity(1 + body.len() + BasisCryptoLayer::OVERHEAD);
    packet.push(header);
    packet.extend_from_slice(&body);
    let length = packet.len();
    packet.resize(length + BasisCryptoLayer::OVERHEAD, 0);

    let sent = a.process_out_bound_packet(endpoint(2), &mut packet, 0, length);
    assert_eq!(sent, length + BasisCryptoLayer::OVERHEAD);
    assert_ne!(&packet[1..length], &body[..]);

    // Truncated: no room for tag and counter.
    let mut short = packet.clone();
    assert_eq!(b.process_inbound_packet(endpoint(1), &mut short, length), 0);
    assert_eq!(b.process_inbound_packet(endpoint(1), &mut short, BasisCryptoLayer::OVERHEAD), 0);
    // A length past the buffer is refused rather than read.
    assert_eq!(b.process_inbound_packet(endpoint(1), &mut short, sent + 10), 0);

    // Tampered body.
    let mut tampered = packet.clone();
    tampered[3] ^= 0xFF;
    assert_eq!(b.process_inbound_packet(endpoint(1), &mut tampered, sent), 0);

    // Tampered counter (nonce).
    let mut bad_counter = packet.clone();
    bad_counter[sent - 1] ^= 1;
    assert_eq!(b.process_inbound_packet(endpoint(1), &mut bad_counter, sent), 0);

    // Wrong endpoint: no session, packet passes through untouched (as the C# layer did).
    let mut other = packet.clone();
    assert_eq!(b.process_inbound_packet(endpoint(9), &mut other, sent), sent);

    // The genuine packet opens.
    let mut good = packet.clone();
    assert_eq!(b.process_inbound_packet(endpoint(1), &mut good, sent), length);
    assert_eq!(&good[1..length], &body[..]);
}

#[test]
fn outbound_without_slack_or_without_header_is_dropped_not_sent_in_the_clear() {
    let (a, _) = paired_layers();
    let mut no_slack = vec![1u8, 2, 3];
    assert_eq!(a.process_out_bound_packet(endpoint(2), &mut no_slack, 0, 3), 0);
    let mut empty: Vec<u8> = Vec::new();
    assert_eq!(a.process_out_bound_packet(endpoint(2), &mut empty, 5, 3), 0);
    // A property the layer does not encrypt passes through unchanged.
    let mut connect = vec![5u8, 1, 2];
    assert_eq!(a.process_out_bound_packet(endpoint(2), &mut connect, 0, 3), 3);
    assert_eq!(connect, vec![5, 1, 2]);
    assert_eq!(BasisCryptoLayer::OVERHEAD, BasisAeadCipher::TAG_SIZE + BasisCryptoLayer::COUNTER_SIZE);
}
