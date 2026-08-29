# Upstream report (draft, not yet sent): per-datagram transmit cost for many-connection, small-datagram workloads

**Where this goes:** `n0-computer/iroh` (or the `noq`/quinn fork's tracker, whichever the
maintainers prefer for transmit-path work). **Status: written, not posted.** Nothing here has
been sent to anyone; posting it is a decision for a human, and it should get a second read
first — the numbers are ours, the conclusions are about someone else's code.

---

## Summary

For a workload of a few hundred long-lived connections each carrying one small unreliable
datagram per tick in both directions, the QUIC stack spends **2.4× the CPU per packet** that
the same traffic costs over a plain UDP socket — and at the same offered load it does not keep
up, delivering 14.3k of an offered 16.6k datagrams/s while a plain socket delivers all of them
for a third of the CPU. The profile puts most of the cost in the per-packet transmit cycle
rather than in cryptography, congestion control or ACK processing. On a two-core box this is
the difference between a server that seats 400 players and one that seats 200.

The standalone reproducer below (200 connections, 83 Hz each way, 500-byte payloads, both
processes pinned to their own core) measures, per five-second window:

| | plain UDP | iroh 1.1 datagrams |
|---|---|---|
| server CPU per datagram | 7.6 – 9.6 µs (median **8.3**) | 19.8 – 20.0 µs (median **20.0**) |
| server CPU at that load | 0.24 – 0.32 cores | **0.57 – 0.58 cores** |
| datagrams actually carried | 16.2k – 16.6k/s (the full offered rate) | 14.2k – 14.7k/s |

We are not reporting a bug — everything is correct and delivery is perfect. We are reporting a
cost profile, with a reproducer, in case a datagram-shaped fast path is interesting to you.

## The workload

A VR social server: 200 connected clients, each sending ~83 small messages per second
(avatar state, voice frames; 60–1400 bytes, typically ~500), and the server sending each client
one merged message per tick. All of it unreliable and unordered — QUIC datagrams, no streams
in the steady state. Connections live for hours. Nothing is bulk; nothing is large.

The same application also speaks a plain-UDP protocol (LiteNetLib's wire format) to older
clients, so we can measure both transports carrying identical application traffic on the same
box, in the same process, at the same time. That is what makes the comparison below tight.

## The measurements

Two-core Linux box (2 vCPU, AVX2, AES-NI present and in use), server pinned to one core, the
client crowd pinned to the other, 200 players, 15-second measurement windows:

| | plain UDP (LiteNetLib) | iroh 1.1 datagrams |
|---|---|---|
| server CPU | **0.371 cores** | **0.587 cores** |
| delivered rate | 83.3 Hz/pair | 83.3 Hz/pair (identical) |
| application egress | 11.4 MB/s | 9–11 MB/s |
| UDP packets/s (box-wide, both directions) | **26.2 k** | **36.6 k** |
| user-side CPU per packet | ~7 µs | ~21 µs |

(The isolated reproducer, with no game logic in the process at all, puts the same pair at
8.3 µs and 20.0 µs — so this is the transport, not the application around it.)

The extra 10 k packets/s are almost entirely ACK-only packets: every QUIC datagram is
ack-eliciting, and the plain-UDP protocol acknowledges nothing on its unreliable path.

## The profile

Sampled inside the running server at 199 Hz for 25 s during the measurement windows (SIGPROF,
`pprof` crate — `perf_event_open` is unavailable in our environment), 1951 samples on the iroh
run:

| component | % of process CPU |
|---|---|
| `noq`/`noq-proto` (the QUIC stack), inclusive | **59 %** |
| — per-packet transmit assembly: `poll_transmit` → `populate_packet` → frame writes → seal | **~27 %** |
| — connection-event / channel plumbing (`ConnectionEvent` mpsc, wakes) | ~13 % |
| — driver poll loops not otherwise attributed | ~11 % |
| — UDP send/recv inside the driver | ~4 % |
| — timers | 1.7 % |
| — **ACK / loss detection / congestion control** | **0.2 %** |
| — **multipath bookkeeping** (`PathId`, `cid_queue`) | **0.4 %** |
| **AEAD (ring, AES-NI)** | **5.1 %** |
| allocator (glibc malloc/free, innermost frames) | **18.5 %** — alloc frames appear in 76.6 % of stacks |
| our application code (handlers, transport glue) | ~15 % |

Two things stand out to us:

1. **Cryptography is not the cost** (5 %), and neither is the loss/congestion machinery (0.2 %).
   The cost is *building the packet*: assembling frames, writing headers, and the per-transmit
   allocations around it.
2. **The allocator is 18.5 %** of the whole process under this load. Swapping the global
   allocator (glibc → mimalloc) recovers about 5 % of server CPU per packet on its own, in
   paired runs — which says the transmit path allocates enough per packet for the choice of
   allocator to be measurable at this rate, and that most of that 18.5 % is work the transmit
   path asks for rather than glibc being slow.

## What we tried first, so you can skip it

* **Coalescing at the application layer** — packing many of our small messages into one QUIC
  datagram, protocol bump and all. It cut our application-level frame count 7× and moved CPU by
  **nothing**, because quinn already packs queued datagram frames into packets. Packet count,
  not frame count, is the cost. We reverted it.
* **ACK frequency and MTU discovery configuration** on the server side: no measurable change.
  (We are re-testing this configured on both ends; the packet counts above are why.)
* **Cheaper AEAD** (ChaCha20 vs AES): pointless at 5 % with AES-NI already engaged.

## Reproducer

`iroh-datagram-cost/` next to this file is a standalone crate (no dependency on our code) that
generates exactly this traffic shape over iroh and over plain UDP, and prints each side's CPU
per datagram from `/proc/self/stat`:

```
cargo run --release -- server --conns 200 --hz 83 --size 500
cargo run --release -- client --connect <id>@127.0.0.1:<port> --conns 200 --hz 83 --size 500

cargo run --release -- udp-server --port 9101 --hz 83 --size 500
cargo run --release -- udp-client --connect 127.0.0.1:9101 --conns 200 --hz 83 --size 500
```

Compare the server's reported µs/datagram between the two runs. Pin the two processes to
separate cores on a small box. Output looks like this (our box, verbatim):

```
[iroh server] conns=200 cpu=0.580 cores  sent=14692/s recv=14675/s  19.75 µs cpu per datagram
[udp server]  conns=200 cpu=0.246 cores  sent=16224/s recv=16020/s   7.63 µs cpu per datagram
```

The windows are stable to about ±1 %, which is what makes it a usable instrument: our
full application benchmark on the same box varies ±25 % run to run at this rung, because the
delivered packet rate is itself a free variable there.

## What would help us most, in order

1. **A transmit fast path for datagram-only connections.** When a connection's send queue holds
   only datagram frames and no stream data, the full `poll_transmit` assembly is more machinery
   than the packet needs.
2. **Reusable transmit buffers.** 18.5 % of our process is the allocator, and the transmit path
   is where the small allocations cluster. A per-connection or per-driver scratch buffer that
   survives across transmits would remove most of it.
3. **Batched wakeups per connection.** ~13 % of CPU is event/channel plumbing at ~115 sends per
   second per connection across 200 connections. Coalescing "work is available" signals per
   driver pass rather than per send would cut into that.
4. **Guidance on ACK frequency for unreliable-only workloads.** If there is a supported way to
   tell a peer "these datagrams do not need prompt acknowledgement", we would take it — 10 k
   packets/s of pure ACK traffic is 28 % of our packet budget.

## Environment

iroh 1.1.0 (noq 1.2.0, noq-proto 1.2.0, rustls 0.23.43, ring AEAD), Rust 1.98 edition 2024,
Linux 6.8 x86_64 (AVX2 + AES-NI), release build with line-tables debug info, GSO unavailable on
this kernel, socket buffers clamped to 416 KB by the host. Relays disabled; all traffic is
direct over loopback/LAN. Full profile artifacts (folded stacks, flamegraphs, `/proc` series)
are in `benchmarks/results/2026-08-29-iroh-profile/` in the repository this file came from, and
we can attach them to the issue.
