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
