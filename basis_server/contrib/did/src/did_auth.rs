use std::collections::HashMap;
use std::sync::Mutex;

use basis_crypto::{Ed25519, Payload, Signature, SigningAlgorithm};
use rand::{Rng, SeedableRng, rngs::StdRng};

use crate::did_document::DidDocument;
use crate::did_key_resolver::DidKeyResolver;
use crate::i_did_method::{DidMethodKind, IDidMethod};
use crate::json_web_key::JsonWebKey;
use crate::newtypes::{Did, DidUrlFragment, Nonce};

/// Configuration for [`DidAuthentication`].
pub struct Config {
    /// Stored so deterministic testing and seeding is possible.
    pub rng: Box<dyn Rng + Send>,
    pub resolvers: HashMap<DidMethodKind, Box<dyn IDidMethod>>,
}

impl Default for Config {
    fn default() -> Self {
        let mut resolvers: HashMap<DidMethodKind, Box<dyn IDidMethod>> = HashMap::new();
        // We will add more did methods in the future, like did:web
        resolvers.insert(DidMethodKind::Key, Box::new(DidKeyResolver));
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        Self {
            rng: Box::new(StdRng::from_seed(seed)),
            resolvers,
        }
    }
}

impl Config {
    pub fn with_rng(rng: impl Rng + Send + 'static) -> Self {
        Self {
            rng: Box::new(rng),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DidResolveErr {
    /// Another generic error happened during DID document resolution.
    Other,
    /// Did method is not supported.
    UnsupportedMethod,
    /// Did had an invalid prefix.
    InvalidPrefix,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DidFragmentErr {
    /// The given fragment was ambiguous.
    AmbiguousFragment,
    /// No such fragment was present in the DID document.
    NoSuchFragment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DidSignatureErr {
    InvalidSignature,
    UnsupportedSignatureAlgorithm,
}

/// The C# `IDidVerifyErr` marker interface, as the closed set of errors verification can produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IDidVerifyErr {
    #[error("resolve: {0:?}")]
    Resolve(DidResolveErr),
    #[error("fragment: {0:?}")]
    Fragment(DidFragmentErr),
    #[error("signature: {0:?}")]
    Signature(DidSignatureErr),
}

impl From<DidResolveErr> for IDidVerifyErr {
    fn from(e: DidResolveErr) -> Self {
        Self::Resolve(e)
    }
}
impl From<DidFragmentErr> for IDidVerifyErr {
    fn from(e: DidFragmentErr) -> Self {
        Self::Fragment(e)
    }
}
impl From<DidSignatureErr> for IDidVerifyErr {
    fn from(e: DidSignatureErr) -> Self {
        Self::Signature(e)
    }
}

// TODO(@thebutlah): Create and implement an `IChallengeResponseAuth` interface. This
// interface should live in basis core.
pub struct DidAuthentication {
    /// Number of bytes in a nonce. This is currently 256 bits.
    // TODO(@thebutlah): Decide if its too performance intensive to use 256 bits, and if 128 bit
    // would be sufficient.
    rng: Mutex<Box<dyn Rng + Send>>,
    resolvers: HashMap<DidMethodKind, Box<dyn IDidMethod>>,
}

impl DidAuthentication {
    const NONCE_LEN: usize = 256 / 8;

    pub fn new(cfg: Config) -> Self {
        Self {
            rng: Mutex::new(cfg.rng),
            resolvers: cfg.resolvers,
        }
    }

    pub fn make_challenge(&self, identity: Did) -> Challenge {
        let mut nonce = vec![0u8; Self::NONCE_LEN];
        self.rng
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .fill_bytes(&mut nonce);
        Challenge {
            identity,
            nonce: Nonce(nonce),
        }
    }

    /// Compares the response against the original challenge.
    ///
    /// Ensures that:
    /// * The response signature matches the public keys of the challenge identity.
    /// * The response signature payload matches the nonce in the challenge
    ///
    /// It is the caller's responsibility to keep track of which challenges should be held for
    /// which responses.
    pub fn verify_response(
        &self,
        response: &Response,
        challenge: &Challenge,
    ) -> Result<(), IDidVerifyErr> {
        let document = self.resolve_did(&challenge.identity)?;
        let pubkey = Self::retrieve_key(&document, &response.did_url_fragment)?;
        Self::verify_signature(pubkey, &challenge.nonce, &response.signature)?;
        Ok(())
    }

    fn verify_signature(
        pubkey: &JsonWebKey,
        nonce: &Nonce,
        signature: &Signature,
    ) -> Result<(), DidSignatureErr> {
        match pubkey.get_algorithm() {
            Some(SigningAlgorithm::Ed25519) => {
                if Ed25519::verify(&pubkey.decode_pubkey(), signature, &Payload(nonce.0.clone())) {
                    Ok(())
                } else {
                    Err(DidSignatureErr::InvalidSignature)
                }
            }
            None => Err(DidSignatureErr::UnsupportedSignatureAlgorithm),
        }
    }

    fn retrieve_key<'a>(
        document: &'a DidDocument,
        key_id: &DidUrlFragment,
    ) -> Result<&'a JsonWebKey, DidFragmentErr> {
        if document.pubkeys.len() == 1 {
            return Ok(document.pubkeys.values().next().expect("one key"));
        }
        document
            .pubkeys
            .get(key_id)
            .ok_or(DidFragmentErr::NoSuchFragment)
    }

    fn resolve_did(&self, identity: &Did) -> Result<DidDocument, DidResolveErr> {
        let mut segments = identity.0.splitn(3, ':');
        let (Some(scheme), Some(method), Some(_rest)) =
            (segments.next(), segments.next(), segments.next())
        else {
            return Err(DidResolveErr::InvalidPrefix);
        };
        if scheme != "did" {
            return Err(DidResolveErr::InvalidPrefix);
        }
        let method = match method {
            "key" => DidMethodKind::Key,
            _ => return Err(DidResolveErr::UnsupportedMethod),
        };
        let resolver = self
            .resolvers
            .get(&method)
            .ok_or(DidResolveErr::UnsupportedMethod)?;
        resolver.resolve_document(identity)
    }
}

/// Challenges are a randomized nonce. The nonce will be the payload that is signed by the
/// user's private key. Generating a random nonce for every authentication attempt ensures that
/// an attacker cannot perform a [replay attack](https://en.wikipedia.org/wiki/Replay_attack).
///
/// Challenges also track the identity of the party that the challenge was sent to, so that
/// later the signature's public key can be compared to the identity's public key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Challenge {
    pub identity: Did,
    pub nonce: Nonce,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// The raw bytes of the signature. For ed25519 this is 64 bytes long.
    pub signature: Signature,
    /// The particular key in the user's did document. If the empty string, it is implied that
    /// there is only one key in the document and that this single key should be what is used
    /// as the pub key.
    ///
    /// Examples:
    /// * `""`
    /// * `"key-0"`
    /// * `"z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK"`
    pub did_url_fragment: DidUrlFragment,
}
