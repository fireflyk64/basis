# Server benchmarks: C# vs Rust, legacy crowd and mixed crowd

The question these runs answer: does the Rust server serve the **existing** clients — the C#
LiteNetLib population every deployment has today — at least as well as the C# server does, and
what does adding iroh clients beside them cost?

## How the comparison is kept honest

* **One harness.** Both servers are measured by the C# `BasisServerBenchmark`, unchanged in how
  it measures: it boots the server, seats the crowd, waits out a warmup, closes timed windows and
  reports medians of what the server's `/health` document and the process's CPU say. The Rust
  server is dropped into a server directory under the name the harness launches
  (`BasisNetworkConsole`) and serves the same `/health` document with the same field names.
* **One crowd.** The legacy runs use the C# load client (`BasisNetworkClientConsole`): real
  LiteNetLib clients with voice on, exactly what the servers see in production. The mixed runs
  add the Rust load client (`basis_network_client_console`) over iroh for the share given by
  `--mix`, seated on the same server at the same time.
* **Two-core mode** (`--two-core`). On a two-core box the server and the crowd otherwise
  time-slice each other and a server's CPU figure mostly measures the scheduler. The mode pins
  the server to core 0 and every load client to core 1 with `taskset`, so the two servers see
  the same machine. The absolute numbers describe one core; the **ratio** between the servers is
  the result. Re-run without the flag on a many-core host for absolute capacity.

## Running it

```sh
# once: build both sides
~/.dotnet/dotnet build "Basis Server/Basis Server.sln" -c Release
(cd basis_server && cargo build --release -p basis_network_console -p basis_network_client_console)

# stage private copies of both servers and both load clients (each booted once for its config)
benchmarks/stage-workdir.sh /tmp/basis-bench

# the legacy comparison: the C# crowd against both servers, 50..400 players
benchmarks/run-comparison.sh /tmp/basis-bench 50 100 200 400

# the mixed crowd on the Rust server: half legacy, half iroh
BENCH_SERVERS=rust BENCH_MIX=0.5 benchmarks/run-comparison.sh /tmp/basis-bench 50 100 200 400

# a table from any results directory
benchmarks/summarize.py /tmp/basis-bench/results/<run> --markdown
```

`DOTNET_ROOT` defaults to `~/.dotnet`. Each `/measure` prints a `RESULT` line the summariser
reads; the full harness output is kept beside it.

## What the numbers mean

| metric | source | reading |
|---|---|---|
| delivered Hz/pair | `/health` bsr window | receiver visits per second before loss — the quality figure, higher is better |
| delivery ratio | `/health` | sends that were not shed at the queue bound |
| server cores | process CPU | cores the server process used over the window; lower is better at equal delivery |
| crowd cores | process CPU | the load clients' cost, excluded from the score, shown so a crowd-bound run is visible |
| egress MB/s, datagrams/s | `/health` counters | what left the server; equal work should show near-equal egress |
| avatar / voice drops/s | `/health` | shedding; voice drops are audio nobody heard |
| slice, tick ms, overrun | `/health` bsr load | how hard the reduction system is working |
| voice heard | load client | share of simulated voice frames a receiver actually got |

Results and the analysis live in `results/`.
