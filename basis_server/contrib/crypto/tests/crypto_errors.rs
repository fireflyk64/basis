//! Negative tests: every malformed key, nonce or tag is refused with a typed error, and a
//! tampered ciphertext never opens.

use basis_crypto::{AeadError, BasisAeadCipher, BasisHkdf, BasisX25519, Ed25519, HkdfLengthError, Payload, PrivKey, PubKey, Signature, X25519Error};

#[test]
fn aead_refuses_a_key_of_the_wrong_length() {
    assert_eq!(BasisAeadCipher::new(&[0u8; 31]).err(), Some(AeadError::KeyLength { expected: 32, actual: 31 }));
    assert_eq!(BasisAeadCipher::new(&[]).err(), Some(AeadError::KeyLength { expected: 32, actual: 0 }));
    assert!(BasisAeadCipher::try_new(&[7u8; 32]).is_ok());
}

#[test]
fn aead_refuses_bad_nonce_and_tag_buffers() {
    let cipher = BasisAeadCipher::new(&[1u8; 32]).unwrap();
    let mut buffer = [0u8; 8];
    let mut tag = [0u8; 16];
    assert_eq!(cipher.seal(&[0u8; 11], 0, &mut buffer, &mut tag), Err(AeadError::NonceLength { expected: 12, actual: 11 }));
    assert_eq!(cipher.seal(&[0u8; 12], 0, &mut buffer, &mut tag[..15]), Err(AeadError::TagLength { expected: 16, actual: 15 }));
    assert_eq!(cipher.open(&[0u8; 13], 0, &mut buffer, &tag), Err(AeadError::NonceLength { expected: 12, actual: 13 }));
    assert_eq!(cipher.open(&[0u8; 12], 0, &mut buffer, &tag[..3]), Err(AeadError::TagLength { expected: 16, actual: 3 }));
}

#[test]
fn tampering_with_ciphertext_tag_aad_or_key_fails_authentication() {
    let key = [3u8; 32];
    let nonce = [5u8; 12];
    let cipher = BasisAeadCipher::new(&key).unwrap();
    let plain = b"hello basis".to_vec();

    let mut sealed = plain.clone();
    let mut tag = [0u8; 16];
    cipher.seal(&nonce, 0x11, &mut sealed, &mut tag).unwrap();
    assert_ne!(sealed, plain);

    let mut ok = sealed.clone();
    cipher.open(&nonce, 0x11, &mut ok, &tag).unwrap();
    assert_eq!(ok, plain);

    let mut flipped = sealed.clone();
    flipped[0] ^= 1;
    assert_eq!(cipher.open(&nonce, 0x11, &mut flipped, &tag), Err(AeadError::AuthenticationFailed));

    let mut bad_tag = tag;
    bad_tag[15] ^= 1;
    assert_eq!(cipher.open(&nonce, 0x11, &mut sealed.clone(), &bad_tag), Err(AeadError::AuthenticationFailed));

    assert_eq!(cipher.open(&nonce, 0x12, &mut sealed.clone(), &tag), Err(AeadError::AuthenticationFailed));

    let other = BasisAeadCipher::new(&[4u8; 32]).unwrap();
    assert_eq!(other.open(&nonce, 0x11, &mut sealed.clone(), &tag), Err(AeadError::AuthenticationFailed));

    let mut wrong_nonce = sealed.clone();
    assert_eq!(cipher.open(&[6u8; 12], 0x11, &mut wrong_nonce, &tag), Err(AeadError::AuthenticationFailed));
}

#[test]
fn x25519_refuses_bad_lengths_and_low_order_points() {
    let (private, public) = BasisX25519::generate_key_pair();
    assert_eq!(private.len(), BasisX25519::KEY_SIZE);
    assert_eq!(public.len(), BasisX25519::KEY_SIZE);
    assert_eq!(BasisX25519::derive_public_key(&private).unwrap(), public);

    assert_eq!(BasisX25519::derive_public_key(&private[..31]).err(), Some(X25519Error::KeyLength { expected: 32, actual: 31 }));
    assert_eq!(BasisX25519::agree(&[0u8; 33], &public).err(), Some(X25519Error::KeyLength { expected: 32, actual: 33 }));
    assert_eq!(BasisX25519::agree(&private, &[]).err(), Some(X25519Error::KeyLength { expected: 32, actual: 0 }));
    // The all-zero point is low order: the shared secret would be zero for every private key.
    assert_eq!(BasisX25519::agree(&private, &[0u8; 32]).err(), Some(X25519Error::NonContributory));

    let (other_private, other_public) = BasisX25519::generate_key_pair();
    let ab = BasisX25519::agree(&private, &other_public).unwrap();
    let ba = BasisX25519::agree(&other_private, &public).unwrap();
    assert_eq!(ab, ba);
    assert_eq!(ab.len(), BasisX25519::SHARED_SECRET_SIZE);
}

#[test]
fn hkdf_refuses_outputs_it_cannot_produce() {
    let max = BasisHkdf::MAX_OUTPUT_LENGTH;
    assert_eq!(BasisHkdf::derive_key(b"ikm", b"salt", b"info", max + 1).err(), Some(HkdfLengthError { length: max + 1, max }));
    assert_eq!(BasisHkdf::derive_key(b"ikm", b"salt", b"info", max).unwrap().len(), max);
    let a = BasisHkdf::derive_key(b"ikm", b"salt", b"info", 32).unwrap();
    let b = BasisHkdf::derive_key(b"ikm", b"salt", b"info", 32).unwrap();
    assert_eq!(a, b);
    assert_ne!(a, BasisHkdf::derive_key(b"ikm", b"salt", b"other", 32).unwrap());
    assert_eq!(BasisHkdf::derive_key(b"ikm", b"salt", b"info", 0).unwrap().len(), 0);
}

#[test]
fn ed25519_rejects_malformed_keys_and_signatures_without_panicking() {
    let bad_priv = PrivKey(vec![1u8; 31]);
    assert!(Ed25519::sign(&bad_priv, &Payload(b"x".to_vec())).is_none());
    assert!(Ed25519::convert_privkey_to_pubkey(&bad_priv).is_none());

    let priv_key = PrivKey(vec![9u8; 32]);
    let pub_key = Ed25519::convert_privkey_to_pubkey(&priv_key).unwrap();
    let payload = Payload(b"challenge".to_vec());
    let sig = Ed25519::sign(&priv_key, &payload).unwrap();
    assert!(Ed25519::verify(&pub_key, &sig, &payload));

    assert!(!Ed25519::verify(&PubKey(vec![0u8; 31]), &sig, &payload));
    assert!(!Ed25519::verify(&pub_key, &Signature(vec![0u8; 63]), &payload));
    assert!(!Ed25519::verify(&pub_key, &sig, &Payload(b"other".to_vec())));
    let mut tampered = sig.0.clone();
    tampered[10] ^= 0x80;
    assert!(!Ed25519::verify(&pub_key, &Signature(tampered), &payload));
}
