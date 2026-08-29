# The iroh performance plan

Why the iroh path costs the server ~1.6× the LiteNetLib path at 200 players on two cores,
what to do about it, and what has already been ruled out. Every claim below comes from the
symbol-level profile in `benchmarks/results/2026-08-29-iroh-profile/` (in-process SIGPROF
sampler, 199 Hz, sampled inside the measurement windows of a live 200-player rung) and the
`/proc` sidecar taken beside it. Numbers are from the two-core rig; ratios, not absolutes,
are the result.

## The finding

All-iroh 200 players: **0.587 cores**. Legacy (LiteNetLib) 200 players: **0.371 cores**.

| where the iroh CPU goes | % of process CPU |
|---|---|
| **noq** (iroh's quinn fork), inclusive | **59 %** |
| — per-packet transmit-cycle assembly (`poll_transmit` → `populate_packet` → frame writes → seal) | ~27 % |
| — connection-event / channel plumbing (mpsc `ConnectionEvent`, wakes) | ~13 % |
| — unclassified driver poll loops | ~11 % |
| allocator (alloc/free in the innermost frames) | **18.5 %** (16.4 % even on the legacy path) |
| syscalls (from the sidecar; SIGPROF cannot see blocked time) | ~29 % of the network worker |
| our transport glue (`sender_task`, enqueue) | 10.4 % |
| game receive handlers running on the worker | 5.0 % |
| **crypto (ring, AES-NI in use)** | **5.1 %** |
| ack / loss / congestion algorithms | **0.2 %** |
| multipath (`PathId`, `cid_queue`) bookkeeping | **0.4 %** |

Box-wide UDP counters, same delivered traffic: legacy **26.2 k packets/s**, all-iroh
**36.6 k/s** — **+40 %**, essentially all ACK-only packets, because every QUIC datagram is
ack-eliciting and LiteNetLib's unreliable path acknowledges nothing.

**So the gap is per-packet transmit machinery and packet count, not cryptography, not the
congestion controller, not the multipath fork, and not our code.** LiteNetLib stamps a
reusable buffer and calls `sendto` (~7 µs user-side per packet all-in); noq builds each
packet through `poll_transmit` behind a per-connection event channel (~21 µs), then also
processes ~10 k/s inbound ACK-only packets that never exist on the legacy path.

## The plan, ranked by evidence

| # | change | evidence | status |
|---|---|---|---|
| 1 | **Swap the global allocator** (mimalloc/jemalloc) in the server binary | 18.5 % of iroh samples and 16.4 % of legacy samples are in glibc malloc/free; alloc frames appear in 76.6 % of iroh stacks — noq allocates per transmit and per event | — |
| 3 | **Stop `send_datagram_wait`-ing per frame**: drain the peer's queue in one pass per wake and send without awaiting (drop-on-full is LiteNetLib's own unreliable semantics) | removes one future/poll round trip per frame (16.6 k/s) from the 13 % event-plumbing slice | — |
| 6 | **Report the transmit cost upstream to n0/iroh** with a standalone reproducer | 27 % of a game server's CPU in `poll_transmit` assembly for sub-MTU datagram traffic; a datagram fast path or reusable transmit buffers would move this server more than anything on this list | — |
| 2 | **ACK economy, configured on both ends** — raise the ACK-eliciting threshold and `max_ack_delay` on the clients as well as the server | the +40 % packet count above; every ACK-only packet the crowd doesn't send is a receive cycle the server doesn't run. The earlier "ack frequency changed nothing" trial predates this profile and configured the server only | — |
| 4 | **On real hosts: GSO and larger socket buffers** | ~29 % of the network worker is syscalls and this kernel offers no GSO, so every packet is its own `sendmsg`; the 416 KB `rmem` clamp here also drops packets during iroh joins | deferred — needs a real host |
| 5 | **The many-core run remains the decider for the ratio itself** | noq's cost is per connection on a work-stealing runtime and spreads across cores; LiteNetLib's single receive thread does not. Two cores is iroh's worst case | deferred — the user's own next step |

## Ruled out by measurement — do not respend here

* **Application-layer datagram coalescing** (`basis/2`, implemented and then retired): cut the
  application frame count 7× and moved CPU by nothing, because quinn already packs queued
  datagram frames into packets. Packet count, not frame count, is the cost.
* **Server-only ACK-frequency tuning and MTU discovery** on this box: tried, nothing measurable.
  Item 2 is the both-ends version, which is a different experiment.
* **Cryptographic work** (ChaCha20 vs AES, hardware checks): crypto is 5.1 % with AES-NI already
  in use.
* **Multipath / fork-specific surgery**: 0.4 %.

## One correction the profile forced

The `basis-iroh`-named threads that burn ~0.05 cores during pure legacy runs are **not** an idle
iroh endpoint: 92 of those 127 samples are the LiteNetLib `net_manager` futures polled on the
shared tokio runtime (the threads merely carry that name). A genuinely idle iroh endpoint costs
approximately nothing.
