//! Port of `AvatarKeyStoreLoader.cs`: reads the Basis client's saved avatars (ItemKeyStore.json)
//! so a load test can advertise a variety of real avatars.

use std::path::{Path, PathBuf};

use basis_network_core::BNL;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub url: String,
    pub password: String,
    pub load_mode: u8,
}

pub struct AvatarKeyStoreLoader;

impl AvatarKeyStoreLoader {
    const MODE_AVATAR: i64 = 0;
    const LOAD_MODE_NETWORK_DOWNLOADABLE: u8 = 0;

    // Unity puts the keystore under Application.persistentDataPath = LocalLow\<company>\<product>.
    // The project still ships as "Basis Unity", so probe the future name first and fall back.
    const DEFAULT_LOCATIONS: [[&'static str; 2]; 2] = [["BasisVR", "BasisVR"], ["Basis Unity", "Basis Unity"]];

    pub fn resolve_default_path() -> PathBuf {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from).unwrap_or_default();
        let local_low = home.join("AppData").join("LocalLow");
        for location in Self::DEFAULT_LOCATIONS {
            let candidate = local_low.join(location[0]).join(location[1]).join("ItemKeyStore.json");
            if candidate.exists() {
                return candidate;
            }
        }
        local_low.join(Self::DEFAULT_LOCATIONS[0][0]).join(Self::DEFAULT_LOCATIONS[0][1]).join("ItemKeyStore.json")
    }

    pub fn load(configured_path: &str, fallback_load_mode: u8) -> Vec<Entry> {
        let path = if configured_path.trim().is_empty() { Self::resolve_default_path() } else { PathBuf::from(configured_path.trim()) };
        if !path.exists() {
            BNL::log_warning(format!("Avatar keystore not found at [{}] (also probed the other default LocalLow company/product folders).", path.display()));
            return Vec::new();
        }
        match std::fs::read(&path).map_err(|e| e.to_string()).and_then(|bytes| Self::parse(&bytes, fallback_load_mode, &path)) {
            Ok(entries) => entries,
            Err(e) => {
                BNL::log_error(format!("Failed to read avatar keystore at [{}]: {e}", path.display()));
                Vec::new()
            }
        }
    }

    pub fn parse(bytes: &[u8], fallback_load_mode: u8, path: &Path) -> Result<Vec<Entry>, String> {
        let doc: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let Some(data) = doc.get("Value").and_then(|v| v.get("Data")).and_then(|d| d.as_array()) else {
            BNL::log_warning(format!("Avatar keystore at [{}] has no Value.Data array.", path.display()));
            return Ok(Vec::new());
        };
        let mut result = Vec::new();
        for element in data {
            if element.get("Mode").and_then(|m| m.as_i64()) != Some(Self::MODE_AVATAR) {
                continue;
            }
            let Some(url) = element.get("Url").and_then(|u| u.as_str()).map(str::trim).filter(|u| !u.is_empty()) else {
                continue;
            };
            let pass = element.get("Pass").and_then(|p| p.as_str()).unwrap_or("").to_string();
            let load_mode = if Self::is_remote_url(url) { Self::LOAD_MODE_NETWORK_DOWNLOADABLE } else { fallback_load_mode };
            result.push(Entry { url: url.to_string(), password: pass, load_mode });
        }
        Ok(result)
    }

    fn is_remote_url(url: &str) -> bool {
        let lower = url.to_ascii_lowercase();
        lower.starts_with("http://") || lower.starts_with("https://")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_avatar_entries_only() {
        let json = br#"{"Value":{"Data":[
            {"Mode":0,"Url":"https://cdn.example/a.bee","Pass":"pw"},
            {"Mode":1,"Url":"https://cdn.example/scene.bee","Pass":"x"},
            {"Mode":0,"Url":"  ","Pass":"x"},
            {"Mode":0,"Url":"local-avatar","Pass":""}
        ]}}"#;
        let entries = AvatarKeyStoreLoader::parse(json, 7, Path::new("test.json")).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], Entry { url: "https://cdn.example/a.bee".into(), password: "pw".into(), load_mode: 0 });
        assert_eq!(entries[1].load_mode, 7);
        assert!(AvatarKeyStoreLoader::parse(b"{\"Value\":{}}", 1, Path::new("t")).unwrap().is_empty());
        assert!(AvatarKeyStoreLoader::parse(b"not json", 1, Path::new("t")).is_err());
    }
}
