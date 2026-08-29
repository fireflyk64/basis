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
| 1 | **Swap the global allocator** (mimalloc/jemalloc) in the server binary | 18.5 % of iroh samples and 16.4 % of legacy samples are in glibc malloc/free; alloc frames appear in 76.6 % of iroh stacks — noq allocates per transmit and per event | **done** — mimalloc adopted, −3.7 % CPU per packet, +19 MB resident; jemalloc measured and rejected |
| 3 | **Stop `send_datagram_wait`-ing per frame**: drain the peer's queue in one pass per wake and send without awaiting (drop-on-full is LiteNetLib's own unreliable semantics) | removes one future/poll round trip per frame (16.6 k/s) from the 13 % event-plumbing slice | **done** — wake plumbing −29 %, sender path −6 %; no end-to-end change at this scale; per-peer buffering bound cut 16× |
| 6 | **Report the transmit cost upstream to n0/iroh** with a standalone reproducer | 27 % of a game server's CPU in `poll_transmit` assembly for sub-MTU datagram traffic; a datagram fast path or reusable transmit buffers would move this server more than anything on this list | **written, not sent** — `upstream/`; the reproducer measures 20.0 µs/datagram over iroh against 8.3 over plain UDP (2.4×) |
| 2 | **ACK economy, configured on both ends** — raise the ACK-eliciting threshold and `max_ack_delay` on the clients as well as the server | the +40 % packet count above; every ACK-only packet the crowd doesn't send is a receive cycle the server doesn't run. The earlier "ack frequency changed nothing" trial predates this profile and configured the server only | **done** — −2.8 % packets, −1.5 % cores, best tick and voice of three arms. The premise was wrong in an instructive way: noq clamps `max_ack_delay` to max(RTT, 25 ms), so on loopback the default is *already* timer-bound at 25 ms and no threshold binds. The lever needs a real-RTT path |
| 4 | **On real hosts: GSO and larger socket buffers** | ~29 % of the network worker is syscalls; the 416 KB `rmem` clamp here drops packets during iroh joins | **half done, and the premise was wrong** — this box *does* perform GSO and GRO (measured, not assumed: see below). The socket-buffer clamp is real and is now reported loudly at boot and in `/health`. Whether quinn *uses* the offload needs `strace` on a host that permits ptrace |
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

## A correction this work forced: this box does have GSO

Three documents in this repository said the benchmark sandbox offered no UDP segmentation
offload, so "every packet is its own `sendmsg`". That was inferred from `/proc/sys/net/core`
being unreadable here, never measured — and it is wrong. A probe that sets `UDP_SEGMENT` and
then checks what actually arrives (`transport::host_udp_capabilities`) reports:

```
UDP offload: GSO yes (up to 64 packets per sendmsg), GRO yes.
Socket buffers: asked 7 MB, granted 416 KB receive / 416 KB send.
```

The send is genuinely segmented — 2400 bytes sent with `UDP_SEGMENT = 1200` arrives as two
1200-byte datagrams — so this is not a kernel that merely accepts the flag. Two consequences:

* **Every measurement in this repository was taken on a host that offers GSO.** The 29 % of the
  network worker spent in syscalls, the 2.4× cost per datagram against a plain socket, the
  `poll_transmit` share — none of them is an artefact of missing segmentation offload, which
  makes the upstream case in item 6 stronger rather than weaker.
* **What remains unknown is whether quinn takes the offload up**, which needs a syscall count
  (`strace -c`) and therefore a host that permits `ptrace`; this sandbox refuses it to root.
  The runbook below is that measurement.

The socket-buffer half of item 4 stands and is worse than it looked: iroh's sockets ask the
kernel for 7 MiB (netwatch's `SOCKET_BUFFER_SIZE`), are granted 416 KB here, and iroh logs that
refusal at `debug` level where no operator sees it. The server now says so at boot, at error
level, in the same words the LiteNetLib path already used, and reports it in `/health`.

## Runbook: measuring item 4 somewhere else

Everything here is host-dependent, so it is written to be run on the machine under test rather
than reasoned about from here.

**1. What the host offers** — no load needed, and it is in the boot log:

```
grep -E 'UDP offload|clamped' logs/*.log
curl -s http://<host>:10666/health | python3 -m json.tool | grep -A 9 hostUdp
```

`hostUdp.gso` is a *verified* segmented send, not a flag check. `grantedReceiveBufferBytes`
against `requestedSocketBufferBytes` is the clamp, and `udpReceiveBufferDrops` is the running
count of datagrams the kernel threw away for want of that buffer — if it climbs during a run,
the run was buffer-bound and its CPU numbers describe a bottleneck, not the transport.

**2. Raise the buffers, then re-run.** The grant is `min(request, net.core.rmem_max)`:

```
sudo sysctl -w net.core.rmem_max=8388608 net.core.wmem_max=8388608   # persist in /etc/sysctl.d
```

Re-read `/health`: `grantedReceiveBufferBytes` should now be near 7 MiB (Linux reports roughly
double what it granted). A benchmark before and after this change, with `udpReceiveBufferDrops`
captured both times, is the whole experiment.

**3. Does QUIC actually batch its sends?** This is the open question, and it needs `ptrace`:

```
strace -c -f -p $(pgrep -x BasisNetworkConsole) &   # let it run ~10 s, then interrupt
```

Compare `sendmsg` + `sendmmsg` calls per second against `packetsSent` per second from
`/health`. One-to-one means the offload is not being taken up despite the host offering it —
which would be worth reporting upstream. Several packets per call is GSO working, and then the
syscall share of the profile is already as low as this workload can drive it.

**4. The transport on its own.** `upstream/iroh-datagram-cost/` needs no game server and its
five-second windows are stable to ±1 %, so it is the cheapest way to compare two hosts, or one
host before and after a sysctl change.
