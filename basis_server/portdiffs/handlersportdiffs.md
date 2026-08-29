# handlers — port diffs

C#: `Basis Server/BasisNetworkServer/Handlers/` · Rust: `basis_server/basis_network_server/src/handlers/`

Six per-message handlers reached from `BasisServerEventsRouter` (chat typing, temp block, voice
record, error report, jiggle grab) and from `BasisServerMessageRegistry` (PIP camera).

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BasisNetworkHandleChatTyping.cs` | `basis_network_handle_chat_typing.rs` | 53→36 | ported, no behavioural deviation |
| `BasisNetworkHandleTempBlock.cs` | `basis_network_handle_temp_block.rs` | 42→27 | ported, no behavioural deviation |
| `BasisNetworkHandleVoiceRecord.cs` | `basis_network_handle_voice_record.rs` | 55→42 | ported, no behavioural deviation |
| `BasisNetworkHandleErrorReport.cs` | `basis_network_handle_error_report.rs` | 197→172 | ported, 3 deviations |
| `BasisNetworkHandleJiggleGrab.cs` | `basis_network_handle_jiggle_grab.rs` | 191→167 | ported, limits identical, 1 deviation |
| `BasisNetworkPIPCamera.cs` | `basis_network_pip_camera.rs` | 276→246 | ported, 2 deviations |
| — | `mod.rs` | —→14 | new (module wiring / re-exports) |

Totals: 814 C# lines across 6 files → 704 Rust lines across 7 files.

## Rate limits and per-player caps

Every threshold was compared literal-for-literal; all match.

| Limit | C# | Rust | same? |
| --- | --- | --- | --- |
| Jiggle grab tokens/sec | `BasisNetworkHandleJiggleGrab.cs:24` `TokensPerSecond = 8f` | `basis_network_handle_jiggle_grab.rs:32` `TOKENS_PER_SECOND = 8.0` | yes |
| Jiggle grab burst | `BasisNetworkHandleJiggleGrab.cs:25` `TokenBurst = 16f` | `basis_network_handle_jiggle_grab.rs:33` `TOKEN_BURST = 16.0` | yes |
| Jiggle grab tracked peers | `BasisNetworkHandleJiggleGrab.cs:26` `MaxTrackedPeers = 4096` | `basis_network_handle_jiggle_grab.rs:34` `MAX_TRACKED_PEERS = 4096` | yes |
| Jiggle grab relevance radius | `BasisNetworkHandleJiggleGrab.cs:22` `RelevanceDistance = 64f` | `basis_network_handle_jiggle_grab.rs:30` `RELEVANCE_DISTANCE = 64.0` | yes |
| Error report message cap | `BasisNetworkHandleErrorReport.cs:30` `MaxMessageChars = 2000` | `basis_network_handle_error_report.rs:35` `MAX_MESSAGE_CHARS = 2000` | yes |
| Error report stack cap | `BasisNetworkHandleErrorReport.cs:31` `MaxStackChars = 12000` | `basis_network_handle_error_report.rs:36` `MAX_STACK_CHARS = 12000` | yes |
| Error report dedup per user | `BasisNetworkHandleErrorReport.cs:34` `MaxSeenPerUser = 256` | `basis_network_handle_error_report.rs:37` `MAX_SEEN_PER_USER = 256` | yes |
| Error report tracked users | `BasisNetworkHandleErrorReport.cs:35` `MaxTrackedUsers = 4096` | `basis_network_handle_error_report.rs:38` `MAX_TRACKED_USERS = 4096` | yes |

Refill semantics are the same shape too: bucket starts full at `TokenBurst`
(`BasisNetworkHandleJiggleGrab.cs:30-31` / `basis_network_handle_jiggle_grab.rs:40`), refills
`elapsed * TokensPerSecond` clamped to the burst, and one token is spent per accepted op
(`BasisNetworkHandleJiggleGrab.cs:45-54` / `basis_network_handle_jiggle_grab.rs:42-50`). The
whole-table wipe past `MaxTrackedPeers` is the same policy in both
(`BasisNetworkHandleJiggleGrab.cs:38-41` / `basis_network_handle_jiggle_grab.rs:37-39`). The
error-report dedup wipe past `MaxTrackedUsers` is likewise conditional on the incoming uuid not
already having a bucket (`BasisNetworkHandleErrorReport.cs:81-84` /
`basis_network_handle_error_report.rs:89-91`).

## Deviations

**1. Token bucket clock: wall clock → monotonic.**
C# `BasisNetworkHandleJiggleGrab.cs:31,45-46` timestamps refills with `DateTime.UtcNow.Ticks`;
Rust `basis_network_handle_jiggle_grab.rs:22,42-43` uses `std::time::Instant`. Why: `Instant` is
monotonic, so an NTP step or a manual clock change cannot hand a peer a free refill (backwards
step: C# computes a negative `elapsedSeconds`, which subtracts tokens; forwards step: an
instant refill to the full burst). Rates and burst are unchanged. Not pinned by a test — there
is no test for this handler on either side.

**2. Truncated payloads no longer count as protocol errors.**
In C#, a short read throws (`Basis Server/LiteNetLib/Utils/NetDataReader.cs:94-99`
`EnsureAvailable` throws `InvalidOperationException`); the handler does not catch it, so it
reaches `BasisNetworkMessageProcessor.cs:55-65`, which increments `_peerErrorCounts` for that
peer and calls `HandleErrorEscalation` — repeated malformed frames warn and eventually
disconnect the client. The Rust handlers absorb the short read locally and return:
`basis_network_handle_chat_typing.rs:13-15`, `basis_network_handle_temp_block.rs:14-16`,
`basis_network_handle_voice_record.rs:22-28`, `basis_network_handle_jiggle_grab.rs:54`
(`let _ = Self::try_handle(...)`), `basis_network_pip_camera.rs:48-50,105-107`. The Rust
processor (`basis_network_message_processor.rs:73-91`) only counts panics and unknown
channels, so a client can now stream truncated frames on these sub-types indefinitely without
accruing a single protocol error. Why: the port replaced exceptions with `Result`, and the
handler signatures return `()`, so there is nowhere to report the fault. Not pinned by a test.
This is the one finding here with an abuse angle.

**3. PIP position sends are now subject to the per-channel queue cap.**
C# `BasisNetworkPIPCamera.cs:184` calls `recipientPeer.Send(...)` directly, deliberately
bypassing `NetworkServer.TrySend`'s cap (`Basis Server/BasisNetworkServer/Core/NetworkServer.cs:376-399`
drops a `Sequenced` send when more than 70 packets are already queued on that channel). Rust
`basis_network_pip_camera.rs:167` routes through `NetworkServer::try_send`, which applies
`DEFAULT_MAX_MESSAGES = 70` (`core/network_server.rs:88,589-603`). So on a backed-up recipient
the Rust drops the PIP frame where the C# queued it. It still stamps
`last_sent_times.insert(recipient_id, now_ms)` (`basis_network_pip_camera.rs:168`) whether or
not the send went out, so a dropped update is not retried sooner than the normal interval. Why:
the port used the shared send helper uniformly rather than reproducing the one call site that
skipped it. Bounding the queue is arguably right; the unconditional last-sent stamp is the part
worth knowing. Not pinned by a test.

**4. PIP tick clock resolution: ticks → milliseconds.**
C# `BasisNetworkPIPCamera.cs:122,180` takes `Stopwatch` ticks and converts the interval with
`MsToTick` (`:34`). Rust `basis_network_pip_camera.rs:128,166` takes milliseconds and compares
directly; the caller truncates (`reduction/basis_server_reduction_system_events/tick.rs:184`,
`update_pip_positions(now / 1000)`, with `now_ticks()` in microseconds per
`reduction/basis_server_reduction_system_events/mod.rs:36-44`). Net effect: the same interval
policy at 1 ms granularity instead of sub-microsecond. Why: the Rust tick clock is already
microseconds, so the C# `Stopwatch.Frequency` dance is unnecessary. Harmless; recorded because
the units in the signature differ. Not pinned by a test.

**5. Crash-report filename sanitisation uses a fixed character set.**
C# `BasisNetworkHandleErrorReport.cs:167-173` uses `Path.GetInvalidFileNameChars()`, which on
Linux .NET is only `'\0'` and `'/'`. Rust `basis_network_handle_error_report.rs:163-171`
replaces control characters plus `" < > | : * ? \ /`. Why: a fixed, platform-independent set is
the safer choice for a filename derived from a client-supplied uuid, and it matches what the C#
would have done on Windows. Practical effect: a uuid containing e.g. `:` lands in a
differently-named `.jsonl` file than the C# on Linux would have written. Not pinned by a test.

**6. Report truncation counts scalar values, not UTF-16 code units.**
C# `BasisNetworkHandleErrorReport.cs:78-79` truncates with `Substring` over UTF-16 chars; Rust
`basis_network_handle_error_report.rs:86-87` uses `chars().take(...)` over Unicode scalar
values. For a message containing astral characters (emoji, some CJK extensions) the two cut at
different points, so the stored text — and therefore the dedup hash, which is computed after
truncation in both — can differ. The hash function itself was deliberately kept byte-compatible
(`basis_network_handle_error_report.rs:124-131` mixes `encode_utf16` units, matching
`BasisNetworkHandleErrorReport.cs:129-142`). Why: `chars()` is the idiomatic Rust truncation
and the difference only shows above the BMP. Not pinned by a test.

**7. Error-report failures are logged more narrowly.**
C# wraps the whole handler in `try/catch` and logs `Failed to handle error report: …`
(`BasisNetworkHandleErrorReport.cs:96-99`) for any fault, including a malformed compressed
blob. Rust only logs when the file write fails (`basis_network_handle_error_report.rs:102-104`);
a malformed read returns silently at `:59-64`. Why: the Rust paths return `Result`/`Option`
rather than throwing, and the read failures are client-caused noise. Not pinned by a test.

**8. `NetworkServer.Configuration` null-handling.**
C# `BasisNetworkHandleErrorReport.cs:65` dereferences `NetworkServer.Configuration` directly
(a `NullReferenceException` if the server is not configured, caught by the surrounding
`try/catch`). Rust uses `NetworkServer::configuration_or_default()`
(`basis_network_handle_error_report.rs:70`). Why: no nulls; falls back to the default
configuration. Not pinned by a test.

**9. Crash-report directory root.**
C# `BasisNetworkHandleErrorReport.cs:146` roots at `AppContext.BaseDirectory`; Rust
`basis_network_handle_error_report.rs:51-53` at `Configuration::base_directory()`, exposed as a
public `crash_report_directory()` accessor the C# does not have. Same intent; the Rust root is
configurable. Not pinned by a test.

Compared and found identical (no deviation): the chat-typing block check and ushort peer-id
guard (`BasisNetworkHandleChatTyping.cs:27-36` / `basis_network_handle_chat_typing.rs:16-22`);
the temp-block target lookup and payload rewrite; the voice-record `needed = 4/3` pre-check
(`BasisNetworkHandleVoiceRecord.cs:26` / `basis_network_handle_voice_record.rs:18`) and both
wire layouts; the jiggle-grab Start/Stop/Deny field order and delivery methods; the
relevance filter including the target unconditionally and failing open to a full broadcast when
the target has no live position (`BasisNetworkHandleJiggleGrab.cs:156-162` /
`basis_network_handle_jiggle_grab.rs:139-142`); the FNV-1a-64 dedup hash including the
first-stack-line-only rule; the JSON report field order, severity names and escaping
(`BasisNetworkHandleErrorReport.cs:149-158,175-195` / `basis_network_handle_error_report.rs:134-150`
plus `util.rs:100-114`); and the PIP create/destroy/late-joiner/disconnect message flows
including the `Mode`-agnostic broadcast-to-all on disconnect
(`BasisNetworkPIPCamera.cs:230-252` / `basis_network_pip_camera.rs:204-224`).

## Corners cut

* **No tests at all, on either side.** Nothing in `basis_server/basis_server_tests/` or
  `Basis Server/BasisServerTests/` exercises any of these six handlers; the only mentions are
  the event-type constants listed in
  `Basis Server/BasisServerTests/Security/PermissionAndMessageCatalogTests.cs:1153-1160`. The
  token bucket, the relevance filter, the crash-report dedup and the PIP interval maths are all
  unpinned in both languages. Every deviation above is therefore unpinned by construction.
* `BasisNetworkHandleJiggleGrab.cs:151-155` guards `PeerSnapshot == null` and returns without
  sending; the Rust snapshot is a `Vec` that cannot be null, so the guard is gone
  (`basis_network_handle_jiggle_grab.rs:138`). Behaviourally identical for an empty instance,
  but the "snapshot not yet built" case now falls through to an empty loop instead of an early
  return.
* `BasisNetworkPIPCamera.cs:34` keeps `MsToTick` as a field; the Rust dropped it, which is why
  deviation 4 exists.

## Improvements

* **Monotonic rate limiting** (deviation 1) — the C# bucket can be reset or starved by a clock
  step.
* **Split locking on PIP state.** C# `CameraPIPState` (`BasisNetworkPIPCamera.cs:12-29`) is a
  bag of plain fields written from the network thread and read from the reduction tick with no
  synchronisation at all (only `LastSentTimes` is concurrent, and the comment there explains
  why). Rust puts the pose behind its own `Mutex` and keeps `last_sent_times` a `DashMap`
  (`basis_network_pip_camera.rs:27-34`), so a half-written transform can no longer be read by
  the tick.
* **Snapshot before iterating.** C# `BasisNetworkPIPCamera.cs:127` iterates the live
  `PIPStates` while `RemovePlayer` may remove from it; Rust collects to a `Vec` first
  (`basis_network_pip_camera.rs:130`), so the tick never holds a map shard while sending.
* **Every send is checked.** C# `UpdatePIPPositions` calls `NetPeer.Send` unguarded; the Rust
  path logs and continues when the transport refuses a payload
  (`core/network_server.rs:603-612`). (The flip side is deviation 3.)
* **Per-user dedup buckets are reference-counted out of the map.** C#
  `BasisNetworkHandleErrorReport.cs:87-92` takes the `HashSet` out of the dictionary and locks
  it, but another thread can clear the dictionary (`:83`) between the `GetOrAdd` and the lock,
  so the insert lands in an orphaned set. The Rust holds an `Arc<Mutex<HashSet>>`
  (`basis_network_handle_error_report.rs:29,94`) — the same orphaning is possible but the
  entry is cloned out under the shard lock first, so the set is never observed half-cleared.

## Verdict

A faithful port. All eight rate-limit and cap constants are literal matches, the refill maths
and the wipe policies are the same, and the wire formats round-trip identically. The
substantive finding is deviation 2: truncated frames on these sub-types no longer feed the
per-peer protocol-error budget, so the escalation-to-disconnect path that the C# got for free
from its exception model does not fire in Rust. Deviation 3 (PIP frames now droppable by the
queue cap, with the last-sent time stamped anyway) is the second-most consequential. Everything
else is cosmetic or a small hardening. The real risk is not the diffs but the coverage: neither
codebase has a single test for this directory, so nothing above is defended by CI.
