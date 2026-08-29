//! Shared test support: a real server on a loopback port, polling waits, a recording fake peer
//! and the avatar delta helpers the C# `DeltaTestSupport` provided.

pub mod delta_test_support;
pub mod fake_peer;

pub use delta_test_support::DeltaTestSupport;
pub use fake_peer::FakePeer;

use std::time::{Duration, Instant};

use basis_network_core::configuration::Configuration;
use basis_network_core::transport::IrohNetManager;
use basis_network_server::NetworkServer;
use basis_network_server::core::basis_server_handle_events::BasisServerHandleEvents;

/// Boots one real server for a test. Started per test because a boot is cheap on iroh (one
/// endpoint bind) and isolation is worth more than the milliseconds.
pub struct HelloWorldServerFixture {
    connection_string: String,
}

impl HelloWorldServerFixture {
    pub const PASSWORD: &'static str = "hello-world-integration-test";

    pub fn new() -> Self {
        let configuration = Configuration {
            set_port: 0,
            password: Self::PASSWORD.to_string(),
            use_auth: true,
            use_auth_identity: true,
            // Keeps the run from writing config.xml, permissions.xml and the allow/ban lists into
            // the test binary's folder. Everything the test needs lives in memory.
            has_file_support: false,
            enable_statistics: false,
            enable_console: false,
            api_enabled: false,
            peer_limit: 64,
            ..Configuration::default()
        };
        NetworkServer::start_server(configuration).unwrap_or_else(|e| panic!("the server did not start: {}", e.report()));
        let server = NetworkServer::server().expect("a started server has a transport");
        let iroh = server.as_any().downcast_ref::<IrohNetManager>().expect("the server runs on the iroh stack");
        Self { connection_string: iroh.connection_string() }
    }

    /// `<endpoint-id>@127.0.0.1:port` — what a client passes to connect.
    pub fn connection_string(&self) -> &str {
        &self.connection_string
    }
}

impl Default for HelloWorldServerFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for HelloWorldServerFixture {
    fn drop(&mut self) {
        // stop_worker first: it unsubscribes the handlers, which reads the server's listener —
        // and stop_server clears it.
        BasisServerHandleEvents::stop_worker();
        NetworkServer::stop_server();
    }
}

/// Polls `condition` until it holds or `timeout` passes, failing with `describe_failure`.
pub fn wait_until(condition: impl Fn() -> bool, timeout: Duration, describe_failure: impl Fn() -> String) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("Timed out after {}s: {}", timeout.as_secs(), describe_failure());
}

/// Reads a scale override, so the same test can be turned up on a machine that has the room
/// without committing a size that fails everywhere else.
pub fn read_scale(variable: &str, fallback: usize, min: usize) -> usize {
    std::env::var(variable).ok().and_then(|raw| raw.trim().parse::<usize>().ok()).map(|v| v.max(min)).unwrap_or(fallback)
}
