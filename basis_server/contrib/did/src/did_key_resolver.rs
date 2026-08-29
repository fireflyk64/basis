use std::collections::HashMap;

use basis_crypto::{Ed25519, PubKey};

use crate::base64_url_safe::Base64UrlSafe;
use crate::did_auth::DidResolveErr;
use crate::did_document::DidDocument;
use crate::i_did_method::{DidMethodKind, IDidMethod};
use crate::json_web_key::JsonWebKey;
use crate::newtypes::{Did, DidUrlFragment};

/// Implements resolution of a did:key to the various information stored in it.
#[derive(Debug, Default, Clone, Copy)]
pub struct DidKeyResolver;

impl DidKeyResolver {
    pub const PREFIX: &'static str = "did:key:";

    /// https://github.com/multiformats/multicodec/blob/master/table.csv#L98
    const ED25519_MULTIFORMAT_CODE: u32 = 0xED;

    /// https://datatracker.ietf.org/doc/html/draft-multiformats-multibase#appendix-D.1
    const BASE58_BTC_MULTIBASE_CODE: char = 'z';

    pub fn resolve(did: &Did) -> Result<DidDocument, DidKeyDecodeException> {
        let multibase_part = did.0.strip_prefix(Self::PREFIX).unwrap_or(&did.0);
        let mut chars = multibase_part.chars();
        // did:key uses base58-btc encoding, see the spec here:
        // https://w3c-ccg.github.io/did-method-key/#format
        if chars.next() != Some(Self::BASE58_BTC_MULTIBASE_CODE) {
            return Err(DidKeyDecodeException::new(DidKeyDecodeError::NotBase58Btc));
        }
        let multicodec_prefixed = bs58::decode(chars.as_str())
            .into_vec()
            .map_err(|_| DidKeyDecodeException::new(DidKeyDecodeError::NotBase58Btc))?;
        let (codec_id, pubkey_bytes) = unsigned_varint::decode::u16(&multicodec_prefixed)
            .map_err(|_| DidKeyDecodeException::new(DidKeyDecodeError::VarintWouldOverflow))?;
        // For now we only support Ed25519 pubkeys.
        if u32::from(codec_id) != Self::ED25519_MULTIFORMAT_CODE {
            return Err(DidKeyDecodeException::new(DidKeyDecodeError::UnsupportedPubkeyType));
        }
        if pubkey_bytes.len() != Ed25519::PUBKEY_SIZE {
            return Err(DidKeyDecodeException::new(DidKeyDecodeError::WrongPubkeyLen));
        }

        let mut pubkeys = HashMap::new();
        pubkeys.insert(
            DidUrlFragment(multibase_part.to_string()),
            Self::create_ed25519_jwk(pubkey_bytes),
        );
        Ok(DidDocument::new(pubkeys))
    }

    pub fn encode_pubkey_as_did(pub_key: &PubKey) -> Did {
        let mut buf = unsigned_varint::encode::u32_buffer();
        let code = unsigned_varint::encode::u32(Self::ED25519_MULTIFORMAT_CODE, &mut buf);
        let mut with_multiformat_code = Vec::with_capacity(code.len() + pub_key.0.len());
        with_multiformat_code.extend_from_slice(code);
        with_multiformat_code.extend_from_slice(&pub_key.0);

        let base58_encoded = bs58::encode(&with_multiformat_code).into_string();
        Did(format!("{}{}{}", Self::PREFIX, Self::BASE58_BTC_MULTIBASE_CODE, base58_encoded))
    }

    fn create_ed25519_jwk(pubkey_bytes: &[u8]) -> JsonWebKey {
        debug_assert_eq!(pubkey_bytes.len(), Ed25519::PUBKEY_SIZE);
        JsonWebKey {
            kty: Some("OKP".into()),
            crv: Some("Ed25519".into()),
            x: Some(Base64UrlSafe::encode(pubkey_bytes)),
            ..Default::default()
        }
    }
}

impl IDidMethod for DidKeyResolver {
    fn resolve_document(&self, did: &Did) -> Result<DidDocument, DidResolveErr> {
        Self::resolve(did).map_err(|_| DidResolveErr::Other)
    }

    fn kind(&self) -> DidMethodKind {
        DidMethodKind::Key
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DidKeyDecodeError {
    /// The public key type is not supported.
    UnsupportedPubkeyType,
    /// Decoding the multicodec varint of the pubkey type overflowed.
    VarintWouldOverflow,
    /// The did key's method specific identifier should have been base58-btc encoded, but was not.
    NotBase58Btc,
    /// The number of bytes in the pubkey did not match the number of bytes expected for the key type.
    WrongPubkeyLen,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{error:?}")]
pub struct DidKeyDecodeException {
    pub error: DidKeyDecodeError,
}

impl DidKeyDecodeException {
    pub fn new(error: DidKeyDecodeError) -> Self {
        Self { error }
    }
}
