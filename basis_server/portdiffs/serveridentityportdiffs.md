# identity (server) — port diffs

C#: `Basis Server/BasisNetworkServer/Identity/` · Rust: `basis_server/basis_network_server/src/identity/`

The network-ID database: the instance-wide `string → ushort` allocator for networked objects,
its per-peer assignment cap, and the shared-space exhaustion guard.

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BasisNetworkIDDatabase.cs` | `basis_network_id_database.rs` | 163→168 | ported, cap identical, 4 deviations |
| — | `mod.rs` | —→4 | new (module wiring / re-export) |

Totals: 163 C# lines in 1 file → 172 Rust lines across 2 files.
(`BasisNetworkIDDatabase.cs.meta` is a Unity asset stub, not ported.)

## Per-player ID cap

| Limit | C# | Rust | same? |
| --- | --- | --- | --- |
| Default per-player id cap | `BasisNetworkIDDatabase.cs:27` `DefaultMaxNetworkIdsPerPlayer = 32768` | `basis_network_id_database.rs:32` `DEFAULT_MAX_NETWORK_IDS_PER_PLAYER = 32768` | yes |
| Config override | `BasisNetworkIDDatabase.cs:29-33` `Configuration?.MaxNetworkIdsPerPlayer`, `> 0` wins | `basis_network_id_database.rs:39-42` `max_network_ids_per_player`, `> 0` wins | yes |
| Shipped config default | `BasisNetworkCore/Configuration/BasisServerConfiguration.cs:342` `= 32768` | `basis_network_core/src/configuration/basis_server_configuration.rs:89` `= 32768` | yes |
| Shared ushort space | `BasisNetworkIDDatabase.cs:80` `newCounter > ushort.MaxValue` | `basis_network_id_database.rs:93` `u16::try_from(new_counter)` | yes |
| Warn-once per peer at the cap | `BasisNetworkIDDatabase.cs:66-69` `PerPeerCapWarned.TryAdd` | `basis_network_id_database.rs:77-82` `PER_PEER_CAP_WARNED.insert(..).is_none()` | yes |
| Counter start | `BasisNetworkIDDatabase.cs:14` `counter = -1` | `basis_network_id_database.rs:19` `AtomicI32::new(-1)` | yes |

Cap semantics match: a lookup of an id the peer already registered is served from the database
before the cap is consulted (`BasisNetworkIDDatabase.cs:44-57` /
`basis_network_id_database.rs:52-67`), only a *new* assignment increments the count
(`:97` / `:108`), and the count is cleared on disconnect by `RemovePeer`/`remove_peer` (`:37-41`
/ `:46-49`) so a rejoin starts fresh. Both are wired into the same disconnect teardown
(`Core/BasisServerHandleEvents.cs:393` / `core/basis_server_handle_events.rs:438`).

## Deviations

**1. The Rust returns a `Limit` error where the C# logged once and dropped.**
Two sites:

* Per-peer cap. C# `BasisNetworkIDDatabase.cs:64-71` warns the first time a peer hits the cap
  and then `return;`s for every subsequent request — the caller gets no signal at all, and
  neither does the client. Rust `basis_network_id_database.rs:74-87` keeps the identical
  warn-once behaviour but also returns
  `Err(BasisError::permanent(ErrorCode::Limit, "peer N is at its per-player network-id limit …"))`.
* Shared-space exhaustion. C# `:80-91` rolls the counter back, logs once behind
  `exhaustedLogged`, and `return;`s. Rust `:93-105` does the same rollback and once-only log and
  returns `Err(… ErrorCode::Limit, "the network-id space is exhausted …")`.

Why: the C# signature is `void`, so "refused" and "assigned" are indistinguishable to the
caller — the only record of a refusal is a log line that, by design, is printed once per session.
Making the refusal a value lets a caller decide, and lets a test assert on it. The port keeps the
log-flood protection intact rather than trading it for the error: the sole production caller,
`core/basis_server_handle_events.rs:1148-1154`, explicitly suppresses logging when
`e.code() == ErrorCode::Limit`, precisely because "logging it per message would hand a capped
client a log storm" — so the observable log output is unchanged from the C#, and no message is
sent to the refused client in either language.

**Pinned by tests, on the Rust side only.**
`basis_server_tests/tests/infrastructure/ownership_state_and_id_database_tests.rs:652-664` asserts
`overflow.is_err()` and that a second overflow request is also an error;
`:676-707` (`per_peer_id_cap_limits_one_client_but_not_others_or_existing_ids`) counts the
refusals — `assert_eq!(refused, 16)` at `:690` — and asserts a lookup of an already-owned id
still succeeds (`:699`). The C# counterparts,
`Basis Server/BasisServerTests/Infrastructure/OwnershipStateAndIdDatabaseTests.cs:800-833` and
`:838-869`, can only assert on the database contents (`Assert.Equal(4, …Count)`) because there
is no return value to check. Every other call site in both suites is `…expect("add")` /
a bare call, so the error type does not otherwise change the tests' shape.

**2. `GetAllNetworkID(out list)` → `Option<Vec<…>>`.**
C# `BasisNetworkIDDatabase.cs:115-129` always fills the `out` list and returns `Count != 0`; an
empty database yields `false` *and* a non-null empty list, which
`OwnershipStateAndIdDatabaseTests.cs:704-706` asserts on explicitly. Rust
`basis_network_id_database.rs:133-142` returns `None` for empty, `Some(vec)` otherwise, so the
"false plus empty list" pair collapses to one value. Why: `Option` is the idiomatic encoding and
no caller used the list when the bool was false. Pinned:
`ownership_state_and_id_database_tests.rs:538` asserts `get_all_network_id().is_none()` on an
empty database.

**3. An empty-string object id is now removable by value.**
C# `RemoveUshortNetworkID` (`:130-150`) finds the matching entry with `FirstOrDefault` and then
tests `!string.IsNullOrEmpty(itemToRemove.Key)`. That check is doing double duty: it is meant to
detect "no match" (the default `KeyValuePair` has a null key), but it also rejects a genuine
match whose key is the empty string — such a mapping can be created (`AddOrFindNetworkID` never
validates the string) and then can never be removed. Rust `:144-157` matches on
`Option<String>`, which separates "not found" from "found, key is empty", so the empty-string
mapping is removed like any other. Why: `Option` distinguishes the two cases the C# sentinel
conflated. Narrow and unlikely to be reachable in practice. Not pinned by a test on either side.

**4. `exhaustedLogged` is an `AtomicBool` rather than an interlocked `int`.**
C# `:15,86` uses `Interlocked.Exchange(ref exhaustedLogged, 1) == 0`; Rust `:20,98` uses
`EXHAUSTED_LOGGED.swap(true, SeqCst)`. Identical semantics, noted only for completeness.

Compared and found identical (no deviation): the "already known" fast path and its
`TrySend`-to-one-peer reply (`:44-57` / `:52-67`); the ordering of cap check → log → counter
increment → database insert → broadcast-to-all (`:63-111` / `:72-128`); the
`Interlocked.Increment`-then-`Decrement` rollback at the ceiling and the fact that both share
the same benign race where concurrent callers past the ceiling transiently inflate the counter
(`:77-91` / `:92-105`); the messages of all seven log lines; and `Reset` clearing the database,
both per-peer maps, the counter and the exhausted flag (`:152-161` / `:159-167`).

## Corners cut

None found. The Rust file is longer than the C# (168 vs 163 lines) and drops nothing: every
method, every log line, both per-peer maps and the exhaustion guard are present. The only
additions are the two `Err` returns and a `ushort_network_database()` accessor
(`basis_network_id_database.rs:35-37`) standing in for the C# public field.

## Improvements

* **A refusal is now a value, not just a log line** (deviation 1). The single caller currently
  ignores it deliberately (to preserve the C#'s log-flood protection), but the information now
  exists — and the Rust tests use it to assert the exact refusal count, which the C# tests
  cannot.
* **The empty-string id can be removed** (deviation 3).
* **`Option` removes the "false but non-empty out-param" ambiguity** (deviation 2).
* Test coverage is a superset: the Rust suite mirrors all nine C# tests one-for-one
  (`ownership_state_and_id_database_tests.rs:505-620` ↔
  `OwnershipStateAndIdDatabaseTests.cs:661-796`) and adds the two refusal assertions above.

## Verdict

The closest of the four modules. The per-player cap, its config override, its shipped default,
the warn-once behaviour, the per-session clearing on disconnect and the shared 65,536-id
ceiling are all literal matches. The one intentional divergence — `Limit` errors where the C#
silently returned — is well-handled: the caller suppresses the log for exactly that code, so the
production log output is unchanged, and the Rust tests pin the refusal where the C# tests could
only infer it from the database size. The `RemoveUshortNetworkID` empty-key difference is a real
if obscure C# bug the port happens to fix.
