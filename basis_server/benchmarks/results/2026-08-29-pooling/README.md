# Packet buffer pooling: targeted benchmarks (2026-08-29, two-core box)

The `pooling` branch removes the per-message heap traffic `noholdsbarred.md` flagged — "a `Vec`
per datagram, a `Vec` per merged entry, a `Bytes` copy per send" — by circulating fixed-capacity
buffers through `basis_network_core::pooling::PacketBufferPool`. The MTU makes the size planable:
every datagram-shaped allocation fits one 2048-byte class (the MTU ladder tops out at 1432 + a
header; the receive buffer is exactly 2048), so buffers of one size recycle instead of hitting
the allocator, and anything larger falls back to a plain allocation transparently.

These are the *targeted* benchmarks on the pooling itself, runnable on this 2-core box. They do
not replace the end-to-end server comparison in `../2026-08-29-two-core/`; they answer one
question: what does a packet-shaped operation cost with and without the pool, on the same
build, with both arms exercising the real code paths in one binary?

## What changed on the packet paths

| path | before | after |
|---|---|---|
| LNL datagram receive | `Vec` per datagram (`to_vec`) | pooled copy, recycled when the app drops its reader |
| LNL merged datagram | a `Vec` **per entry** + synthetic packet | one pooled buffer; entries delivered as zero-copy views |
| LNL send (every unreliable/reliable build) | zeroed `Vec` per packet + copy over the zeros | pooled frame: zero the header only, copy the payload once |
| LNL channel deliver list | `Vec` per channeled datagram | per-thread scratch, reused |
| LNL stamped resend copy | `to_vec` per differing header | pooled copy |
| iroh reliable enqueue | `Bytes::copy_from_slice` per send | pooled copy behind refcounted `Bytes` |
| iroh unreliable frame | `BytesMut` alloc per datagram | pooled frame behind refcounted `Bytes` |
| iroh stream frame receive | zeroed `Vec` per frame | pooled (zeroed) buffer, recycled after the handler |

## Heap allocations per operation (counting global allocator, 100k iterations, steady state)

`cargo bench -p basis_network_core --bench allocations_per_packet`

| scenario | allocs/op | bytes/op |
|---|---:|---:|
| receive 1200 B — unpooled | 1.000 | 1200 |
| receive 1200 B — **pooled** | 1.000 | **40** |
| send 120 B — unpooled | 1.000 | 122 |
| send 120 B — **pooled** | **0.000** | **0** |
| merged datagram, 8 entries — unpooled | 9.000 | 1953 |
| merged datagram, 8 entries — **pooled** | **1.000** | **40** |
| iroh frame 1200 B — unpooled | 1.000 | 1203 |
| iroh frame 1200 B — **pooled** | 1.000 | **40** |

The residual 1 alloc / 40 B on the delivery paths is `Bytes::from_owner`'s shared-state box —
bookkeeping, not payload; the payload buffer itself no longer touches the allocator. The send
path is allocation-free. Pool self-report across the whole run: `reused_local=403999
allocated=1 dropped_full=0` — one real allocation for four hundred thousand packet operations,
everything else served by the thread-local fast path.

## Time per operation (criterion, both arms are the real code, same binary)

`cargo bench -p basis_network_core --bench packet_pooling` — medians, this box (2 cores):

| shape | pooled | unpooled | pooled is |
|---|---:|---:|---|
| copy 200 B | 34.0 ns | 12.7 ns | 2.7× slower |
| copy 1200 B | 41.4 ns | 72.2 ns | **1.7× faster** |
| copy 1432 B (MTU) | 43.4 ns | 71.8 ns | **1.7× faster** |
| zeroed 1432 B | 44.5 ns | 66.7 ns | **1.5× faster** |
| send frame (10 B header + 1200 B) | 54.0 ns | 95.7 ns | **1.8× faster** |
| receive→reader 200 B | 71.5 ns | 39.0 ns | 1.8× slower |
| receive→reader 1200 B | 83.6 ns | 99.6 ns | **1.2× faster** |
| send packet 120 B | 33.1 ns | 26.2 ns | 1.3× slower |
| send packet 1200 B | 54.5 ns | 89.2 ns | **1.6× faster** |
| merged decode, 8×120 B entries | 300 ns (27.4 M entries/s) | 373 ns (21.1 M entries/s) | **1.24× faster** |
| both cores churning 1200 B | 34.2 ns | 31.6 ns | parity |

### Reading these honestly

* **The microbenchmark is malloc's best case, not the server's.** A tight same-size
  alloc→free→alloc loop replays glibc's per-thread tcache head — ~13 ns and immune to
  fragmentation. A live server frees on different threads than it allocates (receive task →
  app handler), interleaves hundreds of other sizes between rent and return, and grows RSS
  under churn. The pool's numbers above are its *worst-case relative* showing, and it still
  wins every MTU-sized shape by 1.2–1.8×.
* **Small payloads pay a fixed ~20 ns** (thread-local lookup + RefCell + accounting) that
  tcache's replay undercuts. At 4000 players × ~50 packets/s × a few buffer ops each, the
  difference between 34 ns and 13 ns is under 1% of one core — while the 1.7× win on
  MTU-sized traffic (voice fan-out, movement fan-out, merged datagrams) is on the dominant
  byte volume. If small-payload cost ever matters, the accounting counters are the first
  thing to strip.
* **Contention is solved structurally, not won at the bench.** The two-level design (per-thread
  stack of 32 buffers over 8 lock-free shards, statistics accumulated per-thread and folded in
  every 1024 ops) means the steady-state rent/recycle touches no shared cache line at all;
  the both-cores arm above confirms parity with tcache under simultaneous churn, where a
  single-level sharded design measured 3× worse before the thread-local layer landed.
* **Bytes/op collapsing 30×** (1200 → 40) is the number that matters at 4000 players on 32
  cores: it is the difference between the allocator walking fresh pages under fan-out load and
  the same 16 MB of warm buffers circulating.

## The safety argument (why reuse cannot deliver one customer's bytes to another)

* A buffer re-enters the pool in exactly one place: `PooledBytes::drop`. When Rust runs a
  drop, no other reference exists — the same ownership proof that made `free` safe before.
  Rents and recycles *move* the `Vec`; safe Rust cannot construct a second owner, and the
  module is `#![forbid(unsafe_code)]` so that stays true by compiler fiat.
* Delivery shares the buffer through `bytes`' refcount and drops the owner once, after the
  last reader — the identical mechanism the unpooled code already trusted.
* Stale *contents* are closed by construction: every rent either zeroes what it returns or
  copies the caller's bytes over the full length. The one API that trusted callers to
  overwrite (`rent_for_overwrite`) was deleted before merge; the iroh stream path eats a
  bounded memset instead. Tests pin this: a dirty recycled buffer must come back zeroed, and a
  4-thread canary stress asserts no foreign tag ever appears in a rented buffer.

## Bounds

The pool is a reservoir, never an owner: at most 8 shards × 1024 buffers × 2 KB = 16 MB,
enforced by the fixed-capacity queues themselves (a recycle into a full shard frees), plus
64 KB per live thread in the local stacks, drained on thread exit. It starts empty.

## Follow-ups (not in this change)

* `create_receive_event` still allocates one `Arc<LnlNetPeer>` per delivered message (~48 B);
  caching the handle per peer needs a cycle-free home and is the next allocation to fall.
* `NetDataWriter` on the application side (reduction system) is already pooled at the message
  level (`QueuedMessagePool`); its inner buffers were not touched.
* Validate on the many-core box alongside the planned end-to-end run; on 2 cores the
  end-to-end benchmark measures scheduling more than allocation.

## Regression check: end-to-end on the two-core rig (same day, post-merge)

The pooled build was re-run through the full server comparison at 100/200 players — legacy and
all-iroh crowds — against fresh C# baselines, with a same-hour A/B against the pre-pooling
`developer` console at the one rung that moved.

| rung | recorded (pre-pool) | pooling pass 1 / 2 | developer, same hour | verdict |
|---|---|---|---|---|
| C# legacy 100 / 200 | 0.231 / 0.405 | 0.227 / 0.408 | — | baseline reproduces ±2 % |
| Rust legacy 100 | 0.276 | 0.272 | — | no regression |
| Rust legacy 200 | 0.363 | 0.382 / 0.377 | **0.369 / 0.377** | ≤ 2 % on overlapping samples — box drift, not the pool |
| Rust all-iroh 100 | 0.322 | 0.317 | — | no regression |
| Rust all-iroh 200 | ~0.70 | 0.529 / 0.611 | — | no regression (rung swings ±8 %; both runs below the record) |

Delivery 1.0, 83.33 Hz/pair, zero drops, voice ≥ 99.3 %, zero overrun on every run of every
build. The one real, expected cost: resident memory is up ~10 MB at 100 players and ~20–24 MB
at 200 (46.7 / 74.9–80.0 MB vs 37 / 55–56 MB) — the bounded reservoir plus 2 KB-capacity
buffers standing in for smaller exact-size ones while queued — and tick time at 200 reads
2.24 ms vs 2.03–2.10 ms on the A/B (overrun still 0). As predicted, a one-core box cannot see
the allocation win end-to-end; the pool's case at scale rests on the microbenchmarks above and
the many-core run.
