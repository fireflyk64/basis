# Working the iroh plan: measured results

One directory per item of `../../../irohplan.md`, in the order they were done. Every arm was
measured on the same two-core rig and the same harness as `../2026-08-29-two-core`, with the
arms **interleaved** (A, B, A, B …) because this box drifts: the crowd's own CPU rose from 0.57
to 0.70 cores across one session, and any A-then-B comparison would have read that as a result.

## How to read a number on this rig — the instrument, first

The all-iroh 200-player rung **cannot resolve an effect below about 20 %** in `serverCores`.
Seven runs of one unchanged binary spread 0.391–0.551 cores. The reason is in the data: the
delivered packet rate is itself a free variable (16.8k–23.6k packets/s across identical runs,
because ACK timing and packet packing shift with scheduling), and server CPU tracks it almost
exactly. The crowd sits at 0.57–0.70 of its own core at that rung — close enough to saturation
that its jitter feeds back into the server's timing. The legacy rung, whose crowd sits at 0.33
cores, is by contrast rock steady (0.363–0.373 across six runs).

So the statistic to compare arms on is **server CPU per delivered packet** (`serverCores ÷
datagramsPerSec`, in µs). It is stable where `serverCores` is not: 23.3, 23.7, 23.0 µs across
three runs of the same binary whose whole-run CPU read 0.391, 0.489 and 0.460 cores.

`summarise-arms.py` prints both, plus every paired difference:

```
python3 summarise-arms.py allocator-runs.tsv
```

## Item 1 — the global allocator (adopted: mimalloc)

`allocator-runs.tsv`: three arms (glibc, mimalloc, jemalloc), all-iroh 200 and legacy 200,
interleaved, 4×15 s windows for the first ten runs and 6×20 s for the last four.

| arm | µs/packet, paired vs glibc | whole-run cores, paired | resident |
|---|---|---|---|
| **mimalloc** | −7.8, −1.0, −5.9, −5.0, +0.3, −2.7, −3.8 → **mean −3.7 %** (6 of 7 negative) | mean −1.7 %, but the range is −19 % … +37 %: noise, not signal | **+19 MB** (107 vs 88) |
| jemalloc | −6.8, −3.4 → mean −5.1 % | **+15 %** — it drove 24.2k packets/s against glibc's 20.0k | −7 MB |

Legacy rung, same comparison: mimalloc −1.1 % µs/packet, jemalloc +1.0 %; both within that
rung's own ±1 % spread. Delivery, Hz/pair, drops and voice were identical on every run of every
arm (83.33 Hz/pair, delivery 1.000, zero drops, voice ≥ 98.8 %).

**Adopted mimalloc**, as a default feature of the server binary
(`--no-default-features` restores the system allocator for heaptrack/valgrind/ASAN).
**Rejected jemalloc**: its per-packet cost was fine, but it generated a fifth more packets and
cost 15 % more CPU per run — for a workload whose cost *is* packets, that is the number that
matters. Its dependency was removed rather than left in the tree.

The honest size of this win is 3.7 % of the iroh path's CPU, not the double digits the first
single run suggested. It is worth having and it is not the answer to the iroh gap; the profile's
18.5 % allocator share is mostly work the transmit path asks for, not glibc being slow.

## Item 3 — drain the datagram queue per wake, send without awaiting (kept)

`sender_task` used to pop one frame, `await` `send_datagram_wait` on it, and loop: a future poll
round trip per frame, ~16.6k of them a second at 200 players, and — because
`send_datagram_wait` waits for buffer space — a policy of holding stale frames ahead of fresh
ones, which is the opposite of what unreliable traffic wants. It now drains both queues (voice
first, then bulk) under one lock acquisition each into a reused batch, and pushes each frame
with the non-waiting `send_datagram`, whose own policy is to drop the *oldest* queued datagrams
to make room — the policy our own per-peer queues already use and the one LiteNetLib's
unreliable path has always had.

Sized alongside it: the connection's datagram send buffer, 4 MiB → 256 KiB. That buffer is
where a stalled path's backlog now lives, and 4 MiB is ~2800 MTU-sized frames — half a minute
of one peer's traffic, 800 MB across a full room, and all of it stale state nobody wants
delivered. 256 KiB is a second or two.

**End-to-end: no measurable change at this scale.** Four paired long-window runs, all-iroh 200
(`item3-runs.tsv`; one baseline run was lost to a flaky join — 199 of 200 clients — on the
*unmodified* binary, the only such failure in ~30 runs this session):

| paired run | µs/packet, item 3 vs item 1 |
|---|---|
| 1 | −4.0 % |
| 2 | +5.4 % |
| 3 | +4.6 % |
| | **mean +2.0 % — inside this rung's noise** |

**Mechanism: confirmed.** Two 25 s in-process profiles, both arms on mimalloc so only item 3
differs (`item1-only.folded`, `item1-plus-item3.folded`, `compare-profiles.py`):

| share of process CPU | item 1 only | item 1 + item 3 |
|---|---|---|
| `Notify` / waker / wake plumbing | 3.78 % | **2.69 % (−29 %)** |
| `sender_task` path, total | 8.97 % | **8.40 % (−6 %)** |
| `send_datagram(_wait)` named frames | 0.90 % | 3.42 % |
| `poll_transmit`, noq inclusive, allocator, receive path | — | within the run's +5 % packet-rate difference |

The `send_datagram` line rising while the sender path's total falls is the change showing up in
the profile: the send work that used to be spread through future-polling scaffolding is now a
direct call inside `sender_task`. The wake plumbing is where the saving landed.

Kept, on three grounds: the mechanism did what it was meant to, the semantics now match the
LiteNetLib path this server also serves, and per-peer worst-case buffering drops 16×. What it
does not do is close the iroh gap — that cost is `poll_transmit`, which is item 6's subject.
Delivery was 1.0000 with zero drops on every run of both arms.

## Item 6 — the upstream report and its reproducer (written, not sent)

`../../../upstream/iroh-datagram-transmit-cost.md` is an issue-ready writeup for n0, and
`../../../upstream/iroh-datagram-cost/` is a standalone crate (its own workspace, no dependency
on this repository) that generates our traffic shape over iroh and over a plain UDP socket and
reports each side's CPU per datagram from `/proc/self/stat`.

**Nothing has been sent.** Posting it is a human's call and it should get a second read first.

Running it here (`reproducer-output.txt`) reproduces the gap outside the application entirely —
200 connections, 83 Hz each way, 500-byte payloads, each process pinned to its own core:

| | plain UDP | iroh 1.1 datagrams |
|---|---|---|
| server CPU per datagram | 7.6–9.6 µs (median **8.3**) | 19.8–20.0 µs (median **20.0**) |
| server CPU at that load | 0.24–0.32 cores | **0.57–0.58 cores** |
| datagrams carried | 16.2k–16.6k/s — the full offered rate | 14.2k–14.7k/s |

**2.4× the CPU per packet, and it does not keep up with the offered rate while a plain socket
does.** That is the same conclusion the application benchmark reached (~7 µs vs ~21 µs), with
no game logic in the process, which is what makes it worth sending upstream.

It is also a much better instrument than the application rung: its five-second windows are
stable to ±1 %, against ±25 % run-to-run for `serverCores` at all-iroh 200. Future transport
work should be measured here first and confirmed end-to-end second.
