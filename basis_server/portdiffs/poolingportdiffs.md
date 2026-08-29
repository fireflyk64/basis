# Pooling — port diffs

C#: `Basis Server/BasisNetworkCore/Pooling/` · Rust: `basis_server/basis_network_core/src/pooling/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BasisByteArrayPooling.cs` | `basis_byte_array_pooling.rs` | 51 → 35 | faithful |
| `ThreadSafeMessagePool.cs` | `thread_safe_message_pool.rs` | 23 → 34 | deviates |
| — | `mod.rs` | — → 5 | extended (Rust module wiring, no C# analogue) |

Both pools exist in Rust with the same member set (`rent`/`return`/`clear`, and `rent`/`return`).
`BasisObjectPool<T>`, which the Rust pooling test file also covers, lives in `compression/` on both
sides and is outside this file map.

## Deviations

1. **The message pool is no longer used in production.** The C# rents a pooled
   `AudioSegmentDataMessage` on both voice paths —
   `Basis Server/BasisNetworkServer/Core/BasisServerHandleEvents.cs:869` and `:903`, returned at
   `:880` and `:936`. The Rust handler allocates a fresh message instead:
   `basis_server/basis_network_server/src/core/basis_server_handle_events.rs:825` and `:843` are
   `AudioSegmentDataMessage::default()`, with no `ThreadSafeMessagePool` call anywhere outside the
   test file. Why: this is the deliberate no-GC decision the brief anticipates — the C# pooled to
   keep allocation pressure off the collector, and Rust frees deterministically. It is still a
   behavioural difference worth recording, because the C# was not only recycling the object but
   also its already-grown internal buffer, so the Rust does one allocation per voice packet on a
   path that runs about 50 times a second per speaker. Consequence for this audit: the Rust
   `ThreadSafeMessagePool` is exercised only by
   `basis_server/basis_server_tests/tests/infrastructure/pooling_tests.rs:260-353`, never in
   production. `BasisByteArrayPooling` is unused outside tests on *both* sides, so it is not part
   of this deviation.

2. **Message reuse order flipped from FIFO to LIFO.** `ThreadSafeMessagePool.cs:7` is a
   `ConcurrentQueue<T>`, drained with `TryDequeue` (`:12`) and filled with `Enqueue` (`:19`) —
   first in, first out. `thread_safe_message_pool.rs:6` stores a `Vec<Box<dyn Any + Send>>` and
   uses `pop()` (`:18`) / `push()` (`:31`) — last in, first out. No correctness consequence (the
   pool holds interchangeable instances) and LIFO is the better cache behaviour, but it changes
   which instance a renter gets and, when the cap is reached, which instances survive. Note the
   byte pool did *not* make this switch: `basis_byte_array_pooling.rs:15,26` use
   `pop_front`/`push_back` on a `VecDeque`, matching the C# `Queue<byte[]>`
   (`BasisByteArrayPooling.cs:19,38`). Not pinned — neither suite asserts message-pool ordering.

3. **Per-type lock-free queues collapsed into one global mutex.** In C#, `ThreadSafeMessagePool<T>`
   is a generic static class (`ThreadSafeMessagePool.cs:5-7`), so each closed type `T` gets its own
   `ConcurrentQueue` static — different message types never contend, and the queue itself is
   lock-free. The Rust models the same "one pool per type" idea as a single
   `Mutex<Option<HashMap<TypeId, Vec<...>>>>` (`thread_safe_message_pool.rs:6`), so every rent and
   return of every message type serialises on one mutex, plus a hash lookup and a `Box` allocation
   per pooled item. Why: Rust has no per-monomorphisation static. Not pinned; see Corners cut.

4. **`Return(null)` has no counterpart.** `BasisByteArrayPooling.cs:28` explicitly no-ops on a null
   array, and the C# test `Return_Null_IsANoOp`
   (`Basis Server/BasisServerTests/Infrastructure/PoolingTests.cs:198-206`) pins it. In Rust
   `return_array` takes `Vec<u8>` by value, so the case cannot be expressed and the test has no
   counterpart. Structural, not a regression.

## Corners cut

Taking the brief's question directly — is the pooling that *remains* still correct?

* **Double-return: impossible, and that is the strongest result here.** In C# both
  `BasisByteArrayPooling.Return(array)` (`:26-40`) and `ThreadSafeMessagePool<T>.Return(obj)`
  (`:15-21`) take a reference and can be called twice with the same one; the array or object is
  then enqueued twice and handed to two renters simultaneously, and neither pool detects it. Both
  Rust functions take ownership (`basis_byte_array_pooling.rs:23`,
  `thread_safe_message_pool.rs:26`), so a double return does not compile. The concurrency tests at
  `pooling_tests.rs:204-258` and `:330-353` assert no aliasing, and by construction they cannot
  fail.
* **Leaks and unbounded growth: preserved, not fixed.** The byte pool has no cap on either side —
  `BasisByteArrayPooling.cs:32-38` and `basis_byte_array_pooling.rs:26` append to a per-size bucket
  forever, there is no eviction or trimming, and the only relief is an explicit `Clear()`
  (`:43-49` / `:30-34`). A workload that rents many distinct sizes accumulates a bucket per size
  permanently. Faithful, and a shared risk. The message pool caps at 500 per type on both sides
  (`ThreadSafeMessagePool.cs:8`, `thread_safe_message_pool.rs:12`) but has no `clear` on either
  side, so those 500 live for the process lifetime.
* **Rented buffers are not zeroed, and the Rust doc says they are.**
  `basis_byte_array_pooling.rs:7` reads "`rent` hands back a zeroed vector of exactly `size`
  bytes". Only the freshly created path is zeroed (`:20`); a pooled buffer comes back with the
  previous tenant's bytes intact — which the Rust's own test pins at `pooling_tests.rs:159-174`
  ("The pool does not zero buffers; stale-data hygiene is on the caller"). The C# has the same
  behaviour and does not claim otherwise. The doc comment is the corner: anyone trusting it will
  leak a previous packet's bytes into a new one.
* **The cap test is weaker than the C# one, and it is order-fragile.**
  `PoolingTests.cs:330-355` asserts *exactly* `MaxPoolSize` instances survive: it rents 500 and
  requires every one to be from the returned set, then requires the 501st to be brand new. The
  Rust counterpart (`pooling_tests.rs:308-328`) only asserts `from_returned <= MAX_POOL_SIZE` and
  `> 0`. The reason it had to be weakened is itself a corner: the C# gave each test its own private
  message type (`PoolingTests.cs:290-294`: `FreshMessage`, `ReusedMessage`, `StatefulMessage`,
  `CappedMessage`, `StormMessage`), so no test could see another's leftovers, whereas the Rust
  reuses one `StormMessage` for both the cap test (`pooling_tests.rs:314`) and the concurrency
  storm (`:340`). `#[serial(message_pool)]` orders them but does not reset the pool, and the pool
  has no `clear`, so the cap test starts with whatever the storm left behind and can only make an
  inequality claim. Adding a `clear` (or a distinct type per test) would let the exact assertion
  come back.
* **A silent-loss path in `rent`.** `thread_safe_message_pool.rs:17-22` chains
  `pool.pop()` and `boxed.downcast::<T>()`; if the downcast ever failed, the popped box would be
  dropped and the instance lost without a word. It is unreachable — the map is keyed by
  `TypeId::of::<T>()`, so the box is always a `T` — but it is a swallow rather than an
  `unreachable!`, which is also what keeps it inside the crate's no-panic policy.
* **Bucket key follows `len`, not capacity.** C# keys on `array.Length`, which is immutable for a
  `byte[]`. Rust keys on `array.len()` (`basis_byte_array_pooling.rs:26`), which the caller can
  change with `truncate`/`push` before returning. The pool's contract still holds — `rent(n)`
  always yields a vector with `len() == n`, pinned at `pooling_tests.rs:123-132` — but a vector
  returned after `truncate` is filed under the smaller bucket while retaining its larger capacity,
  so the accounting is by length and the memory held is by capacity.

## Improvements

* **Ownership makes double-return and use-after-return unrepresentable** — see above. This is the
  one real safety gain, and it removes a whole class of bug the C# pools are open to, including
  the C# voice path returning `audioSegment` at `BasisServerHandleEvents.cs:880` after handing it
  to `SendVoiceMessageToClients`.
* **The 500-item cap is now exact.** `ThreadSafeMessagePool.cs:17` tests `pool.Count < MaxPoolSize`
  on a `ConcurrentQueue`, where `Count` is a racy snapshot and the check is not atomic with the
  `Enqueue` that follows — concurrent returners can push the pool past 500.
  `thread_safe_message_pool.rs:30-32` performs the length check and the push under the same mutex,
  so the cap cannot be exceeded.
* **`Send` is required of pooled types** (`thread_safe_message_pool.rs:11`), so a non-thread-safe
  message cannot be parked in a pool that threads share; the C# `where T : new()` constraint says
  nothing about thread safety.
* **The Rust test suite is a superset.** It keeps every C# behaviour test and adds a concurrency
  storm for the message pool (`pooling_tests.rs:330-353`) alongside the byte-pool ones, plus a
  60-second join deadline (`:28-32`) so a deadlocked pool fails the run instead of hanging it.

## Verdict

The byte pool is a faithful port, right down to preserving FIFO order and the un-zeroed reuse; the
message pool is faithful in contract but differs in reuse order and trades per-type lock-free
queues for one global mutex. The absent pooling on the voice path is the deliberate no-GC call and
is defensible, but it leaves the Rust message pool as test-only code, so its LIFO/global-lock
choices are unvalidated under real load. The pooling that remains is correct — Rust's ownership
removes the double-return and aliasing hazards the C# is open to, and the cap is now exact — with
two things to fix: the `rent` doc comment claims buffers are zeroed when the pinned behaviour is
that they are not, and the cap test was weakened to tolerate cross-test pool pollution the C# did
not have.
