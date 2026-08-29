# protocol — port diffs

C#: `BasisNetworkCore/Protocol/` · Rust: `basis_network_core/src/protocol/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BasisNetworkCommons.cs` lines 3–884 (`BasisCoreLease`, `BasisCpuBudget`) | `basis_cpu_budget.rs` | 882 → 899 | deviates (minor; see damping rounding) |
| `BasisNetworkCommons.cs` lines 886–1468 (`BasisNetworkCommons`) | `basis_network_commons.rs` | 583 → 389 | faithful |
| `BasisNetworkVersion.cs` | `basis_network_version.rs` | 13 → 23 | deviates (mutable static → atomic; same value) |
| `BasisPacketUtil.cs` | `basis_packet_util.rs` | 23 → 12 | faithful |
| `BasisNetworkCommons.cs.meta`, `BasisNetworkVersion.cs.meta` | — | 2 files | not ported (Unity asset metadata, nothing to port) |
| — | `mod.rs` | — | Rust-only module wiring |

The one structural change is that the Rust splits the single C# file: `BasisCoreLease` and
`BasisCpuBudget` (which occupy the first 884 lines of `BasisNetworkCommons.cs` and have nothing to
do with the wire protocol) move to their own `basis_cpu_budget.rs`, and `basis_network_commons.rs`
holds only the protocol constants and helpers.

**Wire constants: exact match.** All 115 `public const` values in `BasisNetworkCommons` were
compared mechanically, name by name, against the 115 corresponding `pub const` values in
`basis_network_commons.rs`; every value is identical. That covers all 62 channel numbers (0–63 with
32 and 33 free), the event sub-types, the P2P sub-types, the content-share and registry sub-types,
the jiggle-grab ops, the delta header bits, `TOTAL_CHANNELS = 64`, `SERVER_INFO_*` magics
(`0xBA515101`/`0xBA515102`, protocol version 1), `REJECT_MAGIC = 0xBA515CE1` and the reject kinds.
`PlayerAvatarQualityChannels` (`BasisNetworkCommons.cs:1449`, `basis_network_commons.rs:371`) has
the same 16 entries in the same order. `BasisNetworkVersion.ServerVersion = 54`
(`BasisNetworkVersion.cs:11`) equals `SERVER_VERSION = 54` (`basis_network_version.rs:11`), with the
same v52/v53/v54 changelog comment carried over. There is no wire incompatibility in this module.

The Rust side pins 62 channel constants to literals in
`basis_server_tests/tests/security/permission_and_message_catalog_tests.rs:739-814`, asserts the
0–63 coverage with 32/33 free at `:817-827`, and pins the magics at `:909-917`. The C# has the
equivalent tests in `BasisServerTests/Security/PermissionAndMessageCatalogTests.cs`.

## Deviations

1. **Damping rounds differently at exact midpoints.**
   `BasisNetworkCommons.cs:791` uses `(int)System.Math.Round((target - current) * Damping)`, and
   `Math.Round(double)` is banker's rounding (midpoints to even).
   `basis_cpu_budget.rs:794` uses `f64::round()`, which rounds half away from zero. They disagree
   when `(target - current) * 0.25` lands exactly on `k.5` with `k` even — i.e. when a lease is
   10, 18, 26 … cores from its target (and the negatives): C# moves `k` cores that step, Rust moves
   `k+1`. The commonest midpoint, a difference of 2, produces `0.5` → C# rounds to 0, but the
   "must not round to a standstill" guard at `BasisNetworkCommons.cs:795` / `basis_cpu_budget.rs:798`
   then pushes it to ±1, so both sides agree there. The effect is one core of convergence speed on
   one 100 ms step; both sides converge on the same target. Not pinned by any test.

2. **`get_quality_from_channel` differs for channels below 6.**
   `BasisNetworkCommons.cs:1404` computes `(byte)((channel - PlayerAvatarVeryLowChannel) / 2)`,
   where the subtraction happens in `int` and can go negative — C# integer division truncates
   toward zero, so channel 5 gives 0 and channel 0 gives 253.
   `basis_network_commons.rs:335` computes `channel.wrapping_sub(6) / 2` entirely in `u8`, so
   channel 5 gives 127 and channel 0 gives 125.
   Verified unreachable: the only production callers are
   `basis_network_server/src/reduction/basis_server_reduction_system_events/inbound.rs:25`
   (its handler is registered only on channels 12 and 13, at
   `basis_network_server/src/messaging/basis_server_message_registry.rs:244-245`, matching
   `BasisNetworkServer/Reduction/BasisServerReductionSystemEvents.Inbound.cs:24`) and
   `basis_network_client_console/src/client/message_handler.rs:289`, which reads the channel out of
   a bundle group whose ids are avatar channels. The sibling
   `channel_has_additional_data` (`basis_network_commons.rs:343` vs `BasisNetworkCommons.cs:1416`)
   agrees for *every* input, because masking with `&1` is unaffected by the two's-complement
   representation. Not pinned by a test for out-of-range channels.

3. **`ServerVersion` is a mutable static field in C#, an atomic in Rust.** `BasisNetworkVersion.cs:11`
   is a plain writable `public static ushort`; `basis_network_version.rs:11,16-22` is an
   `AtomicU16` behind `server_version()` / `set_server_version()`, because Rust has no safe
   `static mut`. Same value, same read semantics. Footnote: the doc comment at
   `basis_network_version.rs:14-15` says tests pinning a version mismatch use
   `set_server_version`, but nothing in the tree calls it — the mismatch test at
   `basis_server_tests/tests/networking/basis_connection_lifecycle_tests.rs:133` derives a wrong
   version with `wrapping_add(1)` instead. The setter is unused, not wrong.

4. **The `default:` arms of the delivery-method switches are unreachable in Rust.**
   `BasisNetworkCommons.cs:1281-1282` returns `RegistryControlChannel` for a `DeliveryMethod`
   outside the five defined values, which a C# `enum : byte` can hold. The Rust match at
   `basis_network_commons.rs:260-266` is exhaustive with no fallback, because `DeliveryMethod`
   (`basis_network_core/src/transport/basis_network_shell.rs:44-56`, identical variants and
   discriminants) is a closed enum and bytes off the wire go through `DeliveryMethod::from_byte`,
   which returns `Option`. Deviation with a reason: invalid enum values are unrepresentable.
   `get_delivery_for_plugin_channel` keeps the C# `_ => ReliableOrdered` fallback verbatim
   (`basis_network_commons.rs:274` vs `BasisNetworkCommons.cs:1295-1296`), because its input is a
   raw channel byte. Pinned by `permission_and_message_catalog_tests.rs:856-880`.

5. **`total_cores` is cached; the C# property re-reads.** `BasisNetworkCommons.cs:355` returns
   `Environment.ProcessorCount` per call; `basis_cpu_budget.rs:286-290,408-410` memoises
   `available_parallelism()` in a `LazyLock`. In practice equivalent — .NET caches
   `ProcessorCount` too, and both respect cgroup/affinity limits on Linux — but the Rust will not
   notice a hot CPU-count change. Nothing in either tree relies on that.

Everything else in the two files matches statement for statement. I compared, in both directions:
`ValidatePacket`/`IsNewer` (C#'s unchecked `(byte)(seq1 - seq2)` → `wrapping_sub`,
`basis_packet_util.rs:10` vs `BasisPacketUtil.cs:20`, pinned by
`basis_server_tests/tests/compression/core_primitive_compression_tests.rs:420-448`);
`Encode/DecodeAvatarIntervalByte` (pinned exhaustively for all 256 bytes by
`basis_server_tests/tests/avatar/avatar_interval_codec_tests.rs`); `CanFragment`;
`BuildPriorityUnreliableChannelMap`; the quality/large-channel maps; the delta header pack/unpack
and control bit; `IsPluginChannel`; and, in `BasisCpuBudget`, `MinWorkersPerPool`,
`ConcurrencyWidth`, `MaxReductionSendWorkers`, `SetSendSocketCount`, `AutoMaxSendSockets`,
`ReservedCores`, `Register`/`Unregister`, the whole `DriveDiscovery` state machine
(reopen-on-demand, cooldown nudge, phases 0/1/2, `RateOver`, `TryNarrow`, `StartWindow`,
`SettleProbe`), `Rebalance` (floors, the trim-the-largest loop, the eight redistribution passes,
the integer-truncation stall fallback, the damping loop) and both `Describe` strings.

## Corners cut

1. **CPU utilisation is Unix-only.** `basis_cpu_budget.rs:881-899` reads
   `getrusage(RUSAGE_SELF)` on Unix and returns `None` everywhere else, so on a Windows build
   `sample_utilization()` never updates and `utilization()` stays 0.0 for the process lifetime.
   The C# used `Process.TotalProcessorTime` (`BasisNetworkCommons.cs:383`), which works on every
   platform. This is not cosmetic: utilisation gates pool widening at
   `basis_network_server/src/reduction/basis_server_reduction_system_events/parallelism.rs:164`
   (`if BasisCpuBudget::utilization() > WIDEN_BELOW_UTILIZATION { return current; }`), so a
   non-Unix build would widen the send pool regardless of how full the machine already is —
   exactly the failure the C# doc comment at `BasisNetworkCommons.cs:363-372` says the signal
   exists to prevent. It is also logged at `load_control.rs:53,66`. The rest of the tree is
   heavily Unix-cfg'd, so this is likely a deliberate platform narrowing rather than an oversight,
   but nothing marks it as such.

2. **A throwing ceiling delegate is no longer swallowed.** `BasisNetworkCommons.cs:79-80` wraps the
   `MaxCores()` call in `try/catch` and falls back to `MinCores`. `basis_cpu_budget.rs:152-155`
   calls the closure bare. A panicking `max_cores` closure supplied by an external `register`
   caller would unwind out of `rebalance_inner` while the rebalance mutex is held; `parking_lot`
   does not poison, so the allocator keeps working, but that pass aborts part-way and the leases it
   had not reached keep their previous grants. Neither of the two standing closures can panic
   (`basis_cpu_budget.rs:274,280` — one reads an atomic, one reads a `LazyLock<i32>`), and nothing
   else in the tree registers a lease outside tests, so this is latent rather than live.

3. **`ConcurrencyWidth`'s default arguments are gone.** `BasisNetworkCommons.cs:239` declares
   `perCore = 2, min = 16, max = 1024`; Rust has no default arguments and
   `basis_cpu_budget.rs:353` requires all three. Verified harmless: both call sites pass the same
   explicit values the C# passed — `basis_network_core/src/statistics/basis_network_statistics.rs:31`
   uses `(2, 16, 1024)` matching `BasisNetworkCore/Statistics/BasisNetworkStatistics.cs:23`, and
   `basis_network_server/src/core/network_server.rs:63` uses `(4, 32, 2048)` matching
   `BasisNetworkServer/Core/NetworkServer.cs:56`.

4. **The `MaxCores` delegate is no longer exposed.** `BasisNetworkCommons.cs:47` exposes the
   `System.Func<int>` itself as a public property; `basis_cpu_budget.rs:120-122` exposes only the
   invoked `i32`. Verified nothing in the C# tree reads `.MaxCores` as a delegate, so no capability
   is lost.

5. **The discovery state machine has no test coverage on either side.**
   `basis_network_core/tests/cpu_budget.rs` covers the invariants, registration, clamping, work
   gating, socket-count invalidation and the seams, but never drives a lease through phase 1 → 2,
   so `try_narrow`, `rate_over`, `settle_probe`, the demand-reopen path
   (`basis_cpu_budget.rs:522-529`) and the cooldown nudge (`:533-540`) are exercised by nothing.
   That is inherited — the C# has no such test either — but it means the most intricate part of
   this file was ported on reading alone.

Nothing else was simplified. There are no TODOs, no `unimplemented!()`, no stubs, and no dropped
helpers in either Rust file.

## Improvements

1. **NaN can no longer enter the allocator.** `basis_cpu_budget.rs:143` clamps a NaN demand to 0;
   `BasisNetworkCommons.cs:68-69` compares `demand01 < 0` and `> 1`, both false for NaN, so NaN is
   stored. It then propagates through `weight[i]` at `BasisNetworkCommons.cs:721` into
   `activeWeight` at `:744`; `activeWeight <= 0` is false for NaN so the pass proceeds, every
   `give` at `:752` truncates to 0 (net10.0 saturates NaN→0), and every redistribution pass falls
   into the one-core-at-a-time fallback at `:759-773` — the allocator degrades to moving a single
   core per pass for as long as the NaN stands. Pinned by `basis_network_core/tests/cpu_budget.rs:85-86`.

2. **NaN work reports are rejected instead of latching discovery.** `basis_cpu_budget.rs:171`
   rejects `busy_ms.is_nan()`; `BasisNetworkCommons.cs:112` only tests `busyMs <= 0`, false for
   NaN, so the call proceeds: `_work` advances, `_busyMicros` gains
   `(long)(double.NaN * 1000.0)` = 0, and `_everReportedWork` latches to 1. The lease is thereby
   opted into ceiling discovery with a rate signal whose numerator grew and denominator did not.
   Pinned by `cpu_budget.rs:104-106`.

3. **`ConcurrencyWidth` can no longer spin forever.** `BasisNetworkCommons.cs:245-246` shifts an
   `int pow2` against a `long wanted`; for a `max` above 2^30 the shift overflows to negative and
   then to 0, and `pow2 < wanted` never becomes false. `basis_cpu_budget.rs:363` bounds the loop
   at `1 << 30` and converts with `try_from(...).unwrap_or(1 << 30)`. The shipped call sites cap at
   2048 so the C# never hits it, but the bound is real. Partially pinned by `cpu_budget.rs:17-19`.

4. **The rebalance race is closed.** The C# mutates `_probing` (`BasisNetworkCommons.cs:519`) and
   the per-lease probe fields (`:130-138`) from `DriveDiscovery` with no synchronisation at all,
   while `Rebalance()` is reachable concurrently from the 100 ms timer, from `Register`
   (`:453`) and from `Unregister` (`:476`); two overlapping passes can interleave a probe
   transition with the grant loop. Rust serialises whole passes on
   `basis_cpu_budget.rs:242` (taken at `:671`) and each lease's probe state on `:75`. The comment
   at `:240-241` records the reason. Lock order was checked: `rebalance()`, `leases()` and
   `unregister()` force the standing leases *before* taking any lock (`:459,466,473,494`), and the
   `STANDING` initialiser calls `register_inner` rather than `register`, so the `LazyLock` is never
   forced re-entrantly and there is no inversion between it and the rebalance mutex.

5. **A concurrent probe transition can no longer split the grant decision.**
   `BasisNetworkCommons.cs:709` computes `max[i]` from `ForcedGrant > 0`, then `:784` re-reads
   `ForcedGrant` in the grant loop; if it changed in between, the damped path runs against a
   `max[i]` derived from the other branch. `basis_cpu_budget.rs:692,700,788` caches the flag once.

6. **Arithmetic is bounded where the C# could wrap.** `assigned` and `remaining` are `i64` in
   `basis_cpu_budget.rs:694,729` where `BasisNetworkCommons.cs:704,736` uses `int`; the ceiling
   reopen at `:526,539` uses `saturating_add` where `BasisNetworkCommons.cs:553,568` can overflow to
   a negative ceiling; `encode_avatar_interval_byte` uses `saturating_sub`
   (`basis_network_commons.rs:50`) where `BasisNetworkCommons.cs:953` wraps, pinned by
   `avatar_interval_codec_tests.rs:54`; `add_work`'s `(busy_ms * 1000.0) as i64` saturates.

7. **Internal state is no longer handed out mutably.** `BasisNetworkCommons.cs:480` returns the
   live `_leases` array, whose elements a caller can overwrite;
   `basis_cpu_budget.rs:495` returns a fresh `Vec<Arc<_>>`. `BasisNetworkCommons.cs:1449` declares
   `PlayerAvatarQualityChannels` as `static readonly byte[]`, whose 16 elements any caller can
   write; `basis_network_commons.rs:371` makes it a `const [u8; 16]`, copied at each use.

8. **The probe-window test seam validates its input.** `basis_cpu_budget.rs:506` rejects non-finite
   and negative windows and falls back to 2000 ms; `BasisNetworkCommons.cs:491` is a bare writable
   static. Pinned by `cpu_budget.rs:135-140`.

## Verdict

Yes, this is a faithful port, and the part that matters most for compatibility — the wire contract —
is exact: all 115 constants, all 62 channel numbers and protocol version 54 match value for value,
and the Rust pins them with tests the C# also has. The only behavioural differences I could find are
a midpoint-rounding disagreement in the CPU allocator's damping step that costs one core of
convergence speed on one 100 ms tick, and a divergence in `get_quality_from_channel` for channel
numbers below 6 that no caller on either side can produce. Both are in the non-wire half of the
module. I would trust it in production on Linux; the one thing I would fix first is the Unix-only
utilisation sampler, which silently disables the pool-widening safety gate on any other platform
rather than failing loudly.
