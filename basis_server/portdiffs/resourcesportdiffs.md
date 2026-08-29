# resources — port diffs

C#: `Basis Server/BasisNetworkServer/Resources/` · Rust: `basis_server/basis_network_server/src/resources/`

The loaded-resource database, the synchronized-preload session tracker, and the cached default
library blob.

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BasisNetworkResourceManagement.cs` | `basis_network_resource_management.rs` | 343→290 | ported, caps identical, 3 deviations |
| `BasisNetworkPreloadResourceManagement.cs` | `basis_network_preload_resource_management.rs` | 293→272 | ported, 3 deviations |
| `BasisNetworkServerLibrary.cs` | `basis_network_server_library.rs` | 138→91 | ported, 2 deviations |
| — | `mod.rs` | —→9 | new (module wiring / re-exports) |

Totals: 774 C# lines across 3 files → 662 Rust lines across 4 files.
(`BasisNetworkResourceManagement.cs.meta` is a Unity asset stub, not ported.)

## Caps and counts

| Limit | C# | Rust | same? |
| --- | --- | --- | --- |
| Per-player loaded-resource cap (default) | `BasisNetworkResourceManagement.cs:19` `DefaultMaxLoadedResourcesPerPlayer = 16384` | `basis_network_resource_management.rs:23` `DEFAULT_MAX_LOADED_RESOURCES_PER_PLAYER = 16384` | yes |
| Config override | `BasisNetworkResourceManagement.cs:21-25` `Configuration?.MaxLoadedResourcesPerPlayer`, `> 0` wins | `basis_network_resource_management.rs:40-43` `max_loaded_resources_per_player`, `> 0` wins | yes |
| Shipped config default | `BasisNetworkCore/Configuration/BasisServerConfiguration.cs:350` `= 16384` | `basis_network_core/src/configuration/basis_server_configuration.rs:97` `= 16384` | yes |
| Server-authoritative loads exempt | `BasisNetworkResourceManagement.cs:34` empty uuid → `true` | `basis_network_resource_management.rs:49-51` empty uuid → `true` | yes |
| Synchronized-load timeout | `BasisNetworkPreloadResourceManagement.cs:21` `TimeSpan.FromMinutes(5)` | `basis_network_preload_resource_management.rs:49` `Duration::from_secs(5 * 60)` | yes |

The per-creator counter behaves the same on both sides: incremented on add
(`BasisNetworkResourceManagement.cs:49-53` / `basis_network_resource_management.rs:62-67`),
decremented and the entry removed at zero (`:56-70` / `:69-80`), and re-derived by an O(N) scan
of the database at the cap boundary so drift can never permanently block a creator (`:39-45` /
`:57-59`).

## Release on disconnect

Verified end to end. `BasisServerHandleEvents.cs:379-393` and
`core/basis_server_handle_events.rs:420-442` call the same teardown in the same order:

* `BasisNetworkResourceManagement.RemovePeerResources(uuid)` — `:382` / `:424`. Unloads and
  forgets every non-persistent resource created by that uuid, broadcasting an `UnLoadResource`
  per item and decrementing the per-creator count
  (`BasisNetworkResourceManagement.cs:108-139` / `basis_network_resource_management.rs:110-125`).
* `BasisNetworkPreloadResourceManagement.RemovePeer(id)` — `:392` / `:435`. Drops the peer from
  every active session's ready/failed sets, decrements `TotalPeerCount`, deletes sessions that
  reach zero, and fires the spawn signal for sessions the remaining peers have already all
  answered (`BasisNetworkPreloadResourceManagement.cs:238-279` /
  `basis_network_preload_resource_management.rs:236-263`).

Both keep `Persist == true` resources — and therefore their per-creator count — across the
disconnect, deliberately and identically. Nothing in the module leaks per-peer state on
disconnect in either language.

## Deviations

**1. Server-library cache is copied on every send.**
C# `BasisNetworkServerLibrary.cs:21-22,107-110` keeps a reusable `byte[]` and a length, grows it
only when the wire actually gets bigger, and the doc comment at `:16-19` states the goal: "Per-peer
joins just memcpy the cached bytes into a pooled writer — zero new allocations on the hot path."
Rust `basis_network_server_library.rs:16,22-28,40-44` stores a `Vec<u8>` behind a `Mutex` and
does `cache.clone()` on every `send_library_to_peer` and every broadcast, allocating a fresh
`Vec` per peer join. Why: cloning out from under the lock is the simple way to avoid holding it
across a send. The allocation is small (the library blob) and joins are not a per-tick path, but
it is a measurable regression against a property the C# called out explicitly. Not pinned by a
test.

**2. Library item count saturates instead of wrapping.**
C# `BasisNetworkServerLibrary.cs:75` writes `(ushort)count`, which wraps modulo 65536 for a
library of more than 65535 items — the count field then disagrees with the item bytes that
follow, and clients mis-parse. Rust `basis_network_server_library.rs:70` writes
`u16::try_from(len).unwrap_or(u16::MAX)`, which saturates. Both are wrong for that case
(neither refuses the payload), but they are wrong differently. Why: the Rust cast is the safe
default; the case is unreachable in practice. Not pinned by a test.

**3. `SetStatic` no longer resurrects a concurrently-removed resource.**
C# `BasisNetworkResourceManagement.cs:325` writes the whole (value-type) record back with
`UshortNetworkDatabase[modifyResource.LoadedNetID] = resource;`. If another thread removed that
id between the read at `:292` and this write, the C# re-inserts it — a deleted prop comes back,
and its per-creator count is now one short of reality. Rust
`basis_network_resource_management.rs:263-266` uses `get_mut`, which mutates in place and does
nothing when the entry is gone; the broadcast still goes out either way. Why: `get_mut` is the
natural DashMap idiom. Effect is a narrow race the Rust closes. Not pinned by a test.

**4. Preload session state is actually synchronized.**
C# `BasisNetworkPreloadResourceManagement.cs:31-32` declares `ReadyPeers`/`FailedPeers` as plain
`HashSet<int>` and mutates them with no lock from two threads: the network receive thread
(`HandleClientReady`, `:121,126`) and the disconnect path (`RemovePeer`, `:245-246`).
`TotalPeerCount` is likewise incremented unlocked from `BasisNetworkResourceManagement.cs:162`
on a late join. Rust wraps the whole session in `Arc<Mutex<SyncLoadSession>>`
(`basis_network_preload_resource_management.rs:42,150-168,236-253`) and exposes the late-joiner
bump as a locked `add_late_joiner` (`:56-64`). Why: the C# is a real data race that can corrupt
the set's bucket chain. Recorded as a deviation because the Rust can now block briefly where the
C# never did. Not pinned by a test.

**5. Timeout task: `Task.Delay` → `IrohRuntime::spawn` + `AbortHandle`, with a new failure path.**
C# `BasisNetworkPreloadResourceManagement.cs:105,142-156` fires an async `Task.Delay` cancelled
by a `CancellationTokenSource`. Rust `basis_network_preload_resource_management.rs:126-141`
spawns on the iroh runtime and keeps an `AbortHandle`. The Rust adds a branch the C# has no
equivalent of: if the spawn itself fails (`:136-140`) it logs and completes the session
immediately, because a session with no timer would wait forever on a peer that never answers.
Why: `Task.Delay` cannot fail to start; `spawn` on a shut-down runtime can. Not pinned by a test.

**6. `remove_peer` defers deletions to after the iteration.**
C# `BasisNetworkPreloadResourceManagement.cs:242-265` removes from `ActiveSessions` while
enumerating it (legal for `ConcurrentDictionary`). Rust collects `emptied`/`completed` and
removes after the loop (`basis_network_preload_resource_management.rs:237-262`) because removing
from a `DashMap` while holding an iterator's shard guard deadlocks. Same observable outcome; the
Rust re-checks `contains_key` at `:258` before firing the spawn signal, which the C# also does
via `TryGetValue` at `:271`. No behavioural difference found. Not pinned by a test.

Compared and found identical (no deviation): `Reset` unloading only non-persistent resources and
decrementing counts (`BasisNetworkResourceManagement.cs:72-107` /
`basis_network_resource_management.rs:96-108`); `LoadResource`'s cap-check-then-duplicate-check
ordering and both log lines (`:188-215` / `:163-192`); the two `UnloadResource` overloads
including the admin-lock check, the creator-or-moderator rule, and removing only after
validation (`:219-282` / `:197-234`); `SetStatic`'s admin-tier authorisation table, the
admin-locked-implies-static normalisation, and the no-op short-circuit (`:290-341` /
`:239-283`); `PredownloadResource` deliberately not entering the database (`:180-187` /
`:149-161`); `SendOutAllResources`' late-joiner handling, including that the `LoadStrategy = 0`
rewrite touches only the outgoing copy and never the stored record (`:157-169` /
`:130-138`); `BroadcastSpawnSignal`'s `Mode == 1` gate and the `excludeNetId` exclusion
(`BasisNetworkPreloadResourceManagement.cs:179-182,201-231` /
`basis_network_preload_resource_management.rs:192-231`); the zero-peer immediate-completion path
(`:96-102` / `:117-121`); and the library wire layout `[u16 rawLen][u16 compressedLen][payload]`
with LZ4 used only when it is strictly smaller and fits a `u16`
(`BasisNetworkServerLibrary.cs:99-125` / `basis_network_server_library.rs:82-89`).

## Corners cut

* **No behavioural tests for this module on either side.** Grepping
  `basis_server/basis_server_tests/` and `Basis Server/BasisServerTests/` finds no reference to
  `BasisNetworkResourceManagement`, `BasisNetworkPreloadResourceManagement` or
  `BasisNetworkServerLibrary`; the only hits are in the REST API suites
  (`basis_server/basis_rest_api_tests/tests/rest_api_tests.rs`,
  `Basis Server/BasisRestApi.Tests/RestApiTests.cs`), which drive resource load/unload through
  HTTP rather than asserting on the cap or the counters.
  `ControlAndResourceMessageRoundTripTests` covers only the message wire formats. So the
  per-creator cap, the O(N) recount heal, and the disconnect release are unpinned in both
  languages.
* The hot-path allocation the C# comment promises to avoid is no longer avoided (deviation 1).
* `basis_network_server_library.rs:60-62` adds an `invalidate_cache()` the C# lacks; it is a
  test seam that nothing currently calls.
* `basis_network_resource_management.rs:286-289` adds `clear_for_tests()`; likewise unused by
  any test today.

## Improvements

* **The preload session race is closed** (deviation 4). The C# version can corrupt a `HashSet`
  under a concurrent add from the receive thread and a remove from the disconnect thread — the
  classic symptom is a hang inside the set, which for this class means a synchronized load that
  never completes.
* **A session can no longer hang forever if the timer fails to start** (deviation 5).
* **`SetStatic` cannot resurrect a deleted resource** (deviation 3).
* **Serialisation failures are handled rather than assumed away.** Every `serialize` call site in
  the Rust is `if …is_ok()`-guarded (e.g. `basis_network_resource_management.rs:84,139,151,178`,
  `basis_network_preload_resource_management.rs:106,198,225`), where the C# calls `Serialize`
  unconditionally and would broadcast whatever the writer happened to hold.
* `try_add_to_database` (`basis_network_resource_management.rs:30-38`) makes the
  check-and-insert a single entry operation, where C# `LoadResource` does a `ContainsKey` at
  `:195` and a `TryAdd` at `:199` — two lookups with a window between them (the C# does handle
  the losing race, at `:205-208`).

## Verdict

Behaviourally faithful. Both caps match their C# values and their config overrides, the
per-creator counter is maintained and healed the same way, and the disconnect path releases
exactly the same state in the same order. The three substantive differences all favour the
Rust — the unsynchronized preload session sets, the resurrect-on-`SetStatic` race, and the
"session hangs if the timer never starts" gap are all fixed. The one thing the port gave up is
the C#'s explicit zero-allocation library send: `send_library_to_peer` now clones the cached
blob per join. As with `handlers`, the exposure here is coverage — no test on either side touches
the resource cap or the preload lifecycle.
