use std::collections::HashMap;

/// Keys the parsers agree on.
pub struct ConnectionTargetKeys;

impl ConnectionTargetKeys {
    pub const ADDRESS: &'static str = "address";
    pub const PORT: &'static str = "port";
    pub const PASSWORD: &'static str = "password";
    pub const LOBBY_ID: &'static str = "lobbyId";
    /// iroh: the server's endpoint id (z-base-32 public key).
    pub const ENDPOINT_ID: &'static str = "endpointId";
    /// iroh: a relay URL to reach the endpoint through.
    pub const RELAY_URL: &'static str = "relayUrl";
}

/// A stack id plus the raw connection string a user typed, and the properties the stack's
/// parser pulled out of it. Keys are case-insensitive.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionTarget {
    pub stack_id: String,
    pub raw: String,
    properties: HashMap<String, (String, String)>,
}

impl ConnectionTarget {
    pub fn new(stack_id: &str, raw: &str) -> Self {
        Self { stack_id: stack_id.to_string(), raw: raw.to_string(), properties: HashMap::new() }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.get_or(key, None)
    }

    pub fn get_or(&self, key: &str, fallback: Option<&str>) -> Option<String> {
        if key.is_empty() {
            return fallback.map(str::to_string);
        }
        match self.properties.get(&key.to_ascii_lowercase()) {
            Some((_, v)) => Some(v.clone()),
            None => fallback.map(str::to_string),
        }
    }

    pub fn try_get(&self, key: &str) -> Option<String> {
        if key.is_empty() {
            return None;
        }
        self.properties.get(&key.to_ascii_lowercase()).map(|(_, v)| v.clone())
    }

    pub fn set(&mut self, key: &str, value: &str) {
        if key.is_empty() {
            return;
        }
        self.properties.insert(key.to_ascii_lowercase(), (key.to_string(), value.to_string()));
    }

    /// The property bag, keyed by the name each key was first set under.
    pub fn properties(&self) -> HashMap<String, String> {
        self.properties.values().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    pub fn property_count(&self) -> usize {
        self.properties.len()
    }
}

pub trait IConnectionTargetParser: Send + Sync {
    fn parse(&self, target: &mut ConnectionTarget);
    fn format(&self, target: &ConnectionTarget) -> String;
}
