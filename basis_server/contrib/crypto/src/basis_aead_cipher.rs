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

/// Why an AEAD operation was refused. Every variant is a permanent fault for the packet it
/// concerns: retrying the same bytes gives the same answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AeadError {
    #[error("Key must be {expected} bytes, got {actual}")]
    KeyLength { expected: usize, actual: usize },
    #[error("Nonce must be {expected} bytes, got {actual}")]
    NonceLength { expected: usize, actual: usize },
    #[error("Tag buffer must hold {expected} bytes, got {actual}")]
    TagLength { expected: usize, actual: usize },
    /// The tag did not verify: the packet was tampered with, or sealed under another key.
    #[error("authentication failed")]
    AuthenticationFailed,
    #[error("cipher failure")]
    CipherFailure,
}

impl BasisAeadCipher {
    pub const KEY_SIZE: usize = 32;
    pub const NONCE_SIZE: usize = 12;
    pub const TAG_SIZE: usize = 16;

    /// Binds a 32-byte key. The C# constructor threw `ArgumentException` on any other length.
    pub fn new(key: &[u8]) -> Result<Self, AeadError> {
        if key.len() != Self::KEY_SIZE {
            return Err(AeadError::KeyLength { expected: Self::KEY_SIZE, actual: key.len() });
        }
        let key = Key::try_from(key).map_err(|_| AeadError::KeyLength { expected: Self::KEY_SIZE, actual: key.len() })?;
        Ok(Self { cipher: ChaCha20Poly1305::new(&key) })
    }

    /// Alias of [`new`](Self::new).
    pub fn try_new(key: &[u8]) -> Result<Self, AeadError> {
        Self::new(key)
    }

    fn nonce(nonce: &[u8]) -> Result<Nonce, AeadError> {
        if nonce.len() != Self::NONCE_SIZE {
            return Err(AeadError::NonceLength { expected: Self::NONCE_SIZE, actual: nonce.len() });
        }
        Nonce::try_from(nonce).map_err(|_| AeadError::NonceLength { expected: Self::NONCE_SIZE, actual: nonce.len() })
    }

    /// Encrypts `buffer` in place and writes the authentication tag to the first
    /// [`TAG_SIZE`](Self::TAG_SIZE) bytes of `tag_dest`.
    pub fn seal(&self, nonce: &[u8], aad: u8, buffer: &mut [u8], tag_dest: &mut [u8]) -> Result<(), AeadError> {
        let nonce = Self::nonce(nonce)?;
        let Some(tag_dest) = tag_dest.get_mut(..Self::TAG_SIZE) else {
            return Err(AeadError::TagLength { expected: Self::TAG_SIZE, actual: tag_dest.len() });
        };
        let tag = self
            .cipher
            .encrypt_inout_detached(&nonce, &[aad], buffer.into())
            .map_err(|_| AeadError::CipherFailure)?;
        tag_dest.copy_from_slice(&tag);
        Ok(())
    }

    /// Decrypts `buffer` in place, verifying against the first [`TAG_SIZE`](Self::TAG_SIZE)
    /// bytes of `tag`. On any error the buffer contents are undefined and must be discarded.
    pub fn open(&self, nonce: &[u8], aad: u8, buffer: &mut [u8], tag: &[u8]) -> Result<(), AeadError> {
        let nonce = Self::nonce(nonce)?;
        let Some(tag) = tag.get(..Self::TAG_SIZE) else {
            return Err(AeadError::TagLength { expected: Self::TAG_SIZE, actual: tag.len() });
        };
        let tag = Tag::try_from(tag).map_err(|_| AeadError::TagLength { expected: Self::TAG_SIZE, actual: tag.len() })?;
        self.cipher
            .decrypt_inout_detached(&nonce, &[aad], buffer.into(), &tag)
            .map_err(|_| AeadError::AuthenticationFailed)
    }
}
