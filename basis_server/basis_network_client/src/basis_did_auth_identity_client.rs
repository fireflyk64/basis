//! Port of `BasisDIDAuthIdentityClient.cs`: the client half of the DID challenge/response.
//!
//! The Unity build persisted the key pair in PlayerPrefs; a headless client keeps it in a small
//! JSON file when a store directory is configured, and otherwise generates a fresh identity per
//! process (which is what the C# non-Unity path effectively did).

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use base64::Engine;
use basis_crypto::{Ed25519, Payload, PrivKey, PubKey};
use basis_did::did_auth::Response;
use basis_did::did_key_resolver::DidKeyResolver;
use basis_did::newtypes::{Did, DidUrlFragment};
use basis_error::{BasisError, BasisResult, ErrorCode, ResultExt};
use basis_network_core::SerializableBasis::BytesMessage;
use basis_network_core::{BNL, NetDataReader, NetDataWriter};
use parking_lot::Mutex;
use rand::Rng;

/// A client's signing identity: the key pair and the did:key it encodes.
#[derive(Clone, Debug)]
pub struct ClientIdentity {
    pub public_key: PubKey,
    pub private_key: PrivKey,
    pub did: Did,
    pub did_url_fragment: DidUrlFragment,
}

impl ClientIdentity {
    /// A fresh random identity.
    pub fn generate() -> BasisResult<Self> {
        let (public_key, private_key) = BasisDIDAuthIdentityClient::random_key_pair()?;
        let did = DidKeyResolver::encode_pubkey_as_did(&public_key);
        Ok(Self { public_key, private_key, did, did_url_fragment: DidUrlFragment::new(String::new()) })
    }

    /// Signs a challenge nonce and writes the `[signature][fragment]` response frame.
    pub fn answer_challenge(&self, nonce: &[u8]) -> BasisResult<NetDataWriter> {
        let payload = Payload::new(nonce.to_vec());
        let signature = Ed25519::sign(&self.private_key, &payload).ok_or_else(|| BasisError::permanent(ErrorCode::Crypto, "Unable to sign Key"))?;
        if !Ed25519::verify(&self.public_key, &signature, &payload) {
            return Err(BasisError::permanent(ErrorCode::Crypto, "Unable to Verify Key"));
        }
        // For simplicity, an empty fragment: the client has exactly one public key.
        let response = Response { signature, did_url_fragment: self.did_url_fragment.clone() };
        let mut writer = NetDataWriter::new();
        BytesMessage.serialize(&mut writer, response.signature.v()).context("writing the signature")?;
        let fragment = if response.did_url_fragment.v().is_empty() { "N/A" } else { response.did_url_fragment.v() };
        BytesMessage.serialize(&mut writer, fragment.as_bytes()).context("writing the fragment")?;
        Ok(writer)
    }
}

static IDENTITY: LazyLock<Mutex<Option<ClientIdentity>>> = LazyLock::new(|| Mutex::new(None));
static STORE_DIRECTORY: Mutex<Option<PathBuf>> = Mutex::new(None);

pub struct BasisDIDAuthIdentityClient;

impl BasisDIDAuthIdentityClient {
    pub const IDENTITY_FILE_NAME: &'static str = "identity-did.json";

    /// Where `get_or_save_did` persists the key pair. `None` (the default) keeps it in memory.
    pub fn set_store_directory(directory: Option<PathBuf>) {
        *STORE_DIRECTORY.lock() = directory;
    }

    /// The current identity, if one has been created.
    pub fn identity() -> Option<ClientIdentity> {
        IDENTITY.lock().clone()
    }

    /// Installs a specific identity (tests, or a client that manages its own keys).
    pub fn set_identity(identity: ClientIdentity) {
        *IDENTITY.lock() = Some(identity);
    }

    pub fn did() -> Option<Did> {
        IDENTITY.lock().as_ref().map(|i| i.did.clone())
    }

    /// Loads the stored identity or creates and stores a new one, returning its DID. An
    /// unreadable or unwritable store is logged and a fresh in-memory identity is used, so a
    /// client can always connect.
    pub fn get_or_save_did() -> String {
        if let Some(identity) = Self::identity() {
            return identity.did.v().to_string();
        }
        let directory = STORE_DIRECTORY.lock().clone();
        let identity = match directory {
            Some(dir) => Self::load_or_create_in(&dir).unwrap_or_else(|e| {
                BNL::log_error(format!("Could not use the identity store in '{}': {e}; using a fresh identity for this run", dir.display()));
                None
            }),
            None => None,
        };
        let identity = match identity.map(Ok).unwrap_or_else(ClientIdentity::generate) {
            Ok(identity) => identity,
            Err(e) => {
                BNL::log_error(format!("Could not create a client identity: {e}"));
                return String::new();
            }
        };
        let did = identity.did.v().to_string();
        *IDENTITY.lock() = Some(identity);
        did
    }

    fn load_or_create_in(directory: &Path) -> BasisResult<Option<ClientIdentity>> {
        let path = directory.join(Self::IDENTITY_FILE_NAME);
        if path.exists() {
            let text = std::fs::read_to_string(&path).with_context(|| format!("reading '{}'", path.display()))?;
            return Self::parse_store(&text).map(Some);
        }
        let identity = ClientIdentity::generate()?;
        std::fs::create_dir_all(directory).with_context(|| format!("creating '{}'", directory.display()))?;
        std::fs::write(&path, Self::render_store(&identity)).with_context(|| format!("writing '{}'", path.display()))?;
        Ok(Some(identity))
    }

    fn render_store(identity: &ClientIdentity) -> String {
        let b64 = base64::engine::general_purpose::STANDARD;
        serde_json::json!({
            "PrivateKeyDID": b64.encode(identity.private_key.v()),
            "PublicKeyDID": b64.encode(identity.public_key.v()),
            "DIDID": identity.did.v(),
        })
        .to_string()
    }

    fn parse_store(text: &str) -> BasisResult<ClientIdentity> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| BasisError::permanent(ErrorCode::Serialization, e.to_string()))?;
        let b64 = base64::engine::general_purpose::STANDARD;
        let field = |name: &str| value.get(name).and_then(|v| v.as_str()).ok_or_else(|| BasisError::permanent(ErrorCode::Serialization, format!("identity store is missing {name}")));
        let private_key = b64.decode(field("PrivateKeyDID")?).map_err(|e| BasisError::permanent(ErrorCode::Serialization, format!("PrivateKeyDID: {e}")))?;
        let public_key = b64.decode(field("PublicKeyDID")?).map_err(|e| BasisError::permanent(ErrorCode::Serialization, format!("PublicKeyDID: {e}")))?;
        let did = field("DIDID")?.to_string();
        if private_key.len() != Ed25519::PRIVKEY_SIZE || public_key.len() != Ed25519::PUBKEY_SIZE {
            return Err(BasisError::permanent(ErrorCode::Serialization, "identity store holds keys of the wrong size"));
        }
        Ok(ClientIdentity {
            public_key: PubKey::new(public_key),
            private_key: PrivKey::new(private_key),
            did: Did::new(did),
            did_url_fragment: DidUrlFragment::new(String::new()),
        })
    }

    /// Answers the server's challenge from `reader`. `None` when the challenge is malformed or
    /// there is no identity to sign with.
    pub fn identity_message(reader: &mut NetDataReader) -> Option<NetDataWriter> {
        let identity = Self::identity()?;
        let nonce = BytesMessage.deserialize(reader)?;
        match identity.answer_challenge(&nonce) {
            Ok(writer) => Some(writer),
            Err(e) => {
                BNL::log_error(format!("{e}"));
                None
            }
        }
    }

    pub fn random_key_pair() -> BasisResult<(PubKey, PrivKey)> {
        let mut private = vec![0u8; Ed25519::PRIVKEY_SIZE];
        rand::rng().fill_bytes(&mut private);
        let private_key = PrivKey::new(private);
        let public_key = Ed25519::convert_privkey_to_pubkey(&private_key).ok_or_else(|| BasisError::permanent(ErrorCode::Crypto, "privkey was invalid"))?;
        Ok((public_key, private_key))
    }

    /// A fresh key pair and the did:key that names it.
    pub fn client_key_creation() -> BasisResult<((PubKey, PrivKey), Did)> {
        let (public_key, private_key) = Self::random_key_pair()?;
        let did = DidKeyResolver::encode_pubkey_as_did(&public_key);
        Ok(((public_key, private_key), did))
    }
}
