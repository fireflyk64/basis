use basis_crypto::{BasisAeadCipher, BasisHkdf, BasisX25519, HkdfLengthError, X25519Error};

/// Why a peer key derivation was refused. Every variant is permanent: the same inputs will be
/// refused again, so the caller reports it and does not retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HandshakeError {
    #[error("{what} must be {expected} bytes, got {actual}")]
    KeyLength { what: &'static str, expected: usize, actual: usize },
    /// Both ends presented the same public key, so the A/B role cannot be decided.
    #[error("both peers presented the same public key")]
    IdenticalKeys,
    #[error(transparent)]
    X25519(#[from] X25519Error),
    #[error(transparent)]
    Hkdf(#[from] HkdfLengthError),
}

/// X25519 + HKDF-SHA256 key agreement for the encrypted peer-to-peer (direct) link. The two
/// peers exchange ephemeral public keys through the server's signalling channel and each derives
/// the same two directional keys, because ECDH(myPriv, peerPub) is symmetric; the transcript
/// (both public keys) is folded into the HKDF salt for channel binding.
pub struct BasisCryptoHandshake;

impl BasisCryptoHandshake {
    pub const PUBLIC_KEY_SIZE: usize = BasisX25519::KEY_SIZE;
    pub const PRIVATE_KEY_SIZE: usize = BasisX25519::KEY_SIZE;
    pub const KEY_SIZE: usize = BasisAeadCipher::KEY_SIZE;

    const INFO_AB: &'static [u8] = b"basis-crypto-v1-ab";
    const INFO_BA: &'static [u8] = b"basis-crypto-v1-ba";

    /// Returns `(private_key, public_key)`.
    pub fn generate_key_pair() -> (Vec<u8>, Vec<u8>) {
        BasisX25519::generate_key_pair()
    }

    /// Derives the directional keys for a peer-to-peer link. Role is decided by public-key
    /// ordering so both ends agree without extra signalling. Returns `(send_key, recv_key)`.
    pub fn derive_peer_keys(
        my_private: &[u8],
        my_public: &[u8],
        peer_public: &[u8],
    ) -> Result<(Vec<u8>, Vec<u8>), HandshakeError> {
        let check = |what: &'static str, key: &[u8], expected: usize| {
            if key.len() != expected {
                Err(HandshakeError::KeyLength { what, expected, actual: key.len() })
            } else {
                Ok(())
            }
        };
        check("private key", my_private, Self::PRIVATE_KEY_SIZE)?;
        check("public key", my_public, Self::PUBLIC_KEY_SIZE)?;
        check("peer public key", peer_public, Self::PUBLIC_KEY_SIZE)?;
        let cmp = my_public.cmp(peer_public);
        if cmp == std::cmp::Ordering::Equal {
            return Err(HandshakeError::IdenticalKeys);
        }
        let i_am_a = cmp == std::cmp::Ordering::Less;

        let (a_pub, b_pub) = if i_am_a { (my_public, peer_public) } else { (peer_public, my_public) };

        let shared = BasisX25519::agree(my_private, peer_public)?;
        let mut salt = Vec::with_capacity(a_pub.len() + b_pub.len());
        salt.extend_from_slice(a_pub);
        salt.extend_from_slice(b_pub);
        let key_ab = BasisHkdf::derive_key(&shared, &salt, Self::INFO_AB, Self::KEY_SIZE)?;
        let key_ba = BasisHkdf::derive_key(&shared, &salt, Self::INFO_BA, Self::KEY_SIZE)?;

        Ok(if i_am_a { (key_ab, key_ba) } else { (key_ba, key_ab) })
    }
}
