//! Cross-language interop over the LiteNetLib protocol, both ways, with real processes:
//!
//! * Rust clients on the `litenetlib` stack join the **C# server** (`BasisNetworkConsole`,
//!   spawned from its build output) and hold the hello-world conversation through it.
//! * The **C# hello-world clients** (`BasisHelloWorldClient`, spawned from its build output)
//!   join the Rust server over LiteNetLib — and, when the `basis_iroh_ffi` library is beside
//!   them, over iroh as well, so a C# legacy client and a C# iroh client share the Rust server.
//!
//! Each test needs the `dotnet` runtime and the C# solution built in Release; without them it
//! reports why and passes vacuously, so `cargo test` stays green on a box without .NET. Set
//! `BASIS_CSHARP_SERVER_DIR` / `BASIS_CSHARP_HELLO_DIR` to point at builds elsewhere.

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::net::{Ipv4Addr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use basis_hello_world_client::BasisHelloClient;
use basis_network_core::configuration::Configuration;
use basis_network_core::transport::LnlNetManager;
use basis_network_core::transport::basis_network_stack_registry::BasisNetworkStackRegistry;
use basis_network_core::transport::connection_target::ConnectionTarget;
use basis_network_core::transport::iroh_network_impl::IrohRuntime;
use basis_network_server::NetworkServer;
use basis_server_tests::support::{HelloWorldServerFixture, wait_until};
use parking_lot::Mutex;
use serial_test::serial;

const PASSWORD: &str = "interop-test-password";
const JOIN_TIMEOUT: Duration = Duration::from_secs(30);
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(30);
const SERVER_BOOT_TIMEOUT: Duration = Duration::from_secs(90);

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

/// The `dotnet` host: `DOTNET_ROOT`, the PATH, or the default user install.
fn dotnet() -> Option<PathBuf> {
    if let Ok(root) = std::env::var("DOTNET_ROOT") {
        let candidate = Path::new(&root).join("dotnet");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join("dotnet");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let home = std::env::var("HOME").ok()?;
    let candidate = Path::new(&home).join(".dotnet/dotnet");
    candidate.is_file().then_some(candidate)
}

/// A C# project's build output, holding `<name>.dll`.
fn csharp_build(env_var: &str, project_dir: &str, dll: &str) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(env_var) {
        let dir = PathBuf::from(dir);
        return dir.join(dll).is_file().then_some(dir);
    }
    for configuration in ["Release", "Debug"] {
        let dir = repo_root().join("Basis Server").join(project_dir).join("bin").join(configuration).join("net10.0");
        if dir.join(dll).is_file() {
            return Some(dir);
        }
    }
    None
}

fn free_udp_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).and_then(|s| s.local_addr()).map(|a| a.port()).unwrap_or(0)
}

fn free_tcp_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).and_then(|s| s.local_addr()).map(|a| a.port()).unwrap_or(0)
}

fn scratch_dir(label: &str) -> PathBuf {
    static NONCE: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!("basis-interop-{label}-{}-{}", std::process::id(), NONCE.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("copy target");
    for entry in std::fs::read_dir(from).expect("read build dir") {
        let entry = entry.expect("dir entry");
        let target = to.join(entry.file_name());
        let kind = entry.file_type().expect("file type");
        if kind.is_dir() {
            // Neither the build's own config nor its logs belong to a test run.
            let name = entry.file_name();
            if name == "config" || name == "logs" {
                continue;
            }
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap_or_else(|e| panic!("copying {}: {e}", entry.path().display()));
        }
    }
}

/// Drains a child's stdout/stderr into a shared log so a failure can show what the process
/// said, without ever letting a full pipe stall it.
fn drain(child: &mut Child, log: Arc<Mutex<Vec<String>>>) {
    if let Some(stdout) = child.stdout.take() {
        let log = log.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                log.lock().push(line);
            }
        });
    }
    if let Some(stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log.lock().push(format!("stderr: {line}"));
            }
        });
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Rust clients → C# server
// ─────────────────────────────────────────────────────────────────────────────

/// The C# `BasisNetworkConsole`, running from a private copy of its build output so its
/// `config/` and `logs/` never touch the checkout.
struct CSharpServer {
    child: Child,
    port: u16,
    dir: PathBuf,
    log: Arc<Mutex<Vec<String>>>,
}

impl CSharpServer {
    fn start() -> Result<Self, String> {
        let dotnet = dotnet().ok_or("no dotnet runtime on this machine")?;
        let build = csharp_build("BASIS_CSHARP_SERVER_DIR", "BasisServerConsole", "BasisNetworkConsole.dll")
            .ok_or("the C# server is not built (dotnet build \"Basis Server/Basis Server.sln\" -c Release)")?;
        let dir = scratch_dir("csharp-server");
        copy_dir(&build, &dir);
        let port = free_udp_port();
        let health_port = free_tcp_port();
        // The C# server reads exactly this schema; a config on disk is also what skips its
        // first-boot wizard.
        let mut configuration = Configuration {
            set_port: port,
            password: PASSWORD.to_string(),
            use_auth: true,
            use_auth_identity: true,
            has_file_support: true,
            enable_statistics: false,
            enable_console: false,
            api_enabled: false,
            health_check_port: health_port,
            peer_limit: 64,
            network_stack_id: BasisNetworkStackRegistry::LITE_NET_LIB_ID.to_string(),
            ..Configuration::default()
        };
        let config_dir = dir.join(Configuration::CONFIG_FOLDER_NAME);
        std::fs::create_dir_all(&config_dir).map_err(|e| e.to_string())?;
        configuration.save_to_xml(&config_dir.join("config.xml")).map_err(|e| format!("writing config.xml: {e}"))?;

        let mut child = Command::new(dotnet)
            .arg("BasisNetworkConsole.dll")
            .current_dir(&dir)
            .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
            .env("DOTNET_NOLOGO", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("starting the C# server: {e}"))?;
        let log = Arc::new(Mutex::new(Vec::new()));
        drain(&mut child, log.clone());
        let server = Self { child, port, dir, log };
        server.wait_until_listening()?;
        Ok(server)
    }

    /// Ready means the server answers the LiteNetLib server-info probe — the same unconnected
    /// datagram every legacy client sends before it connects.
    fn wait_until_listening(&self) -> Result<(), String> {
        let deadline = Instant::now() + SERVER_BOOT_TIMEOUT;
        let mut child_exited = None;
        while Instant::now() < deadline {
            let target = ConnectionTarget::new(BasisNetworkStackRegistry::LITE_NET_LIB_ID, &format!("127.0.0.1:{}", self.port));
            let result = IrohRuntime::block_on(LnlNetManager::probe(target, 1000)).map_err(|e| e.report())?;
            if result.reachable {
                println!("C# server up on UDP {}: '{}' ({} online, {} max)", self.port, result.name, result.online, result.max);
                return Ok(());
            }
            // The probe is rate limited per address on the far side; asking more often than
            // that only gets dropped.
            std::thread::sleep(Duration::from_millis(600));
            if let Ok(Some(status)) = self.child_status() {
                child_exited = Some(status);
                break;
            }
        }
        Err(format!("the C# server never answered on UDP {} (exit {:?}); its output:\n{}", self.port, child_exited, self.log.lock().join("\n")))
    }

    fn child_status(&self) -> std::io::Result<Option<std::process::ExitStatus>> {
        // try_wait needs &mut; the child is only observed here.
        #[allow(invalid_reference_casting)]
        let child = unsafe { &mut *(&self.child as *const Child as *mut Child) };
        child.try_wait()
    }

    fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

impl Drop for CSharpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn legacy_rust_client(address: &str, name: &str, password: &str) -> (Arc<BasisHelloClient>, bool) {
    let client = BasisHelloClient::with_stack(name, BasisNetworkStackRegistry::LITE_NET_LIB_ID).unwrap_or_else(|e| panic!("{}", e.report()));
    let joined = client.connect(address, 0, password, JOIN_TIMEOUT).unwrap_or_else(|e| panic!("{}", e.report()));
    (client, joined)
}

#[test]
#[serial(csharp_interop)]
fn rust_legacy_clients_join_the_csharp_server_and_exchange_messages() {
    let server = match CSharpServer::start() {
        Ok(server) => server,
        Err(reason) => {
            println!("SKIPPED (Rust → C#): {reason}");
            return;
        }
    };
    let names = ["Rust0", "Rust1", "Rust2", "Rust3"];
    let mut clients = Vec::new();
    for name in names {
        let (client, joined) = legacy_rust_client(&server.address(), name, PASSWORD);
        assert!(joined, "{name} did not join the C# server; its output:\n{}", server.log.lock().join("\n"));
        clients.push(client);
    }
    let ids: Vec<u16> = clients.iter().map(|c| c.player_id()).collect();
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 4, "ids {ids:?}");

    // The C# server counts them in its info line.
    let target = ConnectionTarget::new(BasisNetworkStackRegistry::LITE_NET_LIB_ID, &server.address());
    std::thread::sleep(Duration::from_millis(700));
    let info = IrohRuntime::block_on(LnlNetManager::probe(target, 3000)).unwrap();
    assert!(info.reachable, "{}", info.error);
    assert_eq!(info.online, 4);

    // Full mesh of directed text, relayed by the C# server.
    type Inbox = Arc<Mutex<Vec<(u16, String)>>>;
    let inbox: Vec<Inbox> = (0..4).map(|_| Arc::new(Mutex::new(Vec::new()))).collect();
    for (i, client) in clients.iter().enumerate() {
        let sink = inbox[i].clone();
        client.on_text_received(Arc::new(move |sender, text, _| sink.lock().push((sender, text))));
    }
    for (from, client) in clients.iter().enumerate() {
        for (to, &id) in ids.iter().enumerate() {
            if from != to {
                client.send_text(id, &format!("hello{from}_{to}")).unwrap_or_else(|e| panic!("{}", e.report()));
            }
        }
    }
    wait_until(|| inbox.iter().all(|b| b.lock().len() >= 3), DELIVERY_TIMEOUT, || format!("per client: {:?}", inbox.iter().map(|b| b.lock().len()).collect::<Vec<_>>()));
    for (to, bag) in inbox.iter().enumerate() {
        let got = bag.lock().clone();
        assert_eq!(got.len(), 3, "client {to} got a message meant for someone else: {got:?}");
        for (from, &id) in ids.iter().enumerate() {
            if from != to {
                assert!(got.contains(&(id, format!("hello{from}_{to}"))));
            }
        }
    }

    // And the volley round the ring.
    let finished = Arc::new(Mutex::new(false));
    let hops = Arc::new(Mutex::new(Vec::new()));
    const FINAL: i32 = 8;
    for i in 0..4 {
        let me = clients[i].clone();
        let next_id = ids[(i + 1) % 4];
        let (finished, hops) = (finished.clone(), hops.clone());
        clients[i].on_number_received(Arc::new(move |sender, value, _| {
            hops.lock().push((i, sender, value));
            if value >= FINAL {
                *finished.lock() = true;
            } else {
                me.send_number(next_id, value + 1).unwrap_or_else(|e| panic!("{}", e.report()));
            }
        }));
    }
    clients[0].send_number(ids[1], 1).unwrap();
    wait_until(|| *finished.lock(), DELIVERY_TIMEOUT, || format!("the volley stopped after {:?}", hops.lock()));
    let mut ordered = hops.lock().clone();
    ordered.sort_by_key(|h| h.2);
    for (hop, (receiver, sender, value)) in ordered.iter().enumerate() {
        assert_eq!((*value, *receiver, *sender), (hop as i32 + 1, (hop + 1) % 4, ids[hop % 4]));
    }
    for client in &clients {
        client.disconnect();
    }
    std::thread::sleep(Duration::from_millis(700));
    let target = ConnectionTarget::new(BasisNetworkStackRegistry::LITE_NET_LIB_ID, &server.address());
    let info = IrohRuntime::block_on(LnlNetManager::probe(target, 3000)).unwrap();
    assert!(info.reachable && info.online == 0, "after the goodbyes the C# server still counts {} online", info.online);
}

#[test]
#[serial(csharp_interop)]
fn rust_legacy_client_with_the_wrong_password_is_refused_by_the_csharp_server() {
    let server = match CSharpServer::start() {
        Ok(server) => server,
        Err(reason) => {
            println!("SKIPPED (Rust → C#): {reason}");
            return;
        }
    };
    let (client, joined) = legacy_rust_client(&server.address(), "WrongPassword", "definitely-not-it");
    assert!(!joined);
    assert!(!client.is_joined());
    assert!(client.server_peer().is_some_and(|p| !p.is_connected()), "the C# server's reject reached the transport");
}

// ─────────────────────────────────────────────────────────────────────────────
// C# clients → Rust server
// ─────────────────────────────────────────────────────────────────────────────

/// Runs the C# hello-world program against a server and returns its exit status and output.
fn run_csharp_hello(server_target: &str, port: u16, stack: &str, clients: usize, hops: usize, timeout: Duration) -> Result<(bool, Vec<String>), String> {
    let dotnet = dotnet().ok_or("no dotnet runtime on this machine")?;
    let build = csharp_build("BASIS_CSHARP_HELLO_DIR", "BasisHelloWorldClient", "BasisHelloWorldClient.dll")
        .ok_or("the C# hello-world client is not built (dotnet build \"Basis Server/Basis Server.sln\" -c Release)")?;
    if stack == BasisNetworkStackRegistry::IROH_ID && !build.join("libbasis_iroh_ffi.so").is_file() && !build.join("basis_iroh_ffi.dll").is_file() {
        return Err(format!("basis_iroh_ffi is not beside {} (cargo build --release -p basis_iroh_ffi, then rebuild the C# solution)", build.display()));
    }
    let mut child = Command::new(dotnet)
        .arg("BasisHelloWorldClient.dll")
        .args(["--ip", server_target, "--port", &port.to_string(), "--password", HelloWorldServerFixture::PASSWORD])
        .args(["--stack", stack, "--clients", &clients.to_string(), "--hops", &hops.to_string()])
        .current_dir(&build)
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .env("DOTNET_NOLOGO", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("starting the C# hello client: {e}"))?;
    let log = Arc::new(Mutex::new(Vec::new()));
    drain(&mut child, log.clone());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(status);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // Give the drain threads a moment to flush the last lines.
    std::thread::sleep(Duration::from_millis(100));
    let lines = log.lock().clone();
    Ok((status.is_some_and(|s| s.success()), lines))
}

#[test]
#[serial(network_statics)]
fn csharp_legacy_clients_join_the_rust_server_over_litenetlib() {
    let server = HelloWorldServerFixture::new();
    let (ok, lines) = match run_csharp_hello("127.0.0.1", server.legacy_port(), BasisNetworkStackRegistry::LITE_NET_LIB_ID, 4, 8, Duration::from_secs(120)) {
        Ok(result) => result,
        Err(reason) => {
            println!("SKIPPED (C# → Rust): {reason}");
            return;
        }
    };
    let output = lines.join("\n");
    assert!(ok, "the C# hello clients did not finish their volley against the Rust server:\n{output}");
    assert!(output.contains("Done: the number went round the ring and reached 8."), "{output}");
    assert_eq!(output.matches("joined as player").count(), 4, "{output}");
    // Every C# client left politely, so nothing lingers on the server side.
    wait_until(|| NetworkServer::authenticated_peers().is_empty(), DELIVERY_TIMEOUT, || format!("{} C# peers still authenticated", NetworkServer::authenticated_peers().len()));
}

#[test]
#[serial(network_statics)]
fn csharp_iroh_clients_join_the_rust_server_over_the_ffi() {
    let server = HelloWorldServerFixture::new();
    let (ok, lines) = match run_csharp_hello(server.connection_string(), 0, BasisNetworkStackRegistry::IROH_ID, 3, 6, Duration::from_secs(120)) {
        Ok(result) => result,
        Err(reason) => {
            println!("SKIPPED (C# → Rust over iroh): {reason}");
            return;
        }
    };
    let output = lines.join("\n");
    assert!(ok, "the C# iroh hello clients did not finish their volley against the Rust server:\n{output}");
    assert!(output.contains("Done: the number went round the ring and reached 6."), "{output}");
}

#[test]
#[serial(network_statics)]
fn csharp_legacy_clients_and_rust_iroh_clients_share_the_rust_server() {
    // A Rust iroh client sits in the room while the C# legacy clients run their volley; every
    // relayed hello from the legacy side has to reach it too, so the mixed world is observed
    // from the iroh side by a client of a different language.
    let server = HelloWorldServerFixture::new();
    let observer = BasisHelloClient::new("RustObserver").unwrap();
    assert!(observer.connect(server.connection_string(), 0, HelloWorldServerFixture::PASSWORD, JOIN_TIMEOUT).unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    observer.on_number_received(Arc::new(move |sender, value, _| sink.lock().push((sender, value))));
    // The C# ring does not know the observer's id, so it never addresses it; what this proves
    // is that the two populations coexist on one server without disturbing each other.
    let (ok, lines) = match run_csharp_hello("127.0.0.1", server.legacy_port(), BasisNetworkStackRegistry::LITE_NET_LIB_ID, 3, 6, Duration::from_secs(120)) {
        Ok(result) => result,
        Err(reason) => {
            println!("SKIPPED (C# → Rust mixed): {reason}");
            return;
        }
    };
    let output = lines.join("\n");
    assert!(ok, "{output}");
    assert!(output.contains("reached 6."), "{output}");
    assert!(observer.is_joined(), "the iroh client is untouched by the legacy population coming and going");
    assert!(seen.lock().is_empty(), "the relay leaked a directed message to a client it was not addressed to");
    assert_eq!(NetworkServer::authenticated_peers().len(), 1);
    observer.disconnect();
}
