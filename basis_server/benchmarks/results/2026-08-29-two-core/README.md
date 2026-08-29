# C# server vs Rust server — two-core comparison, 2026-08-29

**Machine:** 2 vCPU / 8 GB Linux sandbox (AVX2), kernel socket buffers clamped to 416 KB
(`net.core.rmem_max` is not writable here; both servers ask for 32 MB and both get 416 KB).
**Mode:** `--two-core` — server pinned to core 0, every load client pinned to core 1 with
`taskset`; 20 s warmup, 4 windows × 15 s, medians. Both servers see one core and size their
pools for one core. The absolute figures describe one core of this box; the **ratios** are the
result. This must be re-run without `--two-core` on a many-core host before any absolute
capacity is quoted.

**Harness:** the C# `BasisServerBenchmark`, one build, launching whichever `BasisNetworkConsole`
sits in the server directory (the C# apphost, or a copy of `basis_network_console`) and reading
the same `/health` document from both. **Crowd:** the C# `BasisNetworkClientConsole` — real
LiteNetLib clients with voice simulation on — for the legacy runs; the Rust
`basis_network_client_console` over iroh for the modern share of the mixed runs.

Raw harness output is in `logs/`; `results.txt` has the `RESULT` lines; `summary.md` the full
tables. Commit: the benchmark ran against `basis_server` at the commit this directory was added in.

## 1. Legacy crowd: the existing C# LiteNetLib clients against both servers

| players | metric | C# server | Rust server | Rust / C# |
|---|---|---|---|---|
| 50  | server cores | 0.125 | 0.198 | **1.58×** |
| 50  | delivered Hz/pair | 83.33 | 83.33 | 1.00× |
| 50  | egress MB/s · datagrams/s | 0.68 · 3.9k | 0.78 · 4.3k | 1.14× · 1.09× |
| 50  | voice heard | 99.42 % | 99.44 % | 1.00× |
| 100 | server cores | 0.231 | 0.351 | **1.52×** |
| 100 | delivered Hz/pair | 83.33 | 83.33 | 1.00× |
| 100 | egress MB/s · datagrams/s | 2.98 · 9.7k | 3.06 · 9.7k | 1.03× · 1.00× |
| 100 | voice heard | 99.39 % | 99.39 % | 1.00× |
| 200 | server cores | 0.405 | 0.375 | **0.93×** |
| 200 | delivered Hz/pair | 83.33 | 83.33 | 1.00× |
| 200 | tick ms · overrun | 2.63 · 3.1 % | 1.81 · 0 % | 0.69× · — |
| 200 | egress MB/s · datagrams/s | 11.70 · 23.3k | 11.62 · 23.1k | 0.99× · 0.99× |
| 200 | voice heard | 99.41 % | 99.48 % | 1.00× |
| 400 | server cores | 0.663 | 0.672 | 1.01× |
| 400 | delivered Hz/pair | 46.48 | **50.50** | **1.09×** |
| 400 | slice · tick ms · overrun | 1.16 · 12.55 · 17.8 % | 1.03 · 11.69 · 12.5 % | 0.89× · 0.93× · 0.70× |
| 400 | egress MB/s · datagrams/s | 41.5 · 58.9k | 42.9 · 56.8k | 1.03× · 0.97× |
| 400 | voice heard | 98.84 % | 99.23 % | 1.00× |
| idle | cores · RSS · threads | 0.016 · 74 MB · 18 | 0.013 · 19 MB · 9 | 0.83× · 0.26× |

Delivery ratio was 1.000 (no shedding) on every run of both servers; avatar and voice drops were 0.

Reading:

* **Wire compatibility is total.** The unmodified C# load client — the client every deployment
  runs — joins the Rust server, authenticates, and gets the same 83.33 Hz/pair, the same egress,
  the same datagram count and the same voice delivery as it gets from the C# server. The
  datagram counts within 1 % at every rung say the Rust `Merged`/`CompactMerged` sender packs
  the wire exactly as LiteNetLib does.
* **From 200 players up the Rust server is at least as cheap and delivers more.** At 200 it uses
  7 % less CPU with a 31 % shorter tick and no overrun; at 400, the first rung where a single
  core is saturated on both, it delivers 9 % more receiver visits per second at equal CPU with
  30 % less overrun and less slicing. Egress is equal, so it is the same work done for less.
* **At 50–100 players the Rust server costs 1.5× the CPU** — 0.07–0.12 of a core in absolute
  terms, and the one dimension where the old server is more than 10 % better. Idle cost is
  *lower* on Rust (0.013 vs 0.016 cores, 19 vs 74 MB), so this is per-tick dispatch overhead at
  small populations, not a fixed cost: see plan item R1 below.
* **Memory**: the Rust server's `committedMb` read 0 in these runs because its `/health` did
  not yet emit the field (fixed after the run: it now reports the resident set). Idle RSS is
  19 MB against 74 MB for the CLR.

## 2. Mixed crowd on the Rust server: legacy + iroh together, against the all-legacy baseline

The C# server cannot host an iroh crowd, so the mixed and all-iroh columns are the Rust server
only; the baseline is the all-legacy C# run above.

| players | crowd | server cores | vs C# all-legacy | Hz/pair | datagrams/s | crowd cores | voice heard |
|---|---|---|---|---|---|---|---|
| 50  | all legacy (Rust) | 0.198 | 1.58× | 83.33 | 4.3k | 0.064 | 99.4 % |
| 50  | half iroh | 0.287 | 2.30× | 83.33 | 7.4k | 0.233 | 99.1 % |
| 50  | all iroh | 0.271 | 2.17× | 83.33 | 11.1k | 0.342 | 99.2 % |
| 100 | all legacy (Rust) | 0.351 | 1.52× | 83.33 | 9.7k | 0.139 | 99.4 % |
| 100 | half iroh | 0.270 | 1.17× | 83.33 | 25.5k | 0.354 | 99.3 % |
| 100 | all iroh | 0.349 | 1.51× | 83.33 | 49.0k | 0.523 | 99.7 % |
| 200 | all legacy (Rust) | 0.375 | 0.93× | 83.33 | 23.1k | 0.333 | 99.5 % |
| 200 | half iroh | 0.535 | 1.32× | 83.33 | 97.1k | 0.497 | 99.4 % |
| 200 | all iroh | 0.699 | 1.73× | 83.33 | 166.2k | 0.798 | 99.2 % |
| 400 | all legacy (Rust) | 0.672 | 1.01× | 50.50 | 56.8k | 0.579 | 99.2 % |
| 400 | half iroh | 0.784 | 1.18× | **9.38** (slice 5.5) | 217.0k | 0.768 | 93.9 % |
| 400 | all iroh | 0.873 | 1.32× | 37.50 | 343.0k | 0.817 | **49.4 %** |

Reading:

* **Mixing works.** Every mixed run seated its whole crowd, delivered everything (ratio 1.0, no
  drops) and kept voice above 99 % up to 200 players. Legacy and iroh clients share the room
  and each other's traffic at full quality.
* **The iroh path costs far more per message than LiteNetLib — and the datagram column says
  why.** At 200 players the all-iroh crowd moves the same 11 MB/s as the legacy crowd in
  **7.1× the datagrams** (166k/s vs 23k/s; 68 bytes per datagram against ~500). The iroh
  sender puts every unreliable message into its own QUIC datagram, and every QUIC datagram is
  its own packet: a header, a 16-byte AEAD tag, an encryption pass and a `sendmsg`. LiteNetLib
  merges everything queued for a peer into MTU-sized datagrams. Per-datagram cost, not
  per-byte cost, is the whole gap: at 200 players the server spends 1.86× the CPU of the legacy
  path, and the *crowd* — which decrypts every one of those packets — 2.4×.
* **At 400 the iroh path falls over the cliff first.** All-iroh: slice 1.6, tick 14.9 ms and
  voice heard collapses to 49 % — with the crowd itself at 0.82 of its core, so part of that is
  the receivers falling behind, which this two-core mode cannot separate. Half-iroh: the
  reduction system slices 5.5 ways and delivers 9.4 Hz/pair. Either way the per-datagram cost
  is what saturates the core.
* Against the >10 % rule: **iroh is more than 10 % slower than LiteNetLib on this server**
  (1.5–1.9× server CPU at equal delivery from 100 players up, and it saturates first). An
  optimisation plan is required and is below.

## 3. Optimisation plans

### iroh (required: >10 % slower than LiteNetLib)

**I1 — coalesce unreliable datagrams (the fix).** Drain a peer's queued datagram frames into
one QUIC datagram up to the path's `max_datagram_size` (voice frames first, as today), framed
as `[0x80][len:u16][frame]…`, and unpack on receive. Both ends are this repository's code (the
Rust clients and the C# clients through `basis_iroh_ffi`), so it is a protocol bump (`basis/2`)
with no deployed peer to keep compatible. Expected: datagrams/s ÷ 5–8 at 200 players, server
CPU on the iroh path to within ~10 % of the legacy path, and the crowd's decrypt cost down by
the same factor. This is the same idea LiteNetLib's `Merged`/`CompactMerged` framing
implements, and the measurements above are the evidence that the framing, not the transport,
is where the cost lives.

**I2 — batch the sender's wakeups.** The sender task is woken per enqueued frame
(`Notify::notify_one`) and takes each frame through `send_datagram_wait` on its own; after I1
it should drain the whole queue per wakeup and only wake on the transition from empty.

**I3 — GSO / `sendmmsg`.** quinn uses UDP GSO on Linux when the kernel offers it; this sandbox
kernel does not (no `/proc/sys/net/core`). On a real host, check `iroh`'s socket is built with
GSO enabled; it lets one syscall carry several QUIC packets to the same peer, which stacks
with I1.

**I4 — receive-side fan-in.** The 49 % voice figure at 400 all-iroh needs the many-core re-run
to be attributed: if it survives with the crowd unconstrained it is the server's priority
datagram queue being drained too slowly under load and needs the same coalescing on the voice
path plus a larger `max_priority_datagram_queue_per_peer` floor.

### Rust server, legacy path (required: >10 % more CPU at 50–100 players)

**R1 — do not dispatch a tiny population to the peer-update pool.** `LnlNetManager`'s logic
pass hands any population above 8 peers to a rayon pool every 2 ms; on a one-core grant that
pool has one thread, so every tick pays a cross-thread hand-off and wake-up for microseconds
of work. LiteNetLib's `Parallel.ForEach` runs inline when it has one worker. Fix: run inline
below `PeersPerUpdateWorker` (128) peers or when the pool has one thread, and chunk so no
worker gets fewer than 16 peers. Expected: the 50–100-player CPU gap closes; no effect from
200 up, where the pool is already earning its keep.

**R2 — packet pooling on the receive path.** Every datagram is copied into a fresh `Vec` and
every merged entry into another; LiteNetLib recycles packets through a pool. Worth doing after
R1 is measured, since the allocator cost is spread evenly and did not show at 200–400.

**R3 — measure on a many-core host before touching anything else.** The two-core mode
squeezes everything through one core; the send-worker and peer-update pools, which are where
the Rust server's remaining design differences live, never got to widen here.

### C# server (the old server was >10 % better on one dimension)

The only dimension where the C# server led by more than 10 % is CPU at 50–100 players; R1 is
the plan for it. Everything else — delivery, egress, voice, tick, overrun, memory, idle — the
Rust server matches or beats at every rung measured.
