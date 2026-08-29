use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

use crate::{Payload, PrivKey, PubKey, Signature};

/// Ed25519 elliptic-curve signature algorithm.
pub struct Ed25519;

impl Ed25519 {
    pub const PUBKEY_SIZE: usize = ed25519_dalek::PUBLIC_KEY_LENGTH;
    pub const PRIVKEY_SIZE: usize = ed25519_dalek::SECRET_KEY_LENGTH;
    pub const SIGNATURE_SIZE: usize = ed25519_dalek::SIGNATURE_LENGTH;

    /// Returns `None` when the conversion failed. Should never fail as long as the privkey is
    /// a valid privkey.
    pub fn convert_privkey_to_pubkey(privkey: &PrivKey) -> Option<PubKey> {
        let secret: &[u8; Self::PRIVKEY_SIZE] = privkey.0.as_slice().try_into().ok()?;
        let signing = SigningKey::from_bytes(secret);
        Some(PubKey(signing.verifying_key().to_bytes().to_vec()))
    }

    /// Returns `false` if verification failed.
    pub fn verify(pubkey: &PubKey, sig: &Signature, payload: &Payload) -> bool {
        let Ok(pub_bytes) = <&[u8; Self::PUBKEY_SIZE]>::try_from(pubkey.0.as_slice()) else {
            return false;
        };
        let Ok(verifying) = VerifyingKey::from_bytes(pub_bytes) else {
            return false;
        };
        let Ok(sig_bytes) = <&[u8; Self::SIGNATURE_SIZE]>::try_from(sig.0.as_slice()) else {
            return false;
        };
        let signature = ed25519_dalek::Signature::from_bytes(sig_bytes);
        verifying.verify_strict(&payload.0, &signature).is_ok()
    }

    /// Returns `None` if signing failed (the C# version returned `false` and a null signature).
    pub fn sign(privkey: &PrivKey, payload: &Payload) -> Option<Signature> {
        let secret: &[u8; Self::PRIVKEY_SIZE] = privkey.0.as_slice().try_into().ok()?;
        let signing = SigningKey::from_bytes(secret);
        let signature = signing.sign(&payload.0);
        Some(Signature(signature.to_bytes().to_vec()))
    }
}
