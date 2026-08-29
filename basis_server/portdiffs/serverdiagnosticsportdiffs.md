# Diagnostics — port diffs

C#: `Basis Server/BasisNetworkServer/Diagnostics/` · Rust: `basis_server/basis_network_server/src/diagnostics/`

## File map

| C# | Rust | lines (C#→Rust) | status |
|---|---|---|---|
| `BasisNetworkHealthCheck.cs` | `basis_network_health_check.rs` | 384 → 302 | ported; JSON deviations in the `gc` block and the queue fields, additive `stack`/`legacyPort`/`iroh` |
| `BasisNetworkUdpDropMonitor.cs` | `basis_network_udp_drop_monitor.rs` | 175 → 150 | ported; same thresholds, one delta clamped |
| `BasisServerLogger.cs` | `basis_server_logger.rs` | 22 → 17 | ported; behaviourally identical |
| `BasisServerMemoryReclaim.cs` | `basis_server_memory_reclaim.rs` | 181 → 155 | reinterpreted: same trigger, `malloc_trim` instead of a forced GC |
| `BasisServerSideLogging.cs` | `basis_server_side_logging.rs` | 246 → 277 | ported; drop policy inverted, console colours via ANSI |
| `BasisStatistics.cs` | `basis_statistics.rs` | 51 → 25 | ported; both are inert (the C# body is commented out) |
| `*.cs.meta` (3 files) | — | — | Unity asset metadata, no counterpart by design |
| — | `mod.rs` | — → 14 | Rust module wiring |

Crash reporting is not in this directory on either side: it lives in
`Basis Server/BasisNetworkServer/Security/BasisCrashReportStateManager.cs` →
`basis_server/basis_network_server/src/security/basis_crash_report_state_manager.rs`, and is out
of scope for this file.

## Deviations

### The health document

The benchmark harness parses this document from both servers
(`Basis Server/BasisServerBenchmark/Harness/HealthSample.cs:160-226`), so every field below is a
live external contract. The root-level scalars — `listening`, `ready`, `visitors`, `capacity`,
`sent`, `recv`, `packetsSent`, `packetsRecv`, `droppedUnreliable`, `droppedVoice`, `currentTime`,
`startTime`, `version` — and the whole `bsr` block match name-for-name and type-for-type
(`BasisNetworkHealthCheck.cs:159-209, 270-359` vs `basis_network_health_check.rs:151-173,
209-278`). The differences are these.

**D1 — `gc.gen2` carries a different quantity.** C# `BasisNetworkHealthCheck.cs:262` is
`GC.CollectionCount(2)`, the process's gen2 collection count. Rust
`basis_network_health_check.rs:203-204` puts `BasisServerMemoryReclaim::passes()` there — the
number of idle-reclaim passes. `gen0` and `gen1` are honest zeros; `gen2` is not a zero, it is a
different counter under a GC name. The harness reads it as `Gen2Collections`
(`HealthSample.cs:209`). No test pins the field on either side.

**D2 — `gc.heapMb` and `gc.committedMb` are both the resident set.** C# `heapMb` is
`GC.GetTotalMemory(false)`, the managed heap (`BasisNetworkHealthCheck.cs:263`), and
`committedMb` is `GCMemoryInfo.TotalCommittedBytes`, committed managed segments
(`BasisNetworkHealthCheck.cs:255`). Rust reports RSS from `/proc/self/statm` for both
(`basis_network_health_check.rs:201-203`, `src/util.rs:86-97`). RSS and CLR committed heap are
not the same quantity — RSS includes every non-heap page and excludes committed-but-untouched
ones — and the benchmark report compares them directly as one memory column
(`basis_server/benchmarks/results/2026-08-29-two-core/README.md:174`, "29 / 37 / 56 / 104 MB
against the CLR's 19 / 25 / 52 / 109"). Not a parse break; a meaning change in a number that is
being compared across servers.

**D3 — `gc.allocatedMb`, `gc.fragmentedMb`, `gc.pauseTimePercent`, `gc.serverGc`,
`gc.latencyMode` are constants.** C# `BasisNetworkHealthCheck.cs:254-257, 265-266` measures all
five. Rust hardcodes `0`, `0`, `0`, `false`, `"None"` (`basis_network_health_check.rs:203`).
Types are preserved (numbers stay numbers, `serverGc` stays a bool, `latencyMode` stays a
string), so nothing fails to parse, but `"None"` is not a member of the .NET `GCLatencyMode`
domain the C# emits from. `fragmentedMb` and `pauseTimePercent` are not merely read but printed
in the benchmark summary (`BasisServerBenchmark/Tuning/BenchmarkSession.cs:356`), where the Rust
server will always show 0 MB fragmented and 0.0 % GC pause.

**D4 — fields the Rust adds.** `gc.reclaimedMb` and `gc.runtime:"rust"`
(`basis_network_health_check.rs:203`), and at the root `stack`, `legacyPort`, `iroh`
(`basis_network_health_check.rs:177-192`), absent from the C# document. These are additive and
the harness already expects them to be optional — `HealthSample.cs:21` documents `stack` as
`"" from a server that does not say` — so they break nothing.

**D5 — `queuePerPeer` and `voiceQueuePerPeer` are read from a different source.** C#
`BasisNetworkHealthCheck.cs:185, 190` reads `LNLNetManager.manager.EffectiveUnreliableQueuePerPeer`
/ `EffectivePriorityUnreliableQueuePerPeer` — the *population-scaled effective* bound the drop
counters are actually measured against — and falls back to `0` when the manager is not
LiteNetLib. Rust `basis_network_health_check.rs:155, 166-167` reads
`IrohTransportConfig.max_datagram_queue_per_peer` / `max_priority_datagram_queue_per_peer` from
the transport config store, regardless of which stack is running, and those default to `0`
(`basis_network_core/src/configuration/iroh_transport_config.rs:21-22`) meaning "auto". The Rust
LiteNetLib manager does compute an effective queue, but the accessors are `pub(super)`
(`basis_network_core/src/transport/lnl_network_impl/net_manager.rs:178-184`) and the health check
cannot reach them. So on the LiteNetLib stack the Rust reports the iroh setting instead of the
effective LNL queue, and on the iroh stack it reports the configured maximum rather than the
scaled value applied at `iroh_network_impl.rs:891`. The harness reads both fields
(`HealthSample.cs:189-190`) and the comment in the C# says exactly what they are for: making the
drop counts readable. Nothing pins them in either test suite.

**D6 — timestamp offset spelling.** C# formats `DateTimeOffset` with `"O"`
(`BasisNetworkHealthCheck.cs:191-192` for `currentTime`/`startTime`, `:306` for
`bsr.window.capturedTime`; the snapshot field is a `DateTimeOffset` at
`BasisNetworkServer/Reduction/Profiling.cs:15`), which renders the offset as `+00:00`. Rust emits
a `Z` suffix (`src/util.rs:34-37`, used at `basis_network_health_check.rs:140` and
`reduction/profiling.rs:333`). Both are ISO-8601 and parse to the same instant; a strict
`Z`-only or literal string comparison would differ. The harness does not read these three fields.

**D7 — `bsr.load.intervalMs` clamping.** C# prints the raw interval at
`BasisNetworkHealthCheck.cs:278` and clamps only for `hz` at `:279`. Rust clamps once
(`basis_network_health_check.rs:210`) and prints the clamped value for both `:213`. Visible only
if the tick interval is ever 0.

### The endpoint itself

**D8 — trailing-slash normalisation.** C# `NormalizePath` strips exactly one trailing slash
(`BasisNetworkHealthCheck.cs:62`); Rust loops (`basis_network_health_check.rs:92-94`). A request
for `/health//` normalises to `/health/` in C# (404) and `/health` in Rust (200). Externally
visible, unpinned.

**D9 — bind host grammar.** C# hands the host to an `HttpListener` prefix
(`BasisNetworkHealthCheck.cs:44`), which accepts DNS hostnames and the `+`/`*` wildcards. Rust
accepts `""`, `"*"`, `"+"`, `"0.0.0.0"`, `"localhost"` or an IP literal and returns a permanent
error otherwise (`basis_network_health_check.rs:72-83`). A configuration naming a real hostname
starts on the C# and refuses to start on the Rust. Pinned:
`basis_rest_api_tests/tests/health_check_tests.rs:120-122`.

**D10 — headers on the 503 shed path.** C# writes the overload 503 from the accept loop before
the hardening headers are set (`BasisNetworkHealthCheck.cs:93`), so that response has no
`Cache-Control` or `X-Content-Type-Options`. Rust routes it through `harden`
(`basis_network_health_check.rs:107-108, 125-135`) so every response carries them.

### Logging

**D11 — `WriteToScreen=false` did not stop console output in the C#.**
`BasisServerSideLogging.cs:205` returns only when *both* sinks are off, then `:210` calls
`WriteScreenLine` unconditionally. Rust checks the flag before writing
(`basis_server_side_logging.rs:221-230`). Setting the flag now does what it says.

**D12 — the full-queue drop policy is inverted.** C# `BasisServerSideLogging.cs:215-218` takes
one entry off the front and re-adds, dropping the *oldest* line. Rust
`basis_server_side_logging.rs:242-249` matches on `TrySendError::Full` and simply retries the
same `try_send` — nothing is dequeued, the channel is still full, and the retry fails — so the
*newest* line is dropped. The comment on `:245` says "Drop the oldest line if the queue is full",
which the code does not do. The sender half has no receive handle, so the C# policy is not
reachable from there as written. Bound (200 lines) and the "one line is lost under a burst"
outcome are the same; which line is lost is not. No test.

**D13 — record terminator.** C# appends `Environment.NewLine`
(`BasisServerSideLogging.cs:73, 75`); Rust always `'\n'`
(`basis_server_side_logging.rs:142, 146`). On Windows the log file changes from CRLF to LF.

**D14 — console colouring.** C# drives `Console.ForegroundColor`
(`BasisServerSideLogging.cs:235-242`); Rust writes ANSI escapes unconditionally
(`basis_server_side_logging.rs:58-64, 259`), including when stdout is a pipe or a file, where the
C# would have emitted plain text.

### Memory reclaim

**D15 — `reclaimedBytes` measures a different thing.** C# accumulates the managed-heap delta and
only on the empty-server path — the `players > 0` branch returns before any accounting
(`BasisServerMemoryReclaim.cs:130-138, 146-150`). Rust accumulates the resident-set delta on
every pass (`basis_server_memory_reclaim.rs:114-121`). This feeds the added `gc.reclaimedMb`
field (D4), so it is externally visible but not consumed by anything today.

**D16 — one reclaim path instead of two.** C# escalates: a non-blocking background gen2 while
players remain (`BasisServerMemoryReclaim.cs:132`), and at zero players a LOH-compacting
blocking collection run twice around `WaitForPendingFinalizers` (`:140-143`). Rust calls
`malloc_trim(0)` in both cases (`basis_server_memory_reclaim.rs:116, 131-137`), which is the
closest available equivalent and is a no-op off glibc. The trigger, the divisor, the settle
window, the minimum peak and the 120 s floor between passes are identical
(`BasisServerMemoryReclaim.cs:27-29, 99-121` vs `basis_server_memory_reclaim.rs:33-35, 83-107`).

**D17 — monotonic clock.** C# eligibility and cooldown use `DateTime.UtcNow`
(`BasisServerMemoryReclaim.cs:108, 114-115`); Rust uses `Instant`
(`basis_server_memory_reclaim.rs:91, 96-101`), so a wall-clock step no longer moves the window.

### UDP drop monitor

**D18 — the non-buffer drop delta is clamped.** C# computes `otherDrops = deltaIn - droppedBuf`
(`BasisNetworkUdpDropMonitor.cs:112`); if `RcvbufErrors` went backwards (counter reset,
namespace change) `droppedBuf` is negative and inflates `otherDrops`, producing a spurious
"check NIC/link health" warning. Rust subtracts `dropped_buf.max(0)`
(`basis_network_udp_drop_monitor.rs:106`). Log-only.

**D19 — warning text.** C# says "raise MultiSocketCount in litenetlib.xml"
(`BasisNetworkUdpDropMonitor.cs:106`); Rust says "raise the transport's receive concurrency"
(`basis_network_udp_drop_monitor.rs:100`), since the Rust server has more than one transport.
Parsing, thresholds (10 s sample, `/proc/net/snmp`, column lookup by name) and the published
`TotalReceiveBufferDrops` counter are unchanged.

### Checked and found equivalent

`BasisServerLogger` (both forward only Warning and Error, both drop Trace and Info);
`BasisStatistics` (inert on both sides); the `bsr` block field-for-field including the zstd/lz4
sub-object and every divide-by-zero guard (C# relies on `Num()` turning NaN/Inf into `0`
at `BasisNetworkHealthCheck.cs:227-230`, Rust guards the denominator explicitly at
`basis_network_health_check.rs:232, 248` — same output); numeric precision (`json_num` in
`src/util.rs:117-119` reproduces the C# `F1`/`F2`/`F3`/`F4` formats and the NaN→`0` rule);
`ready`/status-code coupling; the 32-request concurrency cap and its 503; the 405/404 paths; the
`Cache-Control`/`X-Content-Type-Options` hardening; the 250 ms shutdown wait.

## Corners cut

* `working_set_bytes` reads `/proc/self/statm` and returns 0 everywhere else
  (`src/util.rs:86-97`). On macOS or Windows the Rust server reports `heapMb: 0`,
  `committedMb: 0` and `reclaimedMb: 0`, where the C# had real numbers on every platform.
* `malloc_trim` is compiled in only for `target_os = "linux", target_env = "gnu"`
  (`basis_server_memory_reclaim.rs:131-137`). Elsewhere the pass still counts, still logs and
  still reports through `gc.gen2`, having reclaimed nothing.
* The minute-resolution timestamp cache is gone. C# held an immutable `MinuteStamp` and reused it
  for every line inside the minute (`BasisServerSideLogging.cs:134-160`); Rust calls
  `localtime_r` and formats per line (`basis_server_side_logging.rs:203-206`). The C# comment
  explains that this was a measured cost.
* No panic guard around the health handler. C# wrapped the whole of `HandleRequest` in a
  catch-and-abort (`BasisNetworkHealthCheck.cs:221-224`); the Rust handler has none
  (`basis_network_health_check.rs:106-123`). Nothing in the JSON build can panic today, but the
  REST module took the belt-and-braces route (`basis_rest_api_handler.rs:118-121`) and this one
  did not.
* Neither sampler traps errors per iteration the way the C# did
  (`BasisServerMemoryReclaim.cs:83-84`, `BasisNetworkUdpDropMonitor.cs:78-79`); the Rust loops
  call `sample` directly (`basis_server_memory_reclaim.rs:66-69`,
  `basis_network_udp_drop_monitor.rs:70-73`). A panic in a sampler kills that thread silently
  rather than logging and continuing.
* The health JSON is still hand-concatenated on both sides. `shedTierName`, `version` and the
  iroh connection string go in unescaped (`basis_network_health_check.rs:157, 183-189, 213`).
  Same exposure as the C#, not a regression, but `json_escape` exists in `src/util.rs:100-114`
  and is not used here.
* `BasisStatistics` keeps a manager handle the C# had commented out
  (`basis_statistics.rs:16-18` vs `BasisStatistics.cs:14-20`); the poll is a no-op in both, so
  the Rust holds a strong reference to the manager for nothing.
* The Rust-only seams — `BasisServerMemoryReclaim::sample(players)`,
  `BasisNetworkUdpDropMonitor::apply_sample` / `parse_snmp_udp`, and the `reset_for_tests`
  helpers — are public but no test in the tree calls them. The diagnostics module has no unit
  tests at all on either side beyond the four health-endpoint tests.

## Improvements

* A null `Configuration` no longer aborts the connection. C#
  `BasisNetworkHealthCheck.cs:142` dereferences `NetworkServer.Configuration` unguarded; a null
  there throws into the outer catch at `:221` and answers with `Response.Abort()`. Rust uses
  `configuration_or_default()` (`basis_network_health_check.rs:139`,
  `core/network_server.rs:122-124`) and always answers.
* Bind failures are typed rather than thrown: a busy port is transient, an unparseable host is
  permanent (`basis_network_health_check.rs:44-49, 72-83`). Pinned by
  `basis_rest_api_tests/tests/health_check_tests.rs:112-122`.
* Start guards are atomic. The C# `if (_worker != null) return;`
  (`BasisServerMemoryReclaim.cs:57`) and `if (_samplerThread != null) return;`
  (`BasisNetworkUdpDropMonitor.cs:49`) are read-then-write races; the Rust uses
  `STARTED.swap(true, AcqRel)` (`basis_server_memory_reclaim.rs:48`,
  `basis_network_udp_drop_monitor.rs:43`) and rolls the flag back if the spawn fails.
* A failing log write is reported instead of silently killing the writer. The C# task catches
  only `OperationCanceledException` (`BasisServerSideLogging.cs:82`), so an `IOException` from
  `WriteToFileAsync` ends the logging task for the life of the process with no message. Rust
  reports to stderr and keeps draining (`basis_server_side_logging.rs:165-169`).
* One owning writer thread replaces the queue-plus-semaphore arrangement
  (`BasisServerSideLogging.cs:18, 89-106` vs `basis_server_side_logging.rs:120-158`); the file
  cannot have two writers because there is only one.
* `shutdown()` unhooks the BNL sinks (`basis_server_side_logging.rs:182-184`); the C#
  `ShutdownAsync` leaves `BNL.LogOutput` pointing at a stopped logger.
* A console sink hook lets the interactive driver own the screen without the logger fighting it
  (`basis_server_side_logging.rs:30-31, 253-255, 264-276`).
* `players.saturating_mul(DROP_DIVISOR)` (`basis_server_memory_reclaim.rs:87`) cannot overflow
  where `players * DropDivisor` (`BasisServerMemoryReclaim.cs:102`) can.
* Divide-by-zero in the profiling window is handled by an explicit denominator check rather than
  by letting NaN through and normalising it later
  (`basis_network_health_check.rs:232, 248` vs `BasisNetworkHealthCheck.cs:227-230, 303`).
* The C# needed a regression test for comma-decimal cultures
  (`BasisRestApi.Tests/HealthCheckTests.cs:77`); Rust formatting is culture-independent, so that
  whole class of bug is gone.

## Verdict

A faithful port of the endpoint, the monitors and the sink. The endpoint's shape — status codes,
method and path handling, concurrency cap, hardening headers, shutdown — matches, and the `bsr`
block, which is the largest and most consumed part of the document, matches field-for-field
including every guard.

The health JSON is **parse-compatible but not fully meaning-compatible**. Every C# field is still
present with its C# type, so nothing an external tool reads will fail or vanish; the additions
(`stack`, `legacyPort`, `iroh`, `gc.reclaimedMb`, `gc.runtime`) are additive and already tolerated
by the harness. Four fields changed what they mean: `gc.gen2` now counts reclaim passes rather
than collections (D1), `gc.heapMb` and `gc.committedMb` are both RSS rather than two different
managed-heap figures (D2), and `queuePerPeer`/`voiceQueuePerPeer` report a configured iroh setting
rather than the effective queue the drop counters are measured against (D5). D1 and D5 are silent
mis-readings for anything that compares the two servers; D2 is the one already being used as a
cross-server memory column in the benchmark report. Nothing in either test suite pins the `gc`
block, so none of this is caught today.

Everything else here is even or better: the logging path fixes a real C# bug (D11) and a real
silent-death path, the samplers lose two races and a wall-clock dependency, and bind failures
became reportable. The one thing to fix rather than merely record is D12 — the log queue drops
the newest line where the C# dropped the oldest, and the comment claims otherwise.
