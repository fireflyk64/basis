# Statistics — port diffs

C#: `Basis Server/BasisNetworkCore/Statistics/` · Rust: `basis_server/basis_network_core/src/statistics/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `Statistics/BasisNetworkStatistics.cs` | `statistics/basis_network_statistics.rs` | 376 → 336 | deviates |
| — | `statistics/mod.rs` | — → 2 | Rust-only module glue |

Neither side has a test. `grep -rl BasisNetworkStatistics` over `Basis Server/BasisServerTests`
and over the Rust test crates returns nothing, so **every deviation below is unpinned**.

## Deviations

**1. The compression algorithm changed: Brotli → deflate. This breaks the shipping client.**
C# `BrotliCompress` / `BrotliDecompressToSpan` (`BasisNetworkStatistics.cs:356-373`) use
`System.IO.Compression.BrotliStream`. Rust `deflate` / `inflate`
(`basis_network_statistics.rs:264-281`) use `flate2`'s raw deflate. The Rust doc comment
states the reasoning (`:44-46`): "the C# used Brotli, which nothing decodes on the far side
but the same code; deflate keeps the dependency footprint to what the join batch already
needs."

That premise does not hold. The Unity client decodes this payload:
`Basis/Packages/com.basis.framework/Networking/BasisNetworkEvents.cs:737` calls
`BasisNetworkStatistics.Snapshot.Decode(Reader.GetRemainingBytesSegment(), true)`, and
`Decode(..., compressed: true)` routes to `BrotliDecompressToSpan`
(`BasisNetworkStatistics.cs:255-259`). The server sends the blob on
`ServerStatisticsChannel` = 35 in both ports (C# `BasisServerMessageRegistry.cs:342-350`;
Rust `basis_server_message_registry.rs:325-335`). A Rust server therefore hands the existing
C# client a deflate stream where it expects Brotli, and the client-side decode fails. Since
the encoded body is otherwise byte-identical (same varint format — compare
`BasisNetworkStatistics.cs:312-321` with `basis_network_statistics.rs:256-262`), the fix is
the compressor, not the format.

This is the one deviation in this module that changes an on-the-wire contract.

**2. The compression quality parameter was dropped, and the level effectively went up.**
C# `SnapshotResetEncode(bool compress = true, int brotliQuality = 6)`
(`BasisNetworkStatistics.cs:235`) and `EncodeCurrent` (`:245`) take a quality. The live caller
deliberately passes 1, with a comment explaining why
(`BasisServerMessageRegistry.cs:336-342`): "Quality 1, not 6: the snapshot is a few hundred
bytes of already-varint'd counters with the zero entries dropped, so q6's extra search finds
almost nothing while costing real CPU at the 10Hz poll rate." Rust
`snapshot_reset_encode(compress: bool)` (`basis_network_statistics.rs:198`) has no such
parameter and hardcodes `flate2::Compression::default()` (`:266`), which is level 6. The
caller (`basis_server_message_registry.rs:325`) has no way to ask for less. The C#'s explicit
CPU-cost decision at a 10 Hz poll rate was not carried over.

**3. Memory ordering was relaxed everywhere.** C# uses sequentially-consistent
`Interlocked.Increment` / `Interlocked.Add` on the hot path (`:63-64`, `:78-79`, `:99-100`),
`Volatile.Read` in `GetSnapshot` (`:123-126`), and `Interlocked.Exchange` in `Clear`
(`:177-180`). Rust uses `Ordering::Relaxed` for all of them
(`basis_network_statistics.rs:65-66`, `:76-77`, `:87-88`, `:104`, `:127-130`) and
`Relaxed` for the `IS_RECORDING_DATA` flag too (`:51`, `:55`).

For counters that are only ever summed this is sound — there is no cross-variable invariant
being relied on, and `swap`/`fetch_add` remain atomic per counter. The one visible
consequence: a reader can now see a stripe's count updated without its bytes, or the
recording flag flipped without the counters that follow it, in a way the C#'s stronger
ordering made harder. For statistics this is acceptable; it is recorded because it is a
deliberate, unpinned weakening.

**4. Stripe assignment uses a different input.** C# `PickStripe`
(`BasisNetworkStatistics.cs:198-211`) hashes `Thread.CurrentThread.ManagedThreadId`; Rust
(`:149-161`) hashes a global monotonically-increasing `NEXT_THREAD_ID` counter (`:35`, `:151`)
handed out on a thread's first record. Same three-round mixer, same constants, same
`% StripeCount`. Any assignment is valid — the stripe is only a contention-reduction
device — but the resulting distribution differs, and the C#'s property of a recycled managed
thread id landing back on the same stripe is gone. The Rust's counter is cast `as u32`
(`:151`), wrapping after 2^32 distinct recording threads; harmless.

**5. `Dictionary<byte, IndexStats>` → `BTreeMap<u8, IndexStats>`.** C# `:113-114`, `:145-146`;
Rust `:102-103`. Behaviourally this only changes iteration order in `WriteMap` /
`write_map` (`:290` / `:234`). Both loops happen to emit ascending indices in practice — the
C# builds the dictionary by ascending `i` with no removals — so the encoded bytes match; but
the Rust now *guarantees* it rather than relying on `Dictionary`'s unspecified order.

**6. Snapshot encoding is `int`-vs-`usize` at the record API.** C# `RecordInbound(byte index,
int bytesEncoded)` / `RecordOutbound` (`:54`, `:69`); Rust `record_inbound(index: u8,
bytes_encoded: usize)` / `record_outbound` (`:60`, `:71`). A negative `bytesEncoded` in C#
would drive a byte counter *backwards*; the Rust type makes that unrepresentable.
`record_outbound_batch` still takes `i64` on both sides (`:90` / `:82`) and both guard only
`count <= 0` (`:92` / `:83`), not a negative byte total — so that one path can still go
backwards in either port, identically.

## Corners cut

**`Snapshot.TotalCalls` / `OutTotalCalls` were never in the C#.** The C# XML docs promise them
three times (`BasisNetworkStatistics.cs:108-109`, `:141`) but the `Snapshot` class
(`:219-230`) has only `PerIndex` and `OutPerIndex`. The Rust *adds* the two accessors
(`basis_network_statistics.rs:189-195`), implementing what the C# documented. This is the
port being more complete than the original, not a corner cut — recorded here because a reader
diffing the two files will notice new public API.

**A consumer-side counter was dropped.** Not this module, but it changes what this module
counts: C# `BasisServerMessageRegistry.cs:349` records the statistics reply itself with
`RecordOutbound(ServerStatisticsChannel, writer.Length)` before sending; the Rust handler
(`basis_server_message_registry.rs:333-335`) sends without recording. Channel 35's outbound
count and bytes will read lower on the Rust server. Since the snapshot is reset immediately
before the send (`:325`), the C# was recording into the freshly-zeroed window, so the loss is
one message per poll — small, but a real difference in the numbers.

Everything else is present: `RecordInbound`, `RecordOutbound`, `RecordOutboundBatch`,
`GetSnapshot`, `SnapshotAndReset`, `Clear`, `SnapshotResetEncode`, `EncodeCurrent`, `Decode`,
the varint codec and the `SpanReader` all have counterparts. The two snapshot paths were
merged into one `collect(reset: bool)` (`:101-121`) where the C# had two near-identical loops
(`:111-136`, `:143-168`) — a deduplication, not a loss.

## Counter widths and overflow — checked

| | C# | Rust |
| --- | --- | --- |
| stripe cell | `long` (`:26-29`) | `AtomicI64` (`:11-14`) |
| cross-stripe sum | `long` (`:118-119`) | `i64` (`:106`) |
| exported | `ulong`, `unchecked((ulong)…)` (`:130`) | `u64`, `as u64` (`:114`) |
| index space | 256 (`:17`) | 256 (`:8`) |

Same widths, same reinterpretation of a negative `long` into a `ulong`. Nothing narrows.

Overflow: the per-stripe atomics wrap on both sides (`Interlocked.Add` is unchecked;
`fetch_add` wraps by definition). The cross-stripe accumulation differs — C# `inCount += …`
(`:123-126`) wraps under the default unchecked context, Rust `in_count += take(…)`
(`:108-111`) panics in a debug build and wraps in release. Reaching it needs ~9.2×10^18
counted bytes on one channel, so this is theoretical; noted because it is a genuine
difference in failure mode, not because it is reachable.

Going backwards: possible on both sides only through `record_outbound_batch` with a negative
`bytes_encoded` (see deviation 6). The single-record paths cannot go backwards in Rust and
could in C#.

## Improvements

* `basis_network_statistics.rs:271-284` — `inflate` bounds the decompressed size at
  `MAX_INFLATED_BYTES` (64 KiB) via `Read::take`, then rejects anything larger. The C#
  `BrotliDecompressToSpan` (`:366-373`) `CopyTo`s into an unbounded `MemoryStream`, so a
  crafted frame on the statistics channel could balloon server memory.
* `basis_network_statistics.rs:288-300` — decode failures are a `StatisticsDecodeError`
  value. The C# `SpanReader` throws `EndOfStreamException` (`:332`) and
  `InvalidDataException` (`:345`, `:351`) into whatever is parsing, and `Decode` (`:255`) does
  not catch.
* `basis_network_statistics.rs:34`, `:50-56` — `IS_RECORDING_DATA` is an `AtomicBool` behind
  accessors. The C# is a bare `public static bool` field (`:49`), non-volatile, written from
  the message registry (`BasisServerMessageRegistry.cs:334`, `:355`) and read on every record
  — a data race in the strict sense, even if a benign one for a flag.
* `basis_network_statistics.rs:189-195` — the `total_calls` / `out_total_calls` the C# docs
  promised but never implemented.
* `basis_network_statistics.rs:10-26` — one `Stripe` struct holding four fixed arrays,
  replacing four separate jagged `long[][]` (`:26-29`) whose per-stripe rows the C# needed
  only because `Interlocked` requires a referenceable element.

## Verdict

The counter core is a faithful port with the same widths, the same striping scheme, the same
256-index space and the same varint encoding, and the Rust adds a bound on inflate, typed
decode errors, and the two total accessors the C# documented but never wrote. The one
consequential deviation is the switch from Brotli to deflate: the Rust doc comment assumes
nothing else decodes these bytes, but the shipping Unity client does, at
`BasisNetworkEvents.cs:737`, so a Rust server's statistics reply will not decode on a C#
client. The dropped quality parameter compounds that by silently moving the poll path from
the C#'s deliberately-chosen fastest level to level 6. With no test on either side, none of
this is caught by the suite.
