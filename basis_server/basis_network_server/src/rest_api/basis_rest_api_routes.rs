//! Port of `RestApi/BasisRestApiRoutes.cs`: the `/api/*` routes as a pure dispatch over
//! (method, path, body), so they can be exercised without a socket.

use std::sync::Arc;

use basis_network_core::BNL;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::core::basis_server_control::{IServerControl, LoadStrategy, SwitchWorldParams, WorldLoadParams};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// The `Allow` header for a 405.
    pub allow: Option<String>,
}

impl ApiResponse {
    pub fn json(status: u16, json: impl Into<String>) -> Self {
        Self { status, body: json.into().into_bytes(), allow: None }
    }

    pub fn empty(status: u16) -> Self {
        Self { status, body: Vec::new(), allow: None }
    }

    pub fn body_str(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

pub struct BasisRestApiRoutes {
    control: Arc<dyn IServerControl>,
}

impl BasisRestApiRoutes {
    pub const MAX_BODY_BYTES: usize = 1 << 20;
    pub const MAX_MESSAGE_LENGTH: usize = 512;

    pub fn new(control: Arc<dyn IServerControl>) -> Self {
        Self { control }
    }

    fn quote(value: &str) -> String {
        serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
    }

    /// `segments` are the non-empty path parts (`["api", "worlds", "<id>"]`).
    pub fn dispatch(&self, method: &str, segments: &[&str], body: &[u8], cancellation: CancellationToken) -> ApiResponse {
        let resource = segments.get(1).copied().unwrap_or("");
        let id = segments.get(2).copied().unwrap_or("");
        let method = method.to_ascii_uppercase();
        match resource {
            "announce" => {
                if method != "POST" {
                    return Self::method_not_allowed("POST");
                }
                if id.is_empty() { self.announce_all(body) } else { self.announce_player(body, id) }
            }
            "players" => {
                if method != "GET" {
                    return Self::method_not_allowed("GET");
                }
                self.list_players()
            }
            "worlds" => {
                if id == "switch" {
                    return if method == "POST" { self.switch_world(body, cancellation) } else { Self::method_not_allowed("POST") };
                }
                match method.as_str() {
                    "GET" => self.list_worlds(),
                    "POST" => self.load_world(body),
                    "DELETE" => {
                        if id.is_empty() {
                            self.clear_all_worlds()
                        } else {
                            self.unload_world(id)
                        }
                    }
                    _ => Self::method_not_allowed("GET, POST, DELETE"),
                }
            }
            _ => Self::not_found("not found"),
        }
    }

    fn announce_all(&self, body: &[u8]) -> ApiResponse {
        let body = match Self::read_body(body) {
            Ok(body) => body,
            Err(response) => return response,
        };
        let msg = match Self::get_message(&body) {
            Ok(msg) => msg,
            Err(response) => return response,
        };
        self.control.announce_all(&msg);
        ApiResponse::json(200, r#"{"ok":true}"#)
    }

    fn announce_player(&self, body: &[u8], uuid: &str) -> ApiResponse {
        let body = match Self::read_body(body) {
            Ok(body) => body,
            Err(response) => return response,
        };
        let msg = match Self::get_message(&body) {
            Ok(msg) => msg,
            Err(response) => return response,
        };
        if !self.control.announce_player(uuid, &msg) {
            return Self::not_found("player not found");
        }
        ApiResponse::json(200, r#"{"ok":true}"#)
    }

    fn list_players(&self) -> ApiResponse {
        let entries: Vec<String> = self
            .control
            .list_players()
            .iter()
            .map(|p| {
                let position = match p.position {
                    Some(pos) => format!("[{},{},{}]", Self::float(pos[0]), Self::float(pos[1]), Self::float(pos[2])),
                    None => "null".to_string(),
                };
                format!(
                    "{{\"netId\":{},\"uuid\":{},\"displayName\":{},\"platform\":{},\"position\":{position}}}",
                    p.net_id,
                    Self::quote(&p.uuid),
                    Self::quote(&p.display_name),
                    Self::quote(&p.platform)
                )
            })
            .collect();
        ApiResponse::json(200, format!("{{\"players\":[{}]}}", entries.join(",")))
    }

    fn float(value: f32) -> String {
        serde_json::to_string(&value).unwrap_or_else(|_| "0".to_string())
    }

    fn list_worlds(&self) -> ApiResponse {
        let entries: Vec<String> = self
            .control
            .list_worlds()
            .iter()
            .map(|w| {
                format!(
                    "{{\"netId\":{},\"url\":{},\"persistent\":{},\"adminLocked\":{},\"strategy\":{}}}",
                    Self::quote(&w.net_id),
                    Self::quote(&w.url),
                    w.persistent,
                    w.admin_locked,
                    w.strategy
                )
            })
            .collect();
        ApiResponse::json(200, format!("{{\"worlds\":[{}]}}", entries.join(",")))
    }

    fn load_world(&self, body: &[u8]) -> ApiResponse {
        let body = match Self::read_body(body) {
            Ok(body) => body,
            Err(response) => return response,
        };
        let (url, password) = match Self::get_url_and_password(&body) {
            Ok(pair) => pair,
            Err(response) => return response,
        };
        let persistent = body.get("persistent").and_then(Value::as_bool).unwrap_or(false);
        let mut strategy = LoadStrategy::Immediate;
        if let Some(sp) = body.get("strategy") {
            if let Some(name) = sp.as_str() {
                strategy = match name {
                    "synchronized" => LoadStrategy::Synchronized,
                    "predownload" => LoadStrategy::Predownload,
                    "immediate" => LoadStrategy::Immediate,
                    _ => return Self::bad_request("unknown strategy"),
                };
            } else if let Some(n) = sp.as_u64().and_then(|n| u8::try_from(n).ok()) {
                strategy = match LoadStrategy::from_byte(n) {
                    Some(s) => s,
                    None => return Self::bad_request("unknown strategy"),
                };
            }
        }
        let net_id = self.control.load_world(&WorldLoadParams { url, password, persistent, strategy });
        ApiResponse::json(200, format!("{{\"ok\":true,\"netId\":{}}}", Self::quote(&net_id)))
    }

    fn unload_world(&self, net_id: &str) -> ApiResponse {
        if !self.control.unload_world(net_id) {
            return Self::not_found("world not found");
        }
        ApiResponse::json(200, r#"{"ok":true}"#)
    }

    fn clear_all_worlds(&self) -> ApiResponse {
        let count = self.control.clear_all_worlds();
        ApiResponse::json(200, format!("{{\"ok\":true,\"unloaded\":{count}}}"))
    }

    fn switch_world(&self, body: &[u8], cancellation: CancellationToken) -> ApiResponse {
        let body = match Self::read_body(body) {
            Ok(body) => body,
            Err(response) => return response,
        };
        let (url, password) = match Self::get_url_and_password(&body) {
            Ok(pair) => pair,
            Err(response) => return response,
        };
        let persistent = body.get("persistent").and_then(Value::as_bool).unwrap_or(false);
        let mut announce = String::new();
        if let Some(ap) = body.get("announceMessage") {
            let Some(text) = ap.as_str() else {
                return Self::bad_request("announceMessage must be a string");
            };
            if text.chars().count() > Self::MAX_MESSAGE_LENGTH {
                return Self::bad_request(&format!("announceMessage exceeds {} characters", Self::MAX_MESSAGE_LENGTH));
            }
            announce = text.to_string();
        }
        let mut delay = 0;
        if let Some(dp) = body.get("delay") {
            match dp.as_i64() {
                Some(n) if (0..=300).contains(&n) => delay = n as i32,
                _ if dp.is_null() => {}
                _ => return Self::bad_request("delay must be an integer 0–300 (seconds)"),
            }
        }
        let net_id = self.control.switch_world(&SwitchWorldParams { url, password, persistent, announce_message: announce, delay }, cancellation);
        ApiResponse::json(200, format!("{{\"ok\":true,\"netId\":{}}}", Self::quote(&net_id)))
    }

    // ── Parse helpers ──────────────────────────────────────────────────────

    fn get_message(body: &Value) -> Result<String, ApiResponse> {
        let Some(mp) = body.get("message") else {
            return Err(Self::bad_request("missing message"));
        };
        let Some(message) = mp.as_str() else {
            return Err(Self::bad_request("message must be a string"));
        };
        if message.is_empty() {
            return Err(Self::bad_request("message is empty"));
        }
        if message.chars().count() > Self::MAX_MESSAGE_LENGTH {
            return Err(Self::bad_request(&format!("message exceeds {} characters", Self::MAX_MESSAGE_LENGTH)));
        }
        Ok(message.to_string())
    }

    fn get_url_and_password(body: &Value) -> Result<(String, String), ApiResponse> {
        let Some(url_prop) = body.get("url") else {
            return Err(Self::bad_request("missing url"));
        };
        let Some(raw) = url_prop.as_str() else {
            return Err(Self::bad_request("url must be a string"));
        };
        let (raw_url, embedded) = Self::split_url_fragment(raw);
        let mut password: Option<String> = None;
        if let Some(pass_prop) = body.get("password") {
            let Some(pw) = pass_prop.as_str() else {
                return Err(Self::bad_request("password must be a string"));
            };
            password = Some(pw.to_string());
        }
        let password = password.or(embedded);
        if raw_url.is_empty() {
            return Err(Self::bad_request("url must not be empty"));
        }
        if !Self::is_https_url(&raw_url) {
            return Err(Self::bad_request("url must use https://"));
        }
        let Some(password) = password.filter(|p| !p.is_empty()) else {
            return Err(Self::bad_request("password required (provide password field or embed in url as #fragment)"));
        };
        Ok((raw_url, password))
    }

    pub fn split_url_fragment(raw: &str) -> (String, Option<String>) {
        let raw = raw.trim();
        if let Some(idx) = raw.find('#') {
            return (raw[..idx].to_string(), Self::decode_fragment_password(&raw[idx + 1..]));
        }
        if let Some(idx) = raw.to_ascii_lowercase().find("%23") {
            return (raw[..idx].to_string(), Self::decode_fragment_password(&raw[idx + 3..]));
        }
        (raw.to_string(), None)
    }

    fn decode_fragment_password(fragment: &str) -> Option<String> {
        if fragment.is_empty() {
            return None;
        }
        use base64::Engine;
        match base64::engine::general_purpose::STANDARD.decode(fragment.as_bytes()) {
            Ok(bytes) => Some(String::from_utf8(bytes).unwrap_or_else(|_| fragment.to_string())),
            Err(_) => Some(fragment.to_string()),
        }
    }

    /// An absolute `https://` URL with a host.
    pub fn is_https_url(url: &str) -> bool {
        let lower = url.to_ascii_lowercase();
        let Some(rest) = lower.strip_prefix("https://") else {
            return false;
        };
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        !host.is_empty() && !host.chars().any(char::is_whitespace)
    }

    // ── Response helpers ───────────────────────────────────────────────────

    fn read_body(body: &[u8]) -> Result<Value, ApiResponse> {
        if body.len() > Self::MAX_BODY_BYTES {
            return Err(ApiResponse::json(413, r#"{"error":"payload too large"}"#));
        }
        let text = String::from_utf8_lossy(body);
        if text.trim().is_empty() {
            return Ok(Value::Object(serde_json::Map::new()));
        }
        serde_json::from_str::<Value>(&text).map_err(|_| Self::bad_request("invalid JSON body"))
    }

    pub fn bad_request(msg: &str) -> ApiResponse {
        ApiResponse::json(400, format!("{{\"error\":{}}}", Self::quote(msg)))
    }

    pub fn not_found(msg: &str) -> ApiResponse {
        ApiResponse::json(404, format!("{{\"error\":{}}}", Self::quote(msg)))
    }

    pub fn method_not_allowed(allow: &str) -> ApiResponse {
        ApiResponse { status: 405, body: Vec::new(), allow: Some(allow.to_string()) }
    }

    pub fn internal_error() -> ApiResponse {
        BNL::log_error("REST API handler error");
        ApiResponse::json(500, r#"{"error":"internal server error"}"#)
    }
}
