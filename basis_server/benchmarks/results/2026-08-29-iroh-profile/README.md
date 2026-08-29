# Why the iroh path costs more than LiteNetLib — symbol-level profile, 2026-08-29

The two-core comparison left one question open: the iroh path costs the server ~1.5–1.9× the
LiteNetLib path at 200 players and the cost sat somewhere inside "quinn per-connection
processing", unattributable because this sandbox blocks `perf_event_open` even for root.
This directory answers it with an **in-process SIGPROF sampler** (`pprof` crate) compiled into
the console behind `--features pprof`, armed by environment (`BASIS_PPROF=25:<prefix>`,
`BASIS_PPROF_TRIGGER=<file>`), triggered by the harness log the moment the first measurement
window completes, sampling 199 Hz for 25 s inside the measurement windows. The production
build does not contain the probe; the probed builds measured within their rungs' normal bands,
so the numbers below describe the ordinary server.

**Runs** (same box, harness and staging as `../2026-08-29-two-core`, `--two-core`, 200 players):

| rung | serverCores this run (recorded band) | samples | profile |
|---|---|---|---|
| all-iroh 200 | 0.587 (0.51–0.61) | 1951 | `rust-iroh200.folded/.svg` |
| legacy 200   | 0.371 (0.36–0.38) | 1023 | `rust-legacy200.folded/.svg` |

Sampling is CPU-time based, so sample mass ratio ≈ user-CPU ratio: 1951/1023 = 1.9×.
SIGPROF cannot see time spent inside blocked syscalls; the `/proc` sidecar (`sidecar.snap`,
8 s cadence: per-thread utime/stime, UDP counters) fills that in. `foldbuckets.py` and
`allocwho.py` are the exact analysis scripts; every number below reproduces from the
artifacts in this directory.

## Where the CPU goes

**All-iroh 200 (0.587 cores)** — per-thread from the sidecar over the steady window
(19:33:02→19:33:47 of the first full rerun, RESULT 0.614): the single `basis-iroh` tokio
worker 46.6 % of a core (13.4 points of that in syscalls), the reduction send pass
(`bsr-send-0`) 10.8 %, tick loop 0.8 %. Inside the worker, by samples:

| component | % of process samples |
|---|---|
| noq (iroh's quinn fork) — inclusive | **59.0 %** |
| … of the worker's driver mass: transmit-cycle assembly (`poll_transmit`/`populate_packet`/frame writes, AEAD seal inside) | 47 % of driver ≈ **27 % of process** |
| … event/channel plumbing (ConnectionEvent mpsc, wakes) | 22 % of driver ≈ **13 %** |
| … unclassified driver poll loops | ≈ 11 % |
| … udp send path in-driver (`quinn_udp`/`netwatch`) | ≈ 4 % |
| … timers / ack-loss-cc / multipath `PathId` btrees | 1.7 % / **0.2 %** / 0.4 % |
| our transport glue (`sender_task`, enqueue) | 10.4 % |
| our datagram receive task | 0.2 % |
| game receive handlers running on the worker | 5.0 % |
| allocator (alloc/free in the innermost 3 frames, process-wide) | **18.5 %** |
| AEAD/crypto total (ring, AES-NI in use) | **5.1 %** |

**Legacy 200 (0.371 cores)**: `bsr-send-0` 60.2 % of samples (delta build + LNL merge/stamp:
`checked_increment` 11.3 %, `build_delta` 3.7 %, `with_payload` 3.0 %), `basis-lnl-logic`
25.1 %, LNL's async socket tasks on the shared tokio runtime 12.4 %, tick 2.2 %. Allocator
share 16.4 %; crypto 0.3 %; noq 0.4 %.

A correction this profile forced: the "basis-iroh" threads burning ~0.05 cores during pure
legacy runs are **not** an idle iroh endpoint — 92 of those 127 samples are the LNL
`net_manager` futures polled on the shared runtime (the threads are merely named basis-iroh).
The genuinely idle iroh endpoint costs ≈ nothing.

**Packets** (box-wide `/proc/net/snmp`, same delivered traffic): legacy 26.2 k UDP/s each way;
all-iroh 36.6 k/s — **+40 %, almost all ACK-only packets**, since QUIC datagrams are
ack-eliciting. The server's outbound data packets are already minimal (one per peer per tick ≈
16.7 k/s; quinn packs queued datagram frames, which is why the earlier `basis/2` app-layer
coalescing cut frames 7× and moved nothing). Also visible: `RcvbufErrors` tick during iroh
joins on this box's clamped 416 KB sockets; zero on legacy.

## The answer

The gap is **per-packet transmit-cycle and bookkeeping cost inside the quinn fork's
connection driver, amplified by allocator churn and ACK traffic — not cryptography** (5 %,
with AES-NI), not ack/loss/congestion algorithms (0.2 %), not the multipath fork overhead
(0.4 %), not our glue. Legacy transmits a packet by stamping a reusable buffer and calling
`sendto` (~7 µs user-side per packet all-in); noq builds each packet through
`poll_transmit` — assemble, frame, seal, per-transmit buffers — behind a per-connection
event channel (~21 µs user-side per packet), and then also processes ~10 k/s inbound
ACK-only packets that LiteNetLib's unreliable path simply never sends.

## Practical big swings, ranked by evidence

1. **Swap the global allocator (jemalloc or mimalloc).** 18.5 % of iroh-run samples and
   16.4 % of legacy-run samples are in glibc malloc/free paths; alloc frames appear in 76.6 %
   of iroh stacks (noq allocates per transmit and per event). One line + one dep, helps both
   paths, measurable in an afternoon. Expected: mid-single-digit points off both rungs.
2. **ACK economy, configured on both ends.** Raise `max_ack_delay` / ACK-eliciting threshold
   on the **clients** as well as the server (the earlier "ack frequency changed nothing"
   experiment predates this profile and plausibly configured only the server; the packet
   counters now show where the 10 k/s extra packets are). Every ACK-only packet the crowd
   doesn't send is a full receive cycle the server doesn't run. Gate on `/proc/net/snmp`
   deltas: target all-iroh ≤ ~28 k pkt/s from 36.6 k.
3. **Stop `send_datagram_wait`-ing per frame.** Unreliable frames should not await buffer
   space (LNL semantics are drop-on-full); use the non-waiting `send_datagram` and drain the
   peer queue in one pass per wake. Removes one future/poll round-trip per frame (16.6 k/s)
   through the 13 % event-plumbing slice. Cheap to try; measure before keeping.
4. **On real hosts: GSO + bigger socket buffers.** 29 % of the worker's time is syscalls.
   *(Corrected 2026-08-29: this kernel **does** offer GSO and GRO — measured with a real
   segmented send, not inferred from the missing `/proc/sys/net/core`. So the syscall share
   is not a missing-offload artefact. What is unverified is whether quinn takes the offload
   up, which needs `strace` on a host that permits ptrace.)* The socket buffer clamp is real:
   iroh asks for 7 MiB and gets 416 KB here, which is what caused the join-phase receive
   drops. See `irohplan.md` for the runbook.
5. **The many-core run remains the decider for the ratio itself.** noq's cost is per
   connection on a work-stealing runtime and will spread across 32 cores; legacy's
   `lnl-logic` receive thread will not. The 2-core ratio is iroh's worst case. (Unchanged
   plan; this profile tells that run what to look at.)
6. **Upstream conversation with n0/iroh.** With ack/cc at 0.2 % and multipath at 0.4 %, the
   fork is not the problem — but 27 % of process CPU in `poll_transmit` assembly for
   sub-MTU datagram traffic is a workload worth reporting upstream; a transmit fast path for
   datagram-only connections (or reusable transmit buffers) would move this server more than
   anything else on this list.

Not worth doing, per this profile: hardware-crypto work (already 5 %), app-layer datagram
coalescing (`basis/2`, retired — quinn already packs), server-only ACK tuning and MTU
discovery on this box (tried, nothing), multipath surgery (0.4 %).

## Reproducing

```
cargo build --release -p basis_network_console --features pprof
cp target/release/basis_network_console <workdir>/servers/rust/BasisNetworkConsole
BENCH_SERVERS=rust BENCH_MIX=0 BASIS_PPROF="25:/tmp/prof/iroh200" \
  BASIS_PPROF_TRIGGER=/tmp/prof/trigger benchmarks/run-comparison.sh <workdir> 200 &
# touch /tmp/prof/trigger when the live per-rung log under <workdir>/results/*/ first
# prints "window 1/"; the probe writes .folded/.threads/.svg ~25 s later.
python3 foldbuckets.py iroh200.folded ; python3 allocwho.py iroh200.folded
```
