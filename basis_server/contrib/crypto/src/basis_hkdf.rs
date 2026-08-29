use hkdf::Hkdf;
use sha2::Sha256;

/// HKDF-SHA256 (RFC 5869). Used to expand an X25519 shared secret into directional AEAD keys.
pub struct BasisHkdf;

impl BasisHkdf {
    pub fn derive_key(ikm: &[u8], salt: &[u8], info: &[u8], length: usize) -> Vec<u8> {
        let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
        let mut output = vec![0u8; length];
        hk.expand(info, &mut output)
            .expect("HKDF-SHA256 output length is bounded to 255 * 32 bytes");
        output
    }
}
