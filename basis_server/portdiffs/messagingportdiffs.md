# messaging — port diffs

C#: `Basis Server/BasisNetworkServer/Messaging/` · Rust: `basis_server/basis_network_server/src/messaging/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BasisNetworkMessageProcessor.cs` | `basis_network_message_processor.rs` | 98 → 114 | ported; the exception handler becomes `catch_unwind`, plus test helpers `peer_error_count` and `reset` |
| `BasisServerMessageRegistry.cs` | `basis_server_message_registry.rs` | 417 → 382 | ported; all 34 `RegisterCore` bindings present and on the same channels (verified by extracting and diffing both channel lists) |
| — | `mod.rs` | 0 → 6 | Rust-only module wiring |

## Deviations

1. **Handler errors are no longer counted against the peer — the main finding.**
   `ProcessMessage` catches *every* exception a handler throws
   (`BasisNetworkMessageProcessor.cs:55-66`), and the C# reader throws on any short read
   (`Basis Server/LiteNetLib/Utils/NetDataReader.cs:94-99`, reached from `GetByte` at `:268-272`
   and `GetUnmanaged` via `GetUShort` at `:421`). So in the C#, a truncated or malformed payload on
   *any* core channel increments `_peerErrorCounts`, warns at 50 (`:86-90`) and disconnects the
   peer at 500 (`:91-96`).

   The Rust `catch_unwind` at `basis_network_message_processor.rs:52-61` only catches *panics*.
   The ported handlers turn a short read into an early `return` and never touch the counter:
   `send_avatar_message_to_clients` (`core/basis_server_handle_events.rs:755-757`),
   `send_body_fit_message_to_clients` (`:791-793`), `handle_voice_message` (`:826-828`),
   `handle_shout_voice_message` (`:844-846`), `update_voice_receivers` (`:932-934`),
   `update_voice_receivers_inverted` (`:941-943`), `update_voice_receivers_bitfield` (`:985-993`),
   `net_id_assign` (`:1144-1146`), `load_resource` (`:1159-1161`), `handle_preload_ready`
   (`:1212-1214`), `unload_resource` (`:1220-1222`), `handle_modify_resource` (`:1251-1253`),
   the events router (`core/basis_server_events_router.rs:17-19`) and the content-share sub-byte
   (`basis_server_message_registry.rs:302-304`).

   Consequence: a peer can flood malformed core-channel packets indefinitely and never reach
   either threshold. Only three paths still count in Rust — pre-auth traffic (`:38-47`), an
   unknown channel or plugin id (`:64`, `:83-92`) and a genuine panic (`:65-79`). No test pins any
   of this: `BasisNetworkMessageProcessor` has no references outside
   `basis_network_server/src/` (grep across `basis_server_tests/`, `basis_network_server/tests/`
   and `basis_rest_api_tests/`), so neither the 50-error warning nor the 500-error kick is
   exercised in either tree.

   Cross-peer isolation itself is intact and is if anything stronger: the counter is a
   `DashMap<i32, i32>` keyed by peer id (`:13, 30-34`), the escalation acts only on the offending
   `peer` (`:96-108`), and `clear_peer_errors` is called from the disconnect teardown
   (`core/basis_server_handle_events.rs:440`). One peer's malformed message cannot count against,
   disconnect, or corrupt the buffer of another.

2. **The statistics blob is a different compression format.** `BasisServerMessageRegistry.cs:342`
   calls `SnapshotResetEncode(true, 1)` — Brotli at quality 1
   (`Basis Server/BasisNetworkCore/Statistics/BasisNetworkStatistics.cs:235`).
   `basis_server_message_registry.rs:325` calls `snapshot_reset_encode(true)`, which is raw DEFLATE
   (`basis_network_core/src/statistics/basis_network_statistics.rs:198-202` and `:264-269`). The
   `ServerStatisticsChannel` payload is therefore not interchangeable between a C# and a Rust
   server. The quality argument has no analogue in the Rust API.

3. **A zero-length statistics request silently turns recording off.** `BasisServerMessageRegistry.cs:331`
   reads `reader.GetBool()`, which throws on an empty payload and is counted and escalated by
   `ProcessMessage`. `basis_server_message_registry.rs:322` uses `reader.get_bool().unwrap_or(false)`,
   which takes the `else` branch at `:337-338` and sets `IsRecordingData = false`. A peer holding
   `ServerStats` can disable stats collection with a truncated packet and pay nothing for it. Not
   pinned.

4. **Outbound statistics are no longer double-counted.** `SendSupplyTo` records the send at
   `BasisServerMessageRegistry.cs:181` and then calls `NetworkServer.TrySend`, which records it
   again (`Core/NetworkServer.cs:376-382`); `SendToPeer` does the same at `:134-135`. The Rust
   records once, inside `try_send` (`basis_server_message_registry.rs:187` and `:151`). The
   registry-control and plugin channels were reporting roughly double their real outbound volume in
   the C#. Improvement.

5. **The supplied manifest is deterministically ordered.** `BuildSupply` appends
   `PluginDescriptors` in `ConcurrentDictionary` enumeration order
   (`BasisServerMessageRegistry.cs:160-163`), so two clients connecting to the same server can be
   handed the same descriptors in different orders. `basis_server_message_registry.rs:167-168`
   sorts plugins by id before appending. Pinned by
   `basis_server_tests/tests/security/permission_and_message_catalog_tests.rs:950-1002` (id
   assignment, manifest membership, unregister/re-register).

6. **A `RegistrySub_Subscribe` that fails to deserialize is skipped, not counted.**
   `BasisServerMessageRegistry.cs:382-384` lets `subscribe.Deserialize(reader)` throw;
   `basis_server_message_registry.rs:355-357` checks the returned bool and leaves the peer's
   existing subscription untouched. Subscription semantics otherwise match, including "no
   subscription record means subscribed to everything" (`:199-201` ↔ `BasisServerMessageRegistry.cs:193-196`),
   pinned at `permission_and_message_catalog_tests.rs:1011-1022`.

7. **The "unknown plugin id" log line reports 2 more bytes than the C#.**
   `basis_network_message_processor.rs:49` snapshots `available_bytes()` *before* dispatch and
   passes it to `handle_unknown` at `:64`; `BasisNetworkMessageProcessor.cs:74` reads
   `reader.AvailableBytes` *after* `DispatchPlugin` consumed the 2-byte id
   (`BasisServerMessageRegistry.cs:208`). Log text only.

8. **An out-of-range channel no longer throws.** `ResolveCore` indexes a `TotalChannels`-long
   array (`BasisServerMessageRegistry.cs:26, 62`), so a channel byte ≥ 64 raises
   `IndexOutOfRangeException` → caught → one error counted. `basis_server_message_registry.rs:68`
   uses `.get()` and returns `None`, falling through to `handle_unknown` → also one error counted.
   Same accounting, no throw. Unreachable in practice: `TOTAL_CHANNELS = 64`
   (`basis_network_core/src/protocol/basis_network_commons.rs:38`) is also the transport's
   configured channel count (`transport/lnl_network_impl/net_manager.rs:89`) and iroh rejects
   `channel >= TOTAL_CHANNELS` on send (`transport/iroh_network_impl.rs:341-342`).

9. **`reader.Recycle()` has no counterpart, by design.** Rust's `NetPacketReader` is an alias for
   the plain owned `NetDataReader` (`basis_network_core/src/io/net_data_reader.rs:178`), so every
   "recycles inside" contract in the C# registry is discharged by the drop. Worth recording because
   the C# path is subtle: `ProcessMessage`'s catch calls `reader.Recycle()` at
   `BasisNetworkMessageProcessor.cs:64` after a handler may already have recycled it, and only the
   `_recycled` guard in `Basis Server/LiteNetLib/NetManager.cs:33, 44-54` stops that from pushing
   the same event onto the free list twice and handing one peer's buffer to another. The Rust has
   no pool and so cannot reach that state at all.

10. **Plugin id assignment differs in its overflow shape, not its output.**
    `BasisServerMessageRegistry.cs:96` increments an `int` and casts to `ushort`;
    `basis_server_message_registry.rs:110` uses `u16::wrapping_add`. Identical ids for any
    realistic plugin count; both wrap into the reserved core range past ~65,472 registrations.

## Corners cut

- **No test at all for `BasisNetworkMessageProcessor`.** The pre-auth gate, the `<= 5 || % 100`
  log throttle, the 50-error warning, the 500-error disconnect and `clear_peer_errors` are entirely
  unpinned on both sides. `peer_error_count` (`basis_network_message_processor.rs:26-28`) and
  `reset` (`:111-113`) exist as test hooks that nothing calls.
- **`send_to_peer` takes a mutex on the hot path.** `try_get_plugin_id`
  (`basis_server_message_registry.rs:130-132`) locks `PLUGIN_IDS`; the C# read a lock-free
  `ConcurrentDictionary` (`BasisServerMessageRegistry.cs:123`). Every plugin send now contends on
  one lock with plugin registration.
- **`build_supply` always copies the core catalog.** `basis_server_message_registry.rs:166`
  `to_vec()`s it unconditionally; `BasisServerMessageRegistry.cs:152-155` returned the shared array
  when no plugins are registered. Only paid once per invalidation, since both sides cache.
- **`send_to_peer`'s payload closure can fail.** The Rust signature takes
  `impl FnOnce(&mut NetDataWriter) -> NetResult<()>` and returns `false` when the payload could not
  be written (`:137, 149-154`); the C# `Action<NetDataWriter>` had no failure channel
  (`BasisServerMessageRegistry.cs:121-138`). Callers that treated `false` as "peer not subscribed"
  now get a second meaning for it.
- **`ensure_initialized` is called on every dispatch.** `resolve_core`
  (`basis_server_message_registry.rs:66-69`) calls `Self::ensure_initialized()` per packet — a
  `Once::call_once` load, cheap but not free, where the C# relied on the type initializer
  (`BasisServerMessageRegistry.cs:52-58`).

## Improvements

- **A panicking handler cannot take down the transport thread and is charged to the peer that
  caused it.** `basis_network_message_processor.rs:52-79` catches the unwind, extracts the panic
  message, applies the same `<= 5 || % 100` log throttle as the C# exception path and then runs the
  same escalation. The C# equivalent only existed because .NET exceptions are catchable; a Rust
  handler that unwound would otherwise have killed the reader task.
- The counter is bumped through a single `entry().or_insert(0)` mutation
  (`basis_network_message_processor.rs:30-34`), which is the same read-modify-write atomicity as
  `ConcurrentDictionary.AddOrUpdate` but without the C#'s re-invocation semantics under contention.
- Deterministic manifest ordering (deviation 5) and single-counted outbound statistics
  (deviation 4).
- The registry has real coverage the C# lacks: every expected inbound channel is asserted bound
  (`basis_server_tests/tests/infrastructure/config_and_registry_tests.rs:913-938`,
  `permission_and_message_catalog_tests.rs:932`), the two avatar channels are asserted to share one
  handler (`config_and_registry_tests.rs:928-929`), and plugin registration, manifest advertisement
  and subscription filtering are pinned (`permission_and_message_catalog_tests.rs:950-1022`).

## Verdict

The dispatch table is a faithful port — all 34 core channel bindings are present, on the same
channels, with the same handlers and the same delivery methods, and the plugin/subscription
machinery is both matched and better tested than the original.

The error-accounting story is not. The C# derived its per-peer error count from *exceptions*, and
the port removed exceptions without replacing the signal, so twelve-plus handlers now discard
malformed input silently and the 500-error kick is effectively unreachable through the core
channels (deviation 1). Cross-peer isolation is fine — that is not the risk here; the risk is that
a misbehaving or hostile client is never disconnected. Nothing tests the counters, so this would
not have been caught. Two smaller items deserve attention before trusting the stats channel: the
Brotli→DEFLATE format change (deviation 2) and the free "turn recording off" packet (deviation 3).
