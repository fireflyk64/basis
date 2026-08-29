# core — port diffs

C#: `Basis Server/BasisNetworkServer/Core/` · Rust: `basis_server/basis_network_server/src/core/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `NetworkServer.cs` | `network_server.rs` | 409 → 627 | ported; static fields become accessors over one `LazyLock<State>`, `StartServer`/`SetupServer`/`StartListening`/`InitializeAuth` return `BasisResult`, plus a Rust-only `describe_listeners` |
| `BasisServerHandleEvents.cs` | `basis_server_handle_events.rs` | 1492 → 1257 | ported; every C# method has a counterpart, including the whole `JoinBroadcast` nested class |
| `BasisServerEventsRouter.cs` | `basis_server_events_router.rs` | 156 → 103 | ported; same 11 event types, same two delivery methods |
| `BasisServerControl.cs` | `basis_server_control.rs` | 198 → 246 | ported; `IServerControl` and all 8 methods, records become structs |
| `IsExternalInit.cs` | — | 9 → 0 | not ported; a `netstandard2.x` compiler shim for `init` accessors, no Rust analogue exists or is needed |
| — | `mod.rs` | 0 → 6 | Rust-only module wiring |

## Deviations

**The connection lifecycle order is identical.** `HandleConnectionRequest`
(`BasisServerHandleEvents.cs:516-598`) and `handle_connection_request`
(`basis_server_handle_events.rs:537-607`) run the same six gates in the same order:
IP ban → peer limit → version u16 present → version match → password (`UseAuth`) →
`UseAuthIdentity` branch / `ReadyMessage` + headless check. Note that the peer-limit check comes
*before* the version check in both trees (`BasisServerHandleEvents.cs:526-533` ↔
`basis_server_handle_events.rs:543-553`), so a client on the wrong protocol version hitting a full
server gets `RejectKind_ServerFull`, not `RejectKind_VersionMismatch`. The reject payloads match:
plain string for "Banned IP" / "Invalid client data." / "Malformed auth payload" /
"Authentication failed, Auth rejected"; structured `RejectMagic`+kind+aux0+aux1+message for
server-full and version-mismatch (`BasisServerHandleEvents.cs:461-481` ↔
`basis_server_handle_events.rs:480-507`). The post-accept gates in `OnNetworkAccepted`
(`BasisServerHandleEvents.cs:607-642` ↔ `basis_server_handle_events.rs:616-647`) also match in
order: AllowList → BanList → RejoinOnly (with the `ConfigurationEditor` bypass) → display-name
sanitization → `TryAdd` → reconnect-collision eviction → retry → "Peer already exists."

**The disconnect cleanup list is identical.** `CleanupPeerSubsystems`
(`BasisServerHandleEvents.cs:348-406`) and `cleanup_peer_subsystems`
(`basis_server_handle_events.rs:394-447`) perform the same 20 steps in the same order — UUID
resolution with the `BasisSavedState` metadata fallback, `RemoveConnection(id, peer)`, the
holder guard that aborts when a reconnect already owns the id, the four UUID-keyed removals, the
fifteen id-keyed removals (ownership, saved state, reduction, PIP, content share, image cache,
image bandwidth, preload, opus bitrate, P2P, net-id database, scene egress, message-processor
error counts, registry subscription, join sequence) and finally the value-matched removal from
`AuthenticatedPeers`. `HandlePeerDisconnected` (`:408-442` ↔ `:449-463`) then rebuilds the
snapshot, resets the three global databases when the table empties, and enqueues the leave, in
that order. Nothing is missed and nothing is reordered.

Everything below is a difference outside that spine.

1. **`AllowSendSocketGrowth` is never applied.** `NetworkServer.cs:307-312` sets
   `lnlServer.manager.AllowSendSocketGrowth = MaxSendSockets != 1` inside `StartListening`.
   `network_server.rs:482-511` (`start_listening`) has no equivalent, and a repo-wide grep finds
   `allow_send_socket_growth` nowhere in the Rust tree (only `Basis Server/LiteNetLib/NetManager.cs:296`
   and `NetManager.Socket.cs:743` on the C# side). The socket *count* is still honored —
   `initialize_pulse_settings` reads `max_send_sockets` at `network_server.rs:306-313` and feeds
   the reduction system — but the transport's "may add send sockets under load" switch stays at
   its default. No test.

2. **A panic in connection handling no longer rejects the request.** `BasisServerHandleEvents.cs:593-597`
   catches every exception out of `HandleConnectionRequest` and answers with
   `RejectWithReason(ConReq, "Fatal Connection Issue ...")`, so the client always gets a verdict.
   The Rust has no catch-all; a panic is contained one level up by the transport shell
   (`basis_network_core/src/transport/basis_network_shell.rs:380-391`, called from `:398-402`),
   which logs and moves on, leaving the request neither accepted nor rejected — the client waits
   out its connect timeout instead. The reachable surface is small because the parse failures that
   threw in C# are `Result`s in Rust, but it is a real difference. No test.

3. **Missing auth objects fail closed with a different message.** `network_server.rs` leaves
   `auth`/`auth_identity` as `Option`s; `basis_server_handle_events.rs:570` treats a missing
   comparer as "not authenticated" and `:581-583` rejects with "Fatal Connection Issue: no auth
   identity". The C# fields (`NetworkServer.cs:77-78`) would throw a `NullReferenceException` and
   land in the catch-all at `BasisServerHandleEvents.cs:595`. Same fail-closed outcome, different
   reject text. Pinned indirectly by `basis_server_tests/tests/networking/basis_connection_lifecycle_tests.rs:166-177`.

4. **The authenticated-peer table is keyed by the full peer id.** `BasisServerHandleEvents.cs:601`
   computes `ushort PeerId = (ushort)newPeer.Id` and inserts under that, while
   `CleanupPeerSubsystems` (`:348, 371, 404`) and `RejectWithReason(NetPeer)` (`:484`) look up
   with the untruncated `peer.Id`. Rust uses `i32` on both sides
   (`basis_server_handle_events.rs:610, 649, 733-741`). Only observable above peer id 65535, which
   the configured `PeerLimit` normally precludes. Improvement.

5. **`handle_auth` and `raise_server_received` clone the reader per subscriber.**
   `BasisServerHandleEvents.cs:749-753` hands one `NetPacketReader` to the whole multicast
   delegate, so a second subscriber would see a consumed reader; `basis_server_handle_events.rs:371-381`
   gives each handler its own clone. One subscriber exists in practice (`BasisDIDAuthIdentity`),
   so this is latent. Not pinned.

6. **A framing failure in `JoinBroadcast` sends an empty packet.** `basis_server_handle_events.rs:284-291`
   returns `Vec::new()` when `ServerReadyBatchMessage::serialize` fails, and `flush` at `:227-234`
   sends and counts those zero bytes. The C# `Frame` (`BasisServerHandleEvents.cs:275-301`) would
   throw and `WorkerLoop`'s catch (`:136-143`) would abandon the whole flush. Neither path is
   reachable without a serializer bug; neither is tested.

7. **`JoinBroadcast::stop` detaches a slow worker rather than blocking on it.**
   `BasisServerHandleEvents.cs:90` calls `thread.Join(500)`. `basis_server_handle_events.rs:126-132`
   polls `is_finished` for 500 ms and only joins when it has finished, otherwise dropping the
   handle; the thread exits on its own at the next `JOIN_RUNNING` check. Deliberate, per the code
   comment at `:125`.

8. **`subscribe_server_events` is idempotent.** `basis_server_handle_events.rs:318` unsubscribes
   first; `BasisServerHandleEvents.cs:305-313` would double-subscribe if called twice (and then
   dispatch every packet twice). Improvement.

9. **The default network stack differs.** `NetworkServer.cs:276` passes `configuration.NetworkStackId`
   verbatim to `BasisNetworkStackRegistry.Create`, and `SubscribeEvents` (`:263-265`) registers an
   `LNLPeerIntroducer` for `LiteNetLibId`. `network_server.rs:467-475` substitutes
   `SERVER_DEFAULT_ID` when the configured id is blank, and `subscribe_events` (`:448-457`)
   registers an `IrohPeerIntroducer` for `IROH_ID` and `MIXED_ID` instead. This is the port's
   transport story, not an accident, but a blank `NetworkStackId` behaves differently in the two
   trees.

10. **`clear_all_worlds` releases the creator's resource quota.** `BasisServerControl.cs:98`
    removes each scene straight out of `UshortNetworkDatabase` and never calls
    `NoteResourceRemoved`, so `PerCreatorCount` (`Basis Server/BasisNetworkServer/Resources/BasisNetworkResourceManagement.cs:56-70`)
    stays permanently inflated for every creator whose world was cleared — the normal unload path
    at `BasisNetworkResourceManagement.cs:226` does decrement it. `basis_server_control.rs:160-162`
    calls `note_resource_removed(&removed.uuid_of_creator)`. Genuine bug fix. Not pinned by a test.

11. **Control-surface listings are ordered.** `basis_server_control.rs:200` and `:217` sort
    `list_worlds` / `list_players`; `BasisServerControl.cs:124-152` returns concurrent-dictionary
    enumeration order. The REST API output is now deterministic.

12. **The events router does not count a truncated packet against the peer.**
    `BasisServerEventsRouter.cs:11` reads the type byte with `reader.GetByte()`, which throws on an
    empty payload and is counted by `ProcessMessage`; `basis_server_events_router.rs:17-19`
    returns early. Same class of finding as the messaging module's main deviation — see
    `messagingportdiffs.md`.

13. **`SendClientListToNewClient` no longer leaks pooled writers.** `BasisServerHandleEvents.cs:1214-1215`
    rents two writers inside a `try` whose `catch` at `:1243-1246` does not return them, so a throw
    from `Message.Serialize` or `FlushReadyBatch` permanently drains the shared pool.
    `basis_server_handle_events.rs:1056-1077` returns both on every path. Improvement.

14. **Send failures are logged instead of thrown.** `NetworkServer.cs:397` calls `client.Send`
    directly, so a payload the transport refuses throws out of the broadcast loop.
    `network_server.rs:609-617` logs and returns `false`, and the loop continues to the next peer.
    Improvement.

## Corners cut

- **No message pooling on the voice path.** `HandleVoiceMessage` and `HandleShoutVoiceMessage`
  rent and return an `AudioSegmentDataMessage` from `ThreadSafeMessagePool`
  (`BasisServerHandleEvents.cs:869, 880, 903, 936`); `basis_server_handle_events.rs:825` and `:843`
  allocate a fresh `AudioSegmentDataMessage::default()` per packet. Voice arrives ~50 Hz per
  speaker, so this is allocation churn the original avoided. Behaviorally identical.
- **`SendVoiceMessageToClients` copies the target list instead of renting.**
  `BasisServerHandleEvents.cs:996-997` copies into an `ArrayPool<NetPeer>` rental under the list
  lock; `basis_server_handle_events.rs:904` clones the whole `Vec<NetPeerRef>` per voice packet.
  (Partly offset by the `has_offloaded_pairs` short-circuit at `:917`, which the C# lacks — it
  called `IsP2POffloaded` per recipient at `BasisServerHandleEvents.cs:1022`.)
- **`stop_server` has no error containment.** `NetworkServer.cs:118-125` and `:131-132` wrap
  `Server.Stop()` and `AuthIdentity.DeInitialize()` in try/catch and log a warning;
  `network_server.rs:288` and `:294-296` call them bare. A panic there would propagate out of
  `stop_server` and abort a restart mid-way.
- **`ApplyLiveConfiguration` is a straight transcription** (`NetworkServer.cs:203-226` ↔
  `network_server.rs:381-407`) and inherits the original's `if (Server == null) return;` early exit
  before the broadcast half, so a live config edit applied before the transport exists is silently
  half-applied in both trees.
- **No test covers `NetworkServer::start_server`, `stop_server`, `initialize_pulse_settings` or
  `apply_live_configuration`.** The lifecycle tests install fakes directly
  (`basis_server_tests/tests/networking/basis_connection_lifecycle_tests.rs:47-61`) rather than
  booting the server, so the ~70 setting assignments in `initialize_pulse_settings`
  (`network_server.rs:302-373`) are unverified against the C# list.

## Improvements

- `initialize_auth` returns a `BasisResult` and `start_server` propagates it with context
  (`network_server.rs:409-433`, `:261`); the C# lets `Directory.CreateDirectory` and
  `PermissionIntegration.Init` throw out of `StartServer` unannotated (`NetworkServer.cs:240-247`).
- `setup_server` turns "the stack id names nothing" into a typed error
  (`network_server.rs:473-475`) instead of the C#'s implicit null return (`NetworkServer.cs:276`).
- The writer pool is a fixed-capacity `ArrayQueue` (`network_server.rs:75`), so the C#'s
  `_writerPool.Count < MaxPooledWriters` check-then-enqueue race (`NetworkServer.cs:70-73`) cannot
  overshoot the cap.
- `describe_listeners` (`network_server.rs:515-530`) prints what each transport is actually
  reachable at — no C# equivalent.
- The `handle_peer_disconnected` null-peer guard (`BasisServerHandleEvents.cs:412-416`) is
  unrepresentable in Rust; `NetPeerRef` is non-null by construction.
- `remove_authenticated_peer_if_same` (`network_server.rs:219-221`) names the C#'s
  `((ICollection<KeyValuePair<int, NetPeer>>)…).Remove(kvp)` idiom
  (`BasisServerHandleEvents.cs:404-405`, `:493`), which is easy to misread as an unconditional
  remove.
- Reasonable test coverage of the part that matters: `basis_server_tests/tests/networking/basis_connection_lifecycle_tests.rs`
  pins the pre-accept gates (`:67, 97, 115, 129, 148, 166, 181`), the post-accept admission gates
  (`:205, 222, 239, 257, 275, 292`), the reconnect-collision path (`:309, 330, 423, 450, 472, 492`)
  and the stale-disconnect cases (`:407, 511`). The C# has no equivalent suite.

## Verdict

The two things that matter most in this module — the order of the admission checks and the
completeness of the disconnect teardown — are faithful, step for step, and the Rust side has a
test suite pinning them that the C# never had. The reject payloads match byte for byte.

Two real gaps: `AllowSendSocketGrowth` is silently dropped (deviation 1), and an unexpected panic
during connection handling leaves the client hanging where the C# would have rejected it
(deviation 2). Two genuine fixes travel the other way: the `ClearAllWorlds` quota leak
(deviation 10) and the `SendClientListToNewClient` writer leak (deviation 13). The rest is
type-system translation and ordering determinism. `initialize_pulse_settings` is the largest
untested surface in the module and the easiest place for a transcription slip to hide.
