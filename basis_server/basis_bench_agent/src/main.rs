//! Port of `BasisBenchAgent`: runs the crowd on a machine that is not the server's.
//!
//! On a single box the load clients and the server compete for the same cores, cache and memory
//! bandwidth, and the traffic never crosses a NIC. Both problems have the same fix: put the crowd
//! somewhere else. This is the somewhere else. It owns nothing and decides nothing; the benchmark
//! still runs the experiment, and this just starts, stops and reports on load clients when told.
//!
//! Deliberately tiny: one socket, one child process, and a status reply built from counters that
//! were already being kept.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]
#![deny(unused_must_use)]
// LaunchTarget mirrors the C# helper's full surface; the agent uses a subset of it.
#![allow(dead_code)]

mod bench_agent_protocol;
mod launch_target;

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bench_agent_protocol::{AgentRequest, AgentResponse, BenchAgentProtocol};
use launch_target::LaunchTarget;

const LOAD_CLIENT_NAMES: [&str; 2] = ["basis_network_client_console", "BasisNetworkClientConsole"];

struct Running {
    child: Child,
    cpu: ProcessCpuSampler,
}

struct Agent {
    client_directory: PathBuf,
    running: Mutex<Option<Running>>,
    /// Share of simulated voice frames a receiver actually got, or -1 when unknown (f64 bits).
    voice_delivered: AtomicU64,
}

impl Agent {
    fn set_voice_delivered(&self, value: f64) {
        self.voice_delivered.store(value.to_bits(), Ordering::Relaxed);
    }

    fn voice_delivered(&self) -> f64 {
        f64::from_bits(self.voice_delivered.load(Ordering::Relaxed))
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut port = BenchAgentProtocol::DEFAULT_PORT;
    let mut bind = "0.0.0.0".to_string();
    let mut client_directory = String::new();

    let mut i = 0;
    while i < args.len() {
        let next = |i: &mut usize| -> Option<String> {
            if *i + 1 < args.len() {
                *i += 1;
                Some(args[*i].clone())
            } else {
                None
            }
        };
        match args[i].as_str() {
            "--port" => {
                if let Some(v) = next(&mut i).and_then(|v| v.parse::<u16>().ok()) {
                    port = v;
                }
            }
            "--bind" => bind = next(&mut i).unwrap_or(bind),
            "--client" => client_directory = next(&mut i).unwrap_or_default(),
            "--help" | "-h" => {
                print_usage();
                return std::process::ExitCode::SUCCESS;
            }
            _ => {}
        }
        i += 1;
    }

    let client_directory = if client_directory.is_empty() { discover_client_directory() } else { PathBuf::from(client_directory) };
    if find_load_client(&client_directory).is_none() {
        eprintln!("No {} under '{}'. Pass --client <dir>.", LOAD_CLIENT_NAMES[0], client_directory.display());
        return std::process::ExitCode::from(1);
    }

    let listener = match TcpListener::bind((bind.as_str(), port)) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("Could not listen on {bind}:{port}: {e}");
            return std::process::ExitCode::from(1);
        }
    };

    println!("Basis benchmark agent listening on {bind}:{port}");
    println!("  load client: {}", client_directory.display());
    println!("  {} cores, {}", num_cores(), runtime_os());
    println!("  Ctrl-C to stop. Any running load clients are stopped with it.");

    let agent = Arc::new(Agent { client_directory, running: Mutex::new(None), voice_delivered: AtomicU64::new((-1.0f64).to_bits()) });

    // Any running load clients are stopped with the agent.
    let on_exit = agent.clone();
    let _ = ctrlc_handler(move || {
        stop_client(&on_exit);
        std::process::exit(0);
    });

    for connection in listener.incoming() {
        match connection {
            Ok(stream) => serve(&agent, stream),
            Err(e) => eprintln!("  connection failed: {e}"),
        }
    }
    std::process::ExitCode::SUCCESS
}

/// Serves one connection until it closes. One at a time, on the accepting thread: two benchmarks
/// driving the same crowd would each think they owned it.
fn serve(agent: &Arc<Agent>, stream: TcpStream) {
    let remote = stream.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| "?".into());
    println!("  connected: {remote}");
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(e) => {
            eprintln!("  connection failed: {e}");
            return;
        }
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<AgentRequest>(&line) {
            Ok(request) => handle(agent, request),
            Err(e) => AgentResponse::error(format!("unparseable request: {e}")),
        };
        let mut encoded = serde_json::to_string(&response).unwrap_or_else(|_| "{\"ok\":false,\"error\":\"unserializable response\"}".to_string());
        encoded.push('\n');
        if writer.write_all(encoded.as_bytes()).is_err() {
            break;
        }
    }
    println!("  disconnected");
    // A benchmark that dies mid-run must not leave a thousand clients hammering the server with
    // nothing owning them. The control connection closing is the only signal available for that.
    stop_client(agent);
}

fn handle(agent: &Arc<Agent>, request: AgentRequest) -> AgentResponse {
    if request.version != BenchAgentProtocol::VERSION {
        return AgentResponse::error(format!(
            "protocol version {} against this agent's {}. Update whichever side is older - a mismatched pair refuses rather than guessing.",
            request.version,
            BenchAgentProtocol::VERSION
        ));
    }
    match request.command.to_lowercase().as_str() {
        "hello" => AgentResponse { ok: true, agent: Some("BasisBenchAgent".into()), cores: num_cores(), os: Some(runtime_os()), ..AgentResponse::default() },
        "start" => start_client(agent, &request),
        "status" => {
            let mut running = agent.running.lock().unwrap_or_else(|p| p.into_inner());
            let (is_running, client_cores) = match running.as_mut() {
                Some(r) => (r.child.try_wait().ok().flatten().is_none(), r.cpu.sample_cores()),
                None => (false, -1.0),
            };
            AgentResponse { ok: true, running: is_running, client_cores, voice_delivered: agent.voice_delivered(), ..AgentResponse::default() }
        }
        "stop" => {
            stop_client(agent);
            AgentResponse { ok: true, ..AgentResponse::default() }
        }
        other => AgentResponse::error(format!("unknown command '{other}'")),
    }
}

fn start_client(agent: &Arc<Agent>, request: &AgentRequest) -> AgentResponse {
    stop_client(agent);

    if request.clients <= 0 {
        return AgentResponse::error("clients must be positive");
    }
    if request.host.is_empty() {
        return AgentResponse::error("host is required");
    }

    // Resolves the path AND repairs a missing execute bit, which is the usual reason a load client
    // will not start on a Linux agent box. The error, if any, names the fix.
    let exe = match LOAD_CLIENT_NAMES.iter().find_map(|name| LaunchTarget::find(&agent.client_directory, name)) {
        Some(found) => match LaunchTarget::ensure_executable(&found) {
            Ok(()) => found,
            Err(e) => return AgentResponse::error(e),
        },
        None => return AgentResponse::error(format!("no load client under '{}'", agent.client_directory.display())),
    };

    if let Err(e) = write_config(&agent.client_directory, request) {
        return AgentResponse::error(e);
    }

    let mut child = match Command::new(&exe).current_dir(&agent.client_directory).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
        Ok(child) => child,
        Err(e) => return AgentResponse::error(format!("could not start the load client: {e}")),
    };

    if let Some(stdout) = child.stdout.take() {
        let watcher = agent.clone();
        let _ = std::thread::Builder::new().name("LoadClientOutput".into()).spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(pct) = parse_voice_line(&line) {
                    watcher.set_voice_delivered(pct / 100.0);
                }
            }
        });
    }

    let cpu = ProcessCpuSampler::new(child.id());
    *agent.running.lock().unwrap_or_else(|p| p.into_inner()) = Some(Running { child, cpu });
    agent.set_voice_delivered(-1.0);

    println!("  started {} clients -> {}:{} (connect interval {} ms)", request.clients, request.host, request.port, request.connect_interval_ms);
    AgentResponse { ok: true, ..AgentResponse::default() }
}

/// `[VOICE] delivered 97.50% | ...` → 97.5
pub fn parse_voice_line(line: &str) -> Option<f64> {
    let rest = line.split("[VOICE] delivered ").nth(1)?;
    let number: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    if !rest[number.len()..].starts_with('%') {
        return None;
    }
    number.parse::<f64>().ok()
}

/// Points the load client at this run. Patches in place so its other settings survive.
fn write_config(client_directory: &Path, request: &AgentRequest) -> Result<(), String> {
    let path = client_directory.join("ClientSimConfig.xml");
    if !path.exists() {
        return Err(format!("No ClientSimConfig.xml at {}. Run the load client once by hand so it writes its defaults.", path.display()));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let patched = patch_config(&text, &[("ClientCount", request.clients.to_string()), ("Ip", request.host.clone()), ("Port", request.port.to_string()), ("ClientConnectIntervalMs", request.connect_interval_ms.to_string()), ("SimulateVoice", "true".to_string())])?;
    let temp = path.with_extension("xml.agenttmp");
    std::fs::write(&temp, patched).map_err(|e| format!("could not write {}: {e}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|e| format!("could not replace {}: {e}", path.display()))
}

/// Sets each `<Name>value</Name>` under the root in place, appending elements the file lacks.
pub fn patch_config(text: &str, values: &[(&str, String)]) -> Result<String, String> {
    let root_end = text.rfind("</Configuration>").ok_or_else(|| "ClientSimConfig.xml has no root element.".to_string())?;
    let mut out = text.to_string();
    for (name, value) in values {
        let escaped = value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        match (out.find(&open), out.find(&close)) {
            (Some(start), Some(end)) if end > start => {
                out.replace_range(start + open.len()..end, &escaped);
            }
            _ => {
                let root_end = out.rfind("</Configuration>").unwrap_or(root_end);
                out.insert_str(root_end, &format!("  <{name}>{escaped}</{name}>\n"));
            }
        }
    }
    Ok(out)
}

fn stop_client(agent: &Agent) {
    let running = agent.running.lock().unwrap_or_else(|p| p.into_inner()).take();
    let Some(mut running) = running else { return };
    if running.child.try_wait().ok().flatten().is_none() && !try_stop_gracefully(&mut running.child, Duration::from_secs(10)) {
        let _ = running.child.kill();
        let _ = wait_with_timeout(&mut running.child, Duration::from_secs(15));
    }
    println!("  load clients stopped");
}

/// Asks the load client to leave the server before killing it, and returns whether it did.
/// Writing to stdin is the one graceful stop that works on both platforms. The kill still
/// happens if it does not go quietly.
fn try_stop_gracefully(child: &mut Child, timeout: Duration) -> bool {
    let Some(stdin) = child.stdin.as_mut() else { return false };
    if stdin.write_all(b"stop\n").is_err() || stdin.flush().is_err() {
        return false;
    }
    wait_with_timeout(child, timeout)
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => return false,
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn find_load_client(directory: &Path) -> Option<PathBuf> {
    LOAD_CLIENT_NAMES.iter().find_map(|name| LaunchTarget::find(directory, name))
}

fn discover_client_directory() -> PathBuf {
    // Beside the agent first - that is how it ships - then the development layout.
    let here = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."));
    for candidate in [here.join("loadclient"), here.clone()] {
        if find_load_client(&candidate).is_some() {
            return candidate;
        }
    }
    here.join("loadclient")
}

fn num_cores() -> i32 {
    std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(1)
}

fn runtime_os() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(release) = std::fs::read_to_string("/proc/sys/kernel/osrelease") {
            return format!("Linux {}", release.trim());
        }
    }
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}

fn ctrlc_handler(handler: impl Fn() + Send + Sync + 'static) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        static HANDLER: Mutex<Option<Box<dyn Fn() + Send + Sync>>> = Mutex::new(None);
        extern "C" fn on_signal(_: libc::c_int) {
            // Only the flag flips here; the real work runs on a thread so the handler stays
            // async-signal-safe.
            SIGNALLED.store(true, Ordering::SeqCst);
        }
        static SIGNALLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        *HANDLER.lock().unwrap_or_else(|p| p.into_inner()) = Some(Box::new(handler));
        // SAFETY: installing a plain C signal handler that only touches an atomic.
        unsafe {
            libc::signal(libc::SIGINT, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t);
        }
        std::thread::Builder::new().name("SignalWatcher".into()).spawn(|| {
            loop {
                if SIGNALLED.load(Ordering::SeqCst) {
                    if let Some(handler) = HANDLER.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                        handler();
                    }
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = handler;
        Ok(())
    }
}

fn print_usage() {
    println!(
        "
BasisBenchAgent - runs Basis load clients on this machine, driven by a remote benchmark.

  basis_bench_agent [--port <n>] [--bind <addr>] [--client <dir>]

Run this on the machine that should generate the load, then point the benchmark at it:

  basis_server_benchmark --agent <this-machine>:4297

  --port <n>       Control port. Default 4297. NOT the game port - the load clients on this
                   machine are talking to the server's 4296, and sharing the number would stop
                   the agent ever running on the server's own box.
  --bind <addr>    Interface to listen on. Default 0.0.0.0.
  --client <dir>   Directory holding basis_network_client_console. Found automatically when it
                   sits beside this agent, or under ./loadclient.

The control channel is unauthenticated and will start processes on request, so run it on a trusted
network or bind it to one.

Run the load client once by hand first so it writes its default ClientSimConfig.xml; the agent patches that
file rather than replacing it, so its crowd settings - spawn radius, voice behaviour - are yours.
"
    );
}

/// Cores consumed by the load client, sampled between calls.
///
/// NaN when the process will not answer, never zero: a failed read that reports as zero looks
/// like a load generator doing its job for free, which is precisely the wrong conclusion.
struct ProcessCpuSampler {
    pid: u32,
    last_cpu_seconds: f64,
    last_timestamp: Instant,
    last_valid: bool,
}

impl ProcessCpuSampler {
    fn new(pid: u32) -> Self {
        let mut sampler = Self { pid, last_cpu_seconds: 0.0, last_timestamp: Instant::now(), last_valid: false };
        if let Some(cpu) = sampler.try_read() {
            sampler.last_cpu_seconds = cpu;
            sampler.last_valid = true;
        }
        sampler
    }

    fn sample_cores(&mut self) -> f64 {
        let now = Instant::now();
        let read = self.try_read();
        let seconds = now.duration_since(self.last_timestamp).as_secs_f64();
        let cores = match read {
            Some(cpu) if self.last_valid && seconds > 0.0 => (cpu - self.last_cpu_seconds) / seconds,
            _ => f64::NAN,
        };
        self.last_timestamp = now;
        if let Some(cpu) = read {
            self.last_cpu_seconds = cpu;
        }
        self.last_valid = read.is_some();
        if cores.is_nan() { f64::NAN } else { cores.max(0.0) }
    }

    /// Total CPU seconds (user + system) the process has used, from /proc on Linux.
    fn try_read(&self) -> Option<f64> {
        #[cfg(target_os = "linux")]
        {
            let stat = std::fs::read_to_string(format!("/proc/{}/stat", self.pid)).ok()?;
            // Fields after the parenthesised command name; utime and stime are the 14th and 15th.
            let after = stat.rsplit(')').next()?;
            let fields: Vec<&str> = after.split_whitespace().collect();
            let utime: f64 = fields.get(11)?.parse().ok()?;
            let stime: f64 = fields.get(12)?.parse().ok()?;
            // SAFETY: sysconf only reads a constant.
            let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            let ticks = if ticks > 0 { ticks as f64 } else { 100.0 };
            Some((utime + stime) / ticks)
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_line_is_parsed() {
        assert_eq!(parse_voice_line("[VOICE] delivered 97.50% | received=1 lost=0"), Some(97.5));
        assert_eq!(parse_voice_line("[VOICE] delivered 100% |"), Some(100.0));
        assert_eq!(parse_voice_line("[Driver] healthy"), None);
        assert_eq!(parse_voice_line("[VOICE] delivered abc%"), None);
    }

    #[test]
    fn config_patch_replaces_and_appends() {
        let text = "<Configuration>\n  <Ip>localhost</Ip>\n  <Port>4296</Port>\n</Configuration>\n";
        let patched = patch_config(text, &[("Ip", "10.1.1.1".into()), ("Port", "5000".into()), ("ClientCount", "20".into())]).unwrap();
        assert!(patched.contains("<Ip>10.1.1.1</Ip>"));
        assert!(patched.contains("<Port>5000</Port>"));
        assert!(patched.contains("<ClientCount>20</ClientCount>"));
        assert!(patched.trim_end().ends_with("</Configuration>"));
        assert!(patch_config("<nope/>", &[]).is_err());
    }

    #[test]
    fn version_mismatch_is_refused() {
        let agent = Arc::new(Agent { client_directory: PathBuf::from("."), running: Mutex::new(None), voice_delivered: AtomicU64::new((-1.0f64).to_bits()) });
        let response = handle(&agent, AgentRequest { command: "hello".into(), version: 99, ..AgentRequest::default() });
        assert!(!response.ok);
        let response = handle(&agent, AgentRequest { command: "hello".into(), ..AgentRequest::default() });
        assert!(response.ok);
        assert_eq!(response.agent.as_deref(), Some("BasisBenchAgent"));
        let response = handle(&agent, AgentRequest { command: "start".into(), clients: 0, ..AgentRequest::default() });
        assert_eq!(response.error.as_deref(), Some("clients must be positive"));
        let response = handle(&agent, AgentRequest { command: "bogus".into(), ..AgentRequest::default() });
        assert!(!response.ok);
    }
}
