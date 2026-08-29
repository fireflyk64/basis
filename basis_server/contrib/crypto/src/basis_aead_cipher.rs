use chacha20poly1305::aead::AeadInOut;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce, Tag};

/// ChaCha20-Poly1305 AEAD bound to a single 32-byte key, reusable across many packets.
///
/// `seal`/`open` operate in place on a caller-owned buffer; the 16-byte tag is written to /
/// read from a caller-supplied location. Instances are safe for concurrent use — unlike the
/// native .NET cipher context this is a pure-Rust implementation with no shared mutable state,
/// so the lock the C# version needed is gone.
pub struct BasisAeadCipher {
    cipher: ChaCha20Poly1305,
}

impl BasisAeadCipher {
    pub const KEY_SIZE: usize = 32;
    pub const NONCE_SIZE: usize = 12;
    pub const TAG_SIZE: usize = 16;

    /// Panics when the key is not `KEY_SIZE` bytes, mirroring the C# `ArgumentException`.
    /// Use [`BasisAeadCipher::try_new`] to get a `Result` instead.
    pub fn new(key: &[u8]) -> Self {
        Self::try_new(key).unwrap_or_else(|e| panic!("{e}"))
    }

    pub fn try_new(key: &[u8]) -> Result<Self, AeadKeyError> {
        if key.len() != Self::KEY_SIZE {
            return Err(AeadKeyError { length: key.len() });
        }
        let key = Key::try_from(key).map_err(|_| AeadKeyError { length: key.len() })?;
        Ok(Self { cipher: ChaCha20Poly1305::new(&key) })
    }

    /// Encrypts `buffer` in place and writes the authentication tag to `tag_dest`.
    pub fn seal(&self, nonce: &[u8], aad: u8, buffer: &mut [u8], tag_dest: &mut [u8]) {
        let nonce = Nonce::try_from(nonce).expect("nonce is NONCE_SIZE bytes");
        let tag = self
            .cipher
            .encrypt_inout_detached(&nonce, &[aad], buffer.into())
            .expect("ChaCha20-Poly1305 seal cannot fail for a bounded buffer");
        tag_dest[..Self::TAG_SIZE].copy_from_slice(&tag);
    }

    /// Decrypts `buffer` in place, verifying against `tag`. Returns false (and leaves the
    /// buffer undefined) on tag mismatch or a malformed input.
    pub fn open(&self, nonce: &[u8], aad: u8, buffer: &mut [u8], tag: &[u8]) -> bool {
        if nonce.len() != Self::NONCE_SIZE || tag.len() < Self::TAG_SIZE {
            return false;
        }
        let Ok(nonce) = Nonce::try_from(nonce) else { return false };
        let Ok(tag) = Tag::try_from(&tag[..Self::TAG_SIZE]) else { return false };
        self.cipher
            .decrypt_inout_detached(&nonce, &[aad], buffer.into(), &tag)
            .is_ok()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Key must be {} bytes, got {length}", BasisAeadCipher::KEY_SIZE)]
pub struct AeadKeyError {
    pub length: usize,
}
