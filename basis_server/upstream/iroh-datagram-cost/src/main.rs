//! What one small QUIC datagram costs a server, against the same traffic over plain UDP.
//!
//! The workload is a game server's: a few hundred long-lived connections, each carrying one
//! small unreliable message per client per tick in both directions. Nothing here is reliable,
//! ordered or large — it is the shape a state-replication server actually sends.
//!
//!   # terminal 1 — iroh
//!   cargo run --release -- server --conns 200 --hz 83 --size 500
//!   # prints:  dial: <id>@127.0.0.1:<port>
//!   # terminal 2
//!   cargo run --release -- client --connect <id>@127.0.0.1:<port> --conns 200 --hz 83 --size 500
//!
//!   # the same traffic over plain UDP, for the baseline
//!   cargo run --release -- udp-server --port 9101 --hz 83 --size 500
//!   cargo run --release -- udp-client --connect 127.0.0.1:9101 --conns 200 --hz 83 --size 500
//!
//! Every five seconds each side prints its own CPU (from `/proc/self/stat`, so Linux), the
//! datagrams it sent and received, and the microseconds of CPU it spent per datagram. Compare
//! the server's µs/datagram between the iroh run and the UDP run: that difference, times the
//! packet rate, is what the QUIC stack costs above the socket.
//!
//! Pin both sides to their own core (`taskset -c 0` / `taskset -c 1`) if the box has few, or the
//! two processes will measure each other.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use iroh::endpoint::{presets, Connection, QuicTransportConfig, VarInt};
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode};

const ALPN: &[u8] = b"iroh-datagram-cost/1";

#[derive(Clone)]
struct Counters {
    sent: Arc<AtomicU64>,
    received: Arc<AtomicU64>,
}

impl Counters {
    fn new() -> Self {
        Self { sent: Arc::new(AtomicU64::new(0)), received: Arc::new(AtomicU64::new(0)) }
    }
}

struct Args {
    role: String,
    conns: usize,
    hz: u64,
    size: usize,
    port: u16,
    connect: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let role = it.next().ok_or("usage: <server|client|udp-server|udp-client> [--conns N] [--hz N] [--size N] [--port N] [--connect TARGET]")?;
    let mut args = Args { role, conns: 200, hz: 83, size: 500, port: 0, connect: None };
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--conns" => args.conns = value.parse().map_err(|e| format!("--conns: {e}"))?,
            "--hz" => args.hz = value.parse().map_err(|e| format!("--hz: {e}"))?,
            "--size" => args.size = value.parse().map_err(|e| format!("--size: {e}"))?,
            "--port" => args.port = value.parse().map_err(|e| format!("--port: {e}"))?,
            "--connect" => args.connect = Some(value),
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if args.hz == 0 {
        return Err("--hz must be at least 1".to_string());
    }
    Ok(args)
}

/// This process's CPU seconds so far, user + system, from `/proc/self/stat`.
fn cpu_seconds() -> f64 {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else { return 0.0 };
    // The comm field is parenthesised and may itself contain spaces, so the fields are counted
    // from after the last ')': state, ppid, pgrp, session, tty_nr, tpgid, flags, minflt,
    // cminflt, majflt, cmajflt, utime, stime — utime is index 11 and stime index 12.
    let Some((_, after_comm)) = stat.rsplit_once(')') else { return 0.0 };
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let ticks = |i: usize| fields.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    // USER_HZ, which is 100 on every Linux this runs on (sysconf(_SC_CLK_TCK)).
    (ticks(11) + ticks(12)) / 100.0
}

async fn report(label: &'static str, counters: Counters, conns: usize) {
    let mut last = Instant::now();
    let mut last_cpu = cpu_seconds();
    let mut last_sent = 0u64;
    let mut last_received = 0u64;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let now = Instant::now();
        let cpu = cpu_seconds();
        let sent = counters.sent.load(Ordering::Relaxed);
        let received = counters.received.load(Ordering::Relaxed);
        let elapsed = now.duration_since(last).as_secs_f64();
        let d_cpu = cpu - last_cpu;
        let d_sent = sent - last_sent;
        let d_received = received - last_received;
        let packets = d_sent + d_received;
        let per_packet_us = if packets > 0 { d_cpu * 1e6 / packets as f64 } else { 0.0 };
        println!(
            "[{label}] conns={conns} cpu={:.3} cores  sent={:.0}/s recv={:.0}/s  {:.2} µs cpu per datagram",
            d_cpu / elapsed,
            d_sent as f64 / elapsed,
            d_received as f64 / elapsed,
            per_packet_us
        );
        last = now;
        last_cpu = cpu;
        last_sent = sent;
        last_received = received;
    }
}

fn transport_config() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .max_idle_timeout(iroh::endpoint::IdleTimeout::try_from(Duration::from_secs(30)).ok())
        .keep_alive_interval(Duration::from_secs(10))
        .datagram_receive_buffer_size(Some(4 * 1024 * 1024))
        .datagram_send_buffer_size(256 * 1024)
        .build()
}

async fn bind_endpoint(port: u16) -> Result<Endpoint, String> {
    Endpoint::builder(presets::Minimal)
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(RelayMode::Disabled)
        .transport_config(transport_config())
        .bind_addr(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))
        .map_err(|e| format!("bad bind address: {e}"))?
        .bind()
        .await
        .map_err(|e| format!("bind failed: {e}"))
}

/// Sends one datagram every 1/hz on this connection and reads whatever arrives, until it closes.
fn drive_connection(conn: Connection, hz: u64, size: usize, counters: Counters) {
    let payload = Bytes::from(vec![0x5au8; size]);
    let sender = conn.clone();
    let sent = counters.sent.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_micros(1_000_000 / hz));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match sender.send_datagram(payload.clone()) {
                Ok(()) => {
                    sent.fetch_add(1, Ordering::Relaxed);
                }
                Err(iroh::endpoint::SendDatagramError::ConnectionLost(_)) => return,
                Err(_) => {}
            }
        }
    });
    let received = counters.received.clone();
    tokio::spawn(async move {
        while conn.read_datagram().await.is_ok() {
            received.fetch_add(1, Ordering::Relaxed);
        }
    });
}

async fn run_server(args: Args) -> Result<(), String> {
    let endpoint = bind_endpoint(args.port).await?;
    let port = endpoint.bound_sockets().first().map(|s| s.port()).unwrap_or(0);
    println!("dial: {}@127.0.0.1:{port}", endpoint.id().to_z32());
    let counters = Counters::new();
    tokio::spawn(report("iroh server", counters.clone(), args.conns));
    while let Some(incoming) = endpoint.accept().await {
        let counters = counters.clone();
        let (hz, size) = (args.hz, args.size);
        tokio::spawn(async move {
            let Ok(accepting) = incoming.accept() else { return };
            let Ok(conn) = accepting.await else { return };
            drive_connection(conn, hz, size, counters);
        });
    }
    Ok(())
}

async fn run_client(args: Args) -> Result<(), String> {
    let target = args.connect.clone().ok_or("client needs --connect <id>@<host>:<port>")?;
    let (id_text, socket_text) = target.split_once('@').ok_or("--connect must be <id>@<host>:<port>")?;
    let id = EndpointId::from_z32(id_text.trim())
        .or_else(|_| id_text.trim().parse::<EndpointId>())
        .map_err(|e| format!("'{id_text}' is not an endpoint id: {e}"))?;
    let socket: SocketAddr = socket_text.parse().map_err(|e| format!("'{socket_text}' is not host:port: {e}"))?;
    let addr = EndpointAddr::new(id).with_ip_addr(socket);

    let endpoint = bind_endpoint(0).await?;
    let counters = Counters::new();
    tokio::spawn(report("iroh client", counters.clone(), args.conns));
    let mut connections = Vec::with_capacity(args.conns);
    for i in 0..args.conns {
        match endpoint.connect(addr.clone(), ALPN).await {
            Ok(conn) => {
                drive_connection(conn.clone(), args.hz, args.size, counters.clone());
                connections.push(conn);
            }
            Err(e) => return Err(format!("connection {i} failed: {e}")),
        }
        // A small stagger so several hundred handshakes do not land in one instant.
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    println!("[iroh client] {} connections established", connections.len());
    tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
    for conn in connections {
        conn.close(VarInt::from_u32(0), b"done");
    }
    Ok(())
}

async fn run_udp_server(args: Args) -> Result<(), String> {
    let socket = Arc::new(
        tokio::net::UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, args.port)))
            .await
            .map_err(|e| format!("bind failed: {e}"))?,
    );
    println!("udp dial: 127.0.0.1:{}", socket.local_addr().map_err(|e| e.to_string())?.port());
    let counters = Counters::new();
    tokio::spawn(report("udp server", counters.clone(), args.conns));

    // One task per known client address, started the first time that address is heard from, so
    // the send shape matches the iroh run exactly: one datagram per peer per tick.
    let mut known: std::collections::HashSet<SocketAddr> = std::collections::HashSet::new();
    let mut buffer = vec![0u8; 2048];
    loop {
        let (_, from) = socket.recv_from(&mut buffer).await.map_err(|e| format!("recv failed: {e}"))?;
        counters.received.fetch_add(1, Ordering::Relaxed);
        if known.insert(from) {
            let socket = socket.clone();
            let sent = counters.sent.clone();
            let (hz, size) = (args.hz, args.size);
            tokio::spawn(async move {
                let payload = vec![0x5au8; size];
                let mut ticker = tokio::time::interval(Duration::from_micros(1_000_000 / hz));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    if socket.send_to(&payload, from).await.is_err() {
                        return;
                    }
                    sent.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    }
}

async fn run_udp_client(args: Args) -> Result<(), String> {
    let target: SocketAddr = args
        .connect
        .clone()
        .ok_or("udp-client needs --connect <host>:<port>")?
        .parse()
        .map_err(|e| format!("--connect must be host:port: {e}"))?;
    let counters = Counters::new();
    tokio::spawn(report("udp client", counters.clone(), args.conns));
    for _ in 0..args.conns {
        let socket = tokio::net::UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
            .await
            .map_err(|e| format!("bind failed: {e}"))?;
        socket.connect(target).await.map_err(|e| format!("connect failed: {e}"))?;
        let socket = Arc::new(socket);
        let receiver = socket.clone();
        let received = counters.received.clone();
        tokio::spawn(async move {
            let mut buffer = vec![0u8; 2048];
            while receiver.recv(&mut buffer).await.is_ok() {
                received.fetch_add(1, Ordering::Relaxed);
            }
        });
        let sent = counters.sent.clone();
        let (hz, size) = (args.hz, args.size);
        tokio::spawn(async move {
            let payload = vec![0x5au8; size];
            let mut ticker = tokio::time::interval(Duration::from_micros(1_000_000 / hz));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if socket.send(&payload).await.is_err() {
                    return;
                }
                sent.fetch_add(1, Ordering::Relaxed);
            }
        });
    }
    println!("[udp client] {} sockets sending", args.conns);
    tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let result = match args.role.as_str() {
        "server" => run_server(args).await,
        "client" => run_client(args).await,
        "udp-server" => run_udp_server(args).await,
        "udp-client" => run_udp_client(args).await,
        other => Err(format!("unknown role '{other}'")),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
