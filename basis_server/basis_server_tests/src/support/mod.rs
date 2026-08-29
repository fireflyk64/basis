//! Shared test support: a real server on a loopback port, polling waits, a recording fake peer
//! and the avatar delta helpers the C# `DeltaTestSupport` provided.

pub mod delta_test_support;
pub mod fake_peer;
pub mod lifecycle;

pub use delta_test_support::DeltaTestSupport;
pub use fake_peer::FakePeer;
pub use lifecycle::{FakeAuth, FakeNetManager, LifecycleSupport, MapAuthIdentity, RecordingConnectionRequest, ServerStaticsScope};

use std::time::{Duration, Instant};

use basis_network_core::configuration::{BasisTransportConfigStore, Configuration, LNLTransportConfig};
use basis_network_core::transport::basis_network_stack_registry::BasisNetworkStackRegistry;
use basis_network_core::transport::MixedNetManager;
use basis_network_server::NetworkServer;
use basis_network_server::core::basis_server_handle_events::BasisServerHandleEvents;

/// Boots one real server for a test, on the mixed stack: iroh clients connect with
/// [`connection_string`](Self::connection_string), legacy LiteNetLib clients with
/// [`legacy_address`](Self::legacy_address). Started per test because a boot is cheap (two
/// socket binds) and isolation is worth more than the milliseconds.
pub struct HelloWorldServerFixture {
    connection_string: String,
    legacy_port: u16,
}

impl HelloWorldServerFixture {
    pub const PASSWORD: &'static str = "hello-world-integration-test";
    /// The legacy transport's disconnect timeout for tests: long enough for a slow box, short
    /// enough that a test which kills a client can watch the server notice.
    pub const LEGACY_DISCONNECT_TIMEOUT_MS: i32 = 4000;

    pub fn new() -> Self {
        // The legacy sidecar ships a 30 s disconnect timeout; the tests that drop a client
        // without a goodbye would wait that long for the server to notice.
        let mut lnl = BasisTransportConfigStore::get::<LNLTransportConfig>(BasisNetworkStackRegistry::LITE_NET_LIB_ID);
        lnl.disconnect_timeout = Self::LEGACY_DISCONNECT_TIMEOUT_MS;
        BasisTransportConfigStore::set(BasisNetworkStackRegistry::LITE_NET_LIB_ID, lnl);
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
        let mixed = server.as_any().downcast_ref::<MixedNetManager>().expect("the server runs on the mixed stack");
        Self { connection_string: mixed.connection_string(), legacy_port: mixed.legacy_port() }
    }

    /// `<endpoint-id>@127.0.0.1:port` — what an iroh client passes to connect.
    pub fn connection_string(&self) -> &str {
        &self.connection_string
    }

    /// The UDP port legacy LiteNetLib clients connect to.
    pub fn legacy_port(&self) -> u16 {
        self.legacy_port
    }

    /// `127.0.0.1:port` — what a legacy client passes to connect.
    pub fn legacy_address(&self) -> String {
        format!("127.0.0.1:{}", self.legacy_port)
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
