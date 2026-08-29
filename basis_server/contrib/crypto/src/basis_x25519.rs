use rand::Rng;
use x25519_dalek::{PublicKey, StaticSecret};

/// X25519 (Curve25519 ECDH) key agreement. 32-byte keys and shared secrets.
pub struct BasisX25519;

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

    pub fn derive_public_key(private_key: &[u8]) -> Vec<u8> {
        let secret = StaticSecret::from(Self::key(private_key));
        PublicKey::from(&secret).to_bytes().to_vec()
    }

    pub fn agree(private_key: &[u8], peer_public_key: &[u8]) -> Vec<u8> {
        let secret = StaticSecret::from(Self::key(private_key));
        let public = PublicKey::from(Self::key(peer_public_key));
        secret.diffie_hellman(&public).to_bytes().to_vec()
    }

    fn key(bytes: &[u8]) -> [u8; 32] {
        <[u8; 32]>::try_from(bytes).unwrap_or_else(|_| {
            panic!("X25519 keys are {} bytes, got {}", Self::KEY_SIZE, bytes.len())
        })
    }
}
