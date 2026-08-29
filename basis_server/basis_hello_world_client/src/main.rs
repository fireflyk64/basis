//! Hello world for the Basis network: a ring of clients passing a number around, each one adding
//! 1 and handing it to its neighbour, so every hop is a real round trip through a real server.
//!
//! ```text
//!   cargo run -p basis_hello_world_client -- --ip 127.0.0.1 --port 4296 --password default_password
//! ```
//!
//! `--ip` takes a host or an iroh connection string (`<endpoint-id>[@host:port]`). Add `--direct`
//! and the ring runs over direct peer-to-peer links instead; each hop prints the path it took.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use basis_hello_world_client::{BasisHelloClient, HelloPeerClient};
use parking_lot::{Condvar, Mutex};

enum Client {
    Relay(Arc<BasisHelloClient>),
    Direct(Arc<HelloPeerClient>),
}

impl Client {
    fn base(&self) -> &Arc<BasisHelloClient> {
        match self {
            Client::Relay(c) => c,
            Client::Direct(c) => c.base(),
        }
    }

    fn pass(&self, target: u16, value: i32) {
        let result = match self {
            Client::Relay(c) => c.send_number(target, value),
            Client::Direct(c) => c.send_number_direct(target, value),
        };
        if let Err(e) = result {
            eprintln!("{} could not pass {value} on: {e}", self.base().display_name());
        }
    }
}

fn arg(args: &[String], name: &str, fallback: &str) -> String {
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone()).unwrap_or_else(|| fallback.to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let ip = arg(&args, "--ip", "127.0.0.1");
    let port: u16 = arg(&args, "--port", "4296").parse().unwrap_or(4296);
    let password = arg(&args, "--password", "default_password");
    let client_count: usize = arg(&args, "--clients", "2").parse().unwrap_or(2).max(2);
    let hops: i32 = arg(&args, "--hops", "10").parse().unwrap_or(10).max(1);
    let direct = args.iter().any(|a| a == "--direct");

    println!("Connecting {client_count} clients to {ip}:{port} for a {hops}-hop volley{}.", if direct { " over direct links" } else { "" });

    let mut clients: Vec<Client> = Vec::with_capacity(client_count);
    for i in 0..client_count {
        let name = format!("Hello{i}");
        let client = if direct {
            match HelloPeerClient::new(&name) {
                Ok(c) => Client::Direct(c),
                Err(e) => {
                    eprintln!("Client {i} could not be created: {e}");
                    return ExitCode::from(1);
                }
            }
        } else {
            match BasisHelloClient::new(&name) {
                Ok(c) => Client::Relay(c),
                Err(e) => {
                    eprintln!("Client {i} could not be created: {e}");
                    return ExitCode::from(1);
                }
            }
        };
        match client.base().connect(&ip, port, &password, Duration::from_secs(15)) {
            Ok(true) => println!("  {name} joined as player {}", client.base().player_id()),
            Ok(false) => {
                eprintln!("Client {i} could not join {ip}:{port}. Is the server running, and is --password right?");
                return ExitCode::from(1);
            }
            Err(e) => {
                eprintln!("Client {i} could not connect to {ip}:{port}: {e}");
                return ExitCode::from(1);
            }
        }
        clients.push(client);
    }

    let clients = Arc::new(clients);
    let finished = Arc::new((Mutex::new(false), Condvar::new()));
    // Every client does the same thing: take the number, add one, pass it on. The ring is set up
    // after all of them have joined, because a neighbour's player id is only known once it is in.
    for i in 0..client_count {
        let clients_ref = clients.clone();
        let finished_ref = finished.clone();
        let next_index = (i + 1) % client_count;
        clients[i].base().on_number_received(Arc::new(move |sender_id, value, path| {
            let me = &clients_ref[i];
            println!("  {} (player {}) got {value} from player {sender_id} via {path}", me.base().display_name(), me.base().player_id());
            if value >= hops {
                *finished_ref.0.lock() = true;
                finished_ref.1.notify_all();
                return;
            }
            me.pass(clients_ref[next_index].base().player_id(), value + 1);
        }));
    }

    // One link per ring edge, opened before the volley starts. A link that does not come up is
    // not fatal: the send falls back to the server, and the printed path says so on every hop.
    if direct {
        for i in 0..client_count {
            let Client::Direct(me) = &clients[i] else { continue };
            let neighbour = clients[(i + 1) % client_count].base().player_id();
            let up = me.open_direct_link(neighbour, Duration::from_secs(20)).unwrap_or(false);
            println!("  {} -> player {neighbour}: {}", me.display_name(), if up { "direct link up" } else { "no direct link, will relay" });
        }
    }

    println!("{} starts the volley with 1.", clients[0].base().display_name());
    clients[0].pass(clients[1].base().player_id(), 1);

    let done = {
        let mut flag = finished.0.lock();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !*flag {
            let now = std::time::Instant::now();
            if now >= deadline {
                break;
            }
            finished.1.wait_for(&mut flag, deadline - now);
        }
        *flag
    };
    let code = if done {
        println!("Done: the number went round the ring and reached {hops}.");
        ExitCode::SUCCESS
    } else {
        eprintln!("The volley did not reach the end within 30s.");
        ExitCode::from(1)
    };
    for client in clients.iter() {
        client.base().disconnect();
    }
    code
}
