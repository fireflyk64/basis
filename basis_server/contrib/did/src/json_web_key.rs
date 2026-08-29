use basis_crypto::{PrivKey, PubKey, SigningAlgorithm};
use serde::{Deserialize, Serialize};

use crate::base64_url_safe::Base64UrlSafe;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonWebKey {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kty: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#use: Option<String>,
    /// Public portion
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<String>,
    /// Private portion
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crv: Option<String>,
    /// Symmetric key parameter
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k: Option<String>,
}

impl JsonWebKey {
    /// Compact JSON with null members omitted, like the C# serializer settings.
    pub fn serialize(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn deserialize(json: &str) -> Option<JsonWebKey> {
        serde_json::from_str(json).ok()
    }

    /// Returns `None` if the algorithm is unknown.
    pub fn get_algorithm(&self) -> Option<SigningAlgorithm> {
        if self.kty.as_deref() == Some("OKP") && self.crv.as_deref() == Some("Ed25519") {
            return Some(SigningAlgorithm::Ed25519);
        }
        None
    }

    pub fn decode_pubkey(&self) -> PubKey {
        PubKey(Base64UrlSafe::decode(self.x.as_deref().unwrap_or("")).unwrap_or_default())
    }

    pub fn decode_privkey(&self) -> PrivKey {
        PrivKey(Base64UrlSafe::decode(self.d.as_deref().unwrap_or("")).unwrap_or_default())
    }

    pub fn is_pubkey(&self) -> bool {
        self.d.is_none()
    }
}
