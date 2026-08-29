//! The wire contract between the benchmark and a remote load-generating machine.
//!
//! Line-delimited JSON over TCP: one request per line, one response per line. Deliberately the
//! dullest thing that works: inspectable with netcat, no framing subtleties, and the traffic is a
//! handful of messages per run.

use serde::{Deserialize, Serialize};

pub struct BenchAgentProtocol;

impl BenchAgentProtocol {
    /// Default TCP control port. **Not 4296**: that is the server's game port, and the load
    /// clients on this very machine will be talking to it.
    pub const DEFAULT_PORT: u16 = 4297;
    /// Bumped when the message shapes change; a mismatched pair refuses rather than guesses.
    pub const VERSION: i32 = 1;
}

fn default_version() -> i32 {
    BenchAgentProtocol::VERSION
}

fn default_connect_interval() -> i32 {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentRequest {
    #[serde(rename = "cmd", default)]
    pub command: String,
    #[serde(default = "default_version")]
    pub version: i32,
    /// How many simulated clients to run.
    #[serde(default)]
    pub clients: i32,
    /// The server they should connect to, as seen from the agent's machine.
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub port: i32,
    /// Delay between starting each client; 0 is the thundering herd a restart produces.
    #[serde(rename = "connectIntervalMs", default = "default_connect_interval")]
    pub connect_interval_ms: i32,
}

impl Default for AgentRequest {
    fn default() -> Self {
        Self { command: String::new(), version: BenchAgentProtocol::VERSION, clients: 0, host: String::new(), port: 0, connect_interval_ms: 1 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default = "default_version")]
    pub version: i32,

    // ── hello ──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default)]
    pub cores: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,

    // ── status ──
    #[serde(default)]
    pub running: bool,
    /// Load-client CPU on the AGENT's machine, in cores. Reported so the benchmark can tell
    /// whether the load generator itself ran out of capacity.
    #[serde(rename = "clientCores", default)]
    pub client_cores: f64,
    /// Share of simulated voice frames a receiver actually got, or -1 when unknown.
    #[serde(rename = "voiceDelivered", default)]
    pub voice_delivered: f64,
}

impl Default for AgentResponse {
    fn default() -> Self {
        Self { ok: false, error: None, version: BenchAgentProtocol::VERSION, agent: None, cores: 0, os: None, running: false, client_cores: 0.0, voice_delivered: -1.0 }
    }
}

impl AgentResponse {
    pub fn error(message: impl Into<String>) -> Self {
        Self { ok: false, error: Some(message.into()), ..Self::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_uses_wire_names_and_defaults() {
        let request: AgentRequest = serde_json::from_str(r#"{"cmd":"start","clients":10,"host":"h","port":4296}"#).unwrap();
        assert_eq!(request.command, "start");
        assert_eq!(request.version, BenchAgentProtocol::VERSION);
        assert_eq!(request.connect_interval_ms, 1);
        let response = AgentResponse { ok: true, running: true, client_cores: 1.5, ..AgentResponse::default() };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"clientCores\":1.5"));
        assert!(json.contains("\"voiceDelivered\":-1.0"));
        assert!(!json.contains("\"error\""));
    }
}
