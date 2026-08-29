//! Port of `BasisAvatarNetworkLoad`: the blob a wearer advertises for its avatar. KEEP IN STEP
//! with the client copy — these encode the same wire format and a divergence corrupts every
//! avatar change between them.

use std::io::{Read, Write};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BasisAvatarNetworkLoad {
    pub url: String,
    pub unlock_password: String,
    /// Opaque content-version tag, appended after the original two-field format shipped, so it
    /// must be read optionally.
    pub version_tag: String,
}

impl BasisAvatarNetworkLoad {
    /// Encodes the structure to compressed byte data: three `[u16 len][utf8]` strings, raw deflate.
    pub fn encode_to_bytes(&self) -> Vec<u8> {
        let mut raw = Vec::new();
        Self::write_string(&mut raw, &self.url);
        Self::write_string(&mut raw, &self.unlock_password);
        Self::write_string(&mut raw, &self.version_tag);
        let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        // Writing to a Vec cannot fail; the fallbacks keep the signature infallible like the C#.
        if encoder.write_all(&raw).is_err() {
            return Vec::new();
        }
        encoder.finish().unwrap_or_default()
    }

    /// Decodes from compressed byte data back to the structure.
    pub fn decode_from_bytes(compressed: &[u8]) -> Result<Self, String> {
        let mut raw = Vec::new();
        flate2::read::DeflateDecoder::new(compressed).read_to_end(&mut raw).map_err(|e| format!("deflate: {e}"))?;
        let mut cursor = 0usize;
        let url = Self::read_string(&raw, &mut cursor).ok_or("truncated URL")?;
        let unlock_password = Self::read_string(&raw, &mut cursor).ok_or("truncated UnlockPassword")?;
        // Optional: a record produced before VersionTag existed ends after the two strings above.
        let version_tag = Self::read_string(&raw, &mut cursor).unwrap_or_default();
        Ok(Self { url, unlock_password, version_tag })
    }

    fn write_string(out: &mut Vec<u8>, value: &str) {
        let bytes = value.as_bytes();
        let len = bytes.len().min(u16::MAX as usize);
        out.extend_from_slice(&(len as u16).to_le_bytes());
        out.extend_from_slice(&bytes[..len]);
    }

    fn read_string(raw: &[u8], cursor: &mut usize) -> Option<String> {
        if *cursor + 2 > raw.len() {
            return None;
        }
        let len = u16::from_le_bytes([raw[*cursor], raw[*cursor + 1]]) as usize;
        *cursor += 2;
        if *cursor + len > raw.len() {
            return None;
        }
        let text = String::from_utf8_lossy(&raw[*cursor..*cursor + len]).into_owned();
        *cursor += len;
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_reads_old_records() {
        let load = BasisAvatarNetworkLoad { url: "https://x/a.bee".into(), unlock_password: "pw".into(), version_tag: "v3".into() };
        let bytes = load.encode_to_bytes();
        assert_eq!(BasisAvatarNetworkLoad::decode_from_bytes(&bytes).unwrap(), load);

        // Two-field record from before VersionTag existed.
        let mut raw = Vec::new();
        BasisAvatarNetworkLoad::write_string(&mut raw, "u");
        BasisAvatarNetworkLoad::write_string(&mut raw, "p");
        let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&raw).unwrap();
        let old = encoder.finish().unwrap();
        let decoded = BasisAvatarNetworkLoad::decode_from_bytes(&old).unwrap();
        assert_eq!((decoded.url.as_str(), decoded.unlock_password.as_str(), decoded.version_tag.as_str()), ("u", "p", ""));

        assert!(BasisAvatarNetworkLoad::decode_from_bytes(&[0xff, 0xfe, 0x01]).is_err());
    }
}
