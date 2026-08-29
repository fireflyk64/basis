use hkdf::Hkdf;
use sha2::Sha256;

/// HKDF-SHA256 (RFC 5869). Used to expand an X25519 shared secret into directional AEAD keys.
pub struct BasisHkdf;

/// The requested output is longer than HKDF-SHA256 can produce (255 × 32 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("HKDF-SHA256 cannot produce {length} bytes (maximum {max})")]
pub struct HkdfLengthError {
    pub length: usize,
    pub max: usize,
}

impl BasisHkdf {
    /// Longest output HKDF-SHA256 can expand to.
    pub const MAX_OUTPUT_LENGTH: usize = 255 * 32;

    pub fn derive_key(ikm: &[u8], salt: &[u8], info: &[u8], length: usize) -> Result<Vec<u8>, HkdfLengthError> {
        if length > Self::MAX_OUTPUT_LENGTH {
            return Err(HkdfLengthError { length, max: Self::MAX_OUTPUT_LENGTH });
        }
        let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
        let mut output = vec![0u8; length];
        hk.expand(info, &mut output)
            .map_err(|_| HkdfLengthError { length, max: Self::MAX_OUTPUT_LENGTH })?;
        Ok(output)
    }
}
