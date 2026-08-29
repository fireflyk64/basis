use rand::Rng;
use x25519_dalek::{PublicKey, StaticSecret};

/// X25519 (Curve25519 ECDH) key agreement. 32-byte keys and shared secrets.
pub struct BasisX25519;

/// Why an X25519 operation was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum X25519Error {
    #[error("X25519 keys are {expected} bytes, got {actual}")]
    KeyLength { expected: usize, actual: usize },
    /// The peer's public key is a low-order point, so the shared secret would be all zeros and
    /// independent of our private key. Refusing it is what RFC 7748 §6.1 recommends.
    #[error("X25519 peer public key is a low-order point")]
    NonContributory,
}

impl BasisX25519 {
    pub const KEY_SIZE: usize = 32;
    pub const SHARED_SECRET_SIZE: usize = 32;

    /// Returns `(private_key, public_key)` — the C# signature's two `out` parameters, in order.
    pub fn generate_key_pair() -> (Vec<u8>, Vec<u8>) {
        let mut seed = [0u8; Self::KEY_SIZE];
        rand::rng().fill_bytes(&mut seed);
        let secret = StaticSecret::from(seed);
        let public = PublicKey::from(&secret);
        (secret.to_bytes().to_vec(), public.to_bytes().to_vec())
    }

    pub fn derive_public_key(private_key: &[u8]) -> Result<Vec<u8>, X25519Error> {
        let secret = StaticSecret::from(Self::key(private_key)?);
        Ok(PublicKey::from(&secret).to_bytes().to_vec())
    }

    /// The shared secret for our private key and the peer's public key.
    pub fn agree(private_key: &[u8], peer_public_key: &[u8]) -> Result<Vec<u8>, X25519Error> {
        let secret = StaticSecret::from(Self::key(private_key)?);
        let public = PublicKey::from(Self::key(peer_public_key)?);
        let shared = secret.diffie_hellman(&public);
        if !shared.was_contributory() {
            return Err(X25519Error::NonContributory);
        }
        Ok(shared.to_bytes().to_vec())
    }

    fn key(bytes: &[u8]) -> Result<[u8; 32], X25519Error> {
        <[u8; 32]>::try_from(bytes).map_err(|_| X25519Error::KeyLength { expected: Self::KEY_SIZE, actual: bytes.len() })
    }
}
