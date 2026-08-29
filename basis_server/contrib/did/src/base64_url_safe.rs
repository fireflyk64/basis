use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

/// Base64 url-safe encode and decode.
pub struct Base64UrlSafe;

impl Base64UrlSafe {
    pub fn encode(bytes: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(bytes)
    }

    /// Mirrors the C# behaviour: `-`/`_` are mapped back, padding is restored from the
    /// length, and a length of 1 mod 4 is a `FormatException`.
    pub fn decode(s: &str) -> Result<Vec<u8>, Base64UrlSafeError> {
        let mut base64: String = s
            .chars()
            .map(|c| match c {
                '-' => '+',
                '_' => '/',
                other => other,
            })
            .collect();
        match base64.len() % 4 {
            0 => {}
            2 => base64.push_str("=="),
            3 => base64.push('='),
            _ => return Err(Base64UrlSafeError::InvalidLength),
        }
        STANDARD
            .decode(base64)
            .map_err(|e| Base64UrlSafeError::Invalid(e.to_string()))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Base64UrlSafeError {
    #[error("Invalid base64url string length")]
    InvalidLength,
    #[error("Invalid base64url string: {0}")]
    Invalid(String),
}
