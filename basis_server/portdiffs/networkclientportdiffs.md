# BasisNetworkClient — port diffs

C#: `Basis Server/BasisNetworkClient/` · Rust: `basis_server/basis_network_client/src/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `NetworkClient.cs` | `network_client.rs` | 78 → 137 | ported; `manualMode` dropped, `Poll`/`Update` are no-ops |
| `BasisDIDAuthIdentityClient.cs` | `basis_did_auth_identity_client.rs` | 105 → 177 | ported; PlayerPrefs → optional JSON store |
| `BasisDIDAuthIdentityProvider.cs` | `basis_did_auth_identity_provider.rs` | 33 → 28 | ported; Unity auto-register → explicit call |
| — | `lib.rs` | 0 → 13 | new (crate root and re-exports) |
| `BasisNetworkClient.csproj` / `.asmdef` | `Cargo.toml` | — | n/a; no Unity assembly definition in Rust |

## Deviations

### N1. `manualMode` dropped; `poll()` and `update()` are no-ops.

C# `StartClient` takes `bool manualMode = false` (`NetworkClient.cs:17`) and calls
`client.StartManual()` or `client.Start()` accordingly (`:23-26`); `Poll()` forwards to
`PollEvents()` (`:43-46`) and `Update(ms)` to `ManualUpdate(ms)` (`:47-50`). Rust `start_client`
has no such parameter (`network_client.rs:44-51`), always calls `start_default()` (`:60`), and
both pump methods are empty (`:99`, `:101`), documented as kept only so callers written against
the C# shape still compile (`:97-98`).

**Is that safe for every caller? For event delivery, yes — and not only because of iroh.**

* No Rust transport implements manual mode at all. `start_manual`, `poll_events` and
  `manual_update` exist only as `NetManager` trait defaults returning `Unsupported`
  (`basis_network_core/src/transport/basis_network_shell.rs:284-292`), and a grep across
  `basis_server/**/*.rs` finds no override in the iroh, LiteNetLib or mixed implementations. So
  there is no queued state a caller could leave un-pumped: calling `poll()` was never going to do
  anything even if it forwarded.
* The Rust LiteNetLib port raises listener events inline on the receive task — the C# server's
  `UnsyncedEvents = true` mode — and runs acks, resends, pings and timeouts on a dedicated logic
  thread started in `start`
  (`basis_network_core/src/transport/lnl_network_impl/net_manager.rs:4-11`, receive tasks at
  `:681`, logic thread at `:694-698`).
* Both in-tree callers still call the no-ops in a driver loop and are unaffected because the loop
  paces itself: `basis_network_client_console/src/main.rs:322-323`, with the tick sleep at
  `:419-421` — the same shape as the C# loop at
  `BasisNetworkClientConsole/BasisNetworkClientConsole/Program.cs:338-339` and its sleep at `:449`.

**What is not safe is the other thing manual mode bought.** C#
`NetManager.Start(..., manualMode: true)` skips the per-socket receive threads
(`LiteNetLib/NetManager.Socket.cs:819`) and the logic thread, which is exactly why the load-test
console asked for it (`BasisNetworkClientConsole/.../Client/ClientManager.cs:169` and `:237`,
which drive a thousand-plus clients from a fixed pool of driver threads). In Rust every
`LnlNetManager::start` spawns its **own** logic OS thread and builds its **own** rayon peer pool
of up to `cores` threads (`net_manager.rs:686-698`). A process that creates many `NetworkClient`s
on the `litenetlib` stack pays that per client.

In practice the blast radius is limited: the Rust default stack is iroh (N2), which shares one
runtime, and the only many-client Rust harness (`basis_network_client_console`) uses the default.
The mixed-world tests create a handful of LNL clients. But the capability the C# console depended
on is gone, no test covers many LNL clients in one process, and a caller who needs deterministic
thread accounting has no way to ask for it — and no error telling them so, because `poll()`
returns `()` rather than the `Unsupported` the trait would have produced.

### N2. The default network stack changed.

`start_client` passes `configuration.network_stack_id` straight to the registry
(`network_client.rs:57`), and the field defaults to `""` on both sides
(`BasisNetworkCore/Configuration/BasisServerConfiguration.cs:72`;
`basis_network_core/src/configuration/basis_server_configuration.rs:45`). But an empty id resolves
to `LiteNetLibId` in C# (`BasisNetworkCore/Transport/BasisNetworkStackRegistry.cs:37`, used at
`:157`) and to `IROH_ID` in Rust
(`basis_network_core/src/transport/basis_network_stack_registry.rs:86`, used at `:188`). The same
`Configuration` therefore produces a different transport. The decision belongs to
`basis_network_core`, but `NetworkClient::start_client` is where it becomes observable.

### N3. Reuse and connect-failure handling.

* C# sets `IsInUse = true` only after `Connect` returns (`NetworkClient.cs:33-34`). If `Connect`
  throws, the transport has already been started (`:24-26`) and is leaked — no `Stop()` runs.
  Rust stops it on the error path (`network_client.rs:67-73`) before returning.
* C# `Shutdown()` (`NetworkClient.cs:74-77`) calls `client?.Stop()` and never clears `IsInUse`;
  only `NotifyServerOfDeparture()` clears it (`:70`). So `Shutdown()` called on its own leaves the
  object permanently unusable — the next `StartClient` takes the else branch, logs
  "Call Shutdown First!" and returns `null` (`:37-41`). Rust `shutdown()` clears `is_in_use` and
  drops the peer and listener handles (`network_client.rs:124-136`). This is a latent C# bug that
  the port fixed; neither behaviour is pinned by a test on either side.
* Reuse is signalled by `null` plus a log line in C# (`:39-40`) and by
  `Err(ErrorCode::Conflict, "Call Shutdown First!")` in Rust (`network_client.rs:53-55`).

### N4. Identity persistence: PlayerPrefs → an optional JSON file.

C# `GetOrSaveDID` (`BasisDIDAuthIdentityClient.cs:24-56`) is wholly inside
`#if UNITY_2017_1_OR_NEWER`. The non-Unity build returns `string.Empty` (`:54`) and never
initialises the static `Key` or `DID` (`:18-19`), so a headless C# caller that reached
`IdentityMessage` (`:58-88`) would be signing with a default key — the sign at `:66` would fail
or produce nothing usable.

Rust `get_or_save_did` (`basis_did_auth_identity_client.rs:85-107`) always produces a usable
identity. It persists to `identity-did.json` (`:61`) only when `set_store_directory` was called
(`:64-66`), and on an unreadable or unwritable store it logs and falls back to a fresh in-memory
identity so a client can always connect (`:91-94`). The three stored field names are the ones
PlayerPrefs used — `PrivateKeyDID`, `PublicKeyDID`, `DIDID` (`:123-127` vs
`BasisDIDAuthIdentityClient.cs:21-23`) — and the values are the same base64 encodings. The Rust
adds a key-length check on load (`:138-140`) that the C# had no equivalent of.

### N5. `IdentityMessage` signature and failure reporting.

C# is `IdentityMessage(NetPeer peer, NetPacketReader Reader, out NetDataWriter Writer) -> bool`
(`BasisDIDAuthIdentityClient.cs:58`); the `peer` parameter is never used, and a malformed
challenge is not detected — `ChallengeBytes.Deserialize` (`:63`) result is discarded. Rust is
`identity_message(reader) -> Option<NetDataWriter>` (`basis_did_auth_identity_client.rs:151-161`)
and returns `None` for a malformed challenge (`:153`) or a missing identity (`:152`).

The response frame is unchanged: `[signature bytes][fragment bytes]`, with the fragment replaced
by `"N/A"` when empty (C# `:80-86`, Rust `:48-50`).

Note the error-string typo was fixed: "Unable to Very Key" (`BasisDIDAuthIdentityClient.cs:73`)
became "Unable to Verify Key" (`basis_did_auth_identity_client.rs:43`). Nothing asserts on it.

### N6. Provider registration is explicit.

C# registers itself from a Unity `[RuntimeInitializeOnLoadMethod]`
(`BasisDIDAuthIdentityProvider.cs:24-31`) and does nothing at all outside Unity. Rust exposes
`auto_register()` (`basis_did_auth_identity_provider.rs:14-17`) that a headless host has to call.
`PlayerIdentity` also gains a `properties` field, defaulted at the construction site (`:26`).

### Checked and found identical

The connect payload is byte-for-byte the same, which is the part that has to be: the protocol
version as a bare `ushort` with no key prefix (`NetworkClient.cs:27-29` vs
`network_client.rs:62-64`; `ServerVersion` is `ushort` at
`BasisNetworkCore/Protocol/BasisNetworkVersion.cs:11` and `u16` at
`basis_network_core/src/protocol/basis_network_version.rs:16`), then the auth bytes as a
`BytesMessage`, then the `ReadyMessage` (`NetworkClient.cs:30-32` vs `network_client.rs:65-66`).
`Disconnect` keeps the same order and the same two log lines (`NetworkClient.cs:51-57` vs
`network_client.rs:104-109`), and `NotifyServerOfDeparture` still clears the in-use flag before
disconnecting the peer (`:70-71` vs `:113-120`). The cross-language tests exercise this handshake
in both directions (`basis_server_tests/tests/networking/csharp_interop_tests.rs:242`, `:387`,
`:406`).

## Corners cut

* Manual mode is not merely unimplemented in this crate — it is unimplemented in every Rust
  transport (N1). The trait keeps the three methods so the shape survives, but nothing can satisfy
  them.
* `poll()` and `update()` are silent no-ops rather than errors (`network_client.rs:99`, `:101`).
  A caller that genuinely needs a pumped transport gets no signal.
* No test targets this crate directly: there is no `mod tests` in any of its three source files,
  and `basis_server_tests` reaches it only through `basis_hello_world_client`. The C# has no
  dedicated test project either, so this is parity rather than a regression, but the fixes in N3
  are consequently unpinned.
* `set_store_directory` (`basis_did_auth_identity_client.rs:64`) is process-global, as PlayerPrefs
  was. Two clients in one process share one identity file — which is why the hello world client
  bypasses it entirely (`basis_hello_world_client/src/basis_hello_client.rs:429-435`).
* The Unity build path has no analogue and is simply absent: no `.asmdef`, no `PlayerPrefs`, no
  `RuntimeInitializeOnLoadMethod`. Expected for a server-side port, but it means this crate is not
  a drop-in replacement for the C# assembly the Unity client compiles.

## Improvements

* A failed connect no longer leaks a started transport (`network_client.rs:67-73` vs
  `NetworkClient.cs:33`).
* `shutdown()` makes the object reusable again (`network_client.rs:126-128`), where the C#
  left it permanently wedged (N3).
* Errors are typed and carry context: `BasisError::permanent(ErrorCode::Conflict/Transport, …)`
  plus `.context(…)` on each serialization and on the connect
  (`network_client.rs:54`, `:58`, `:60`, `:65`, `:66`, `:71`), against a `null` return and a
  `BNL.LogError` in C# (`NetworkClient.cs:39-40`).
* A headless client actually has an identity (N4), where the C# non-Unity path returned an empty
  DID and an uninitialised key.
* A malformed auth challenge is detected and refused rather than signed
  (`basis_did_auth_identity_client.rs:153` vs `BasisDIDAuthIdentityClient.cs:63`), and stored keys
  are length-checked on load (`:138-140`).
* State is behind one `Mutex<Inner>` (`network_client.rs:15`, `:52`) rather than four unsynchronised
  fields (`NetworkClient.cs:7-10`), so the reuse check and the assignment that follows it cannot
  race.

## Verdict

The wire behaviour is a faithful port and is covered by cross-language tests in both directions.
The lifecycle handling is better than the original in three specific ways (N3, N4, N5).

The one deviation that deserves a decision rather than a note is N1. Making `poll`/`update` no-ops
is safe for event delivery — provably so, because no Rust transport has a manual mode for them to
forward to — and both existing callers pace their own loops. But manual mode in the C# also meant
"do not spawn transport threads", and that guarantee is gone: each `LnlNetManager::start` now
takes a logic thread plus a rayon pool. Anyone porting the C# load-test topology (a thousand
LiteNetLib clients in one process) will hit that, and the API gives them no way to notice. If the
no-ops are to stay, they are worth a doc line saying which guarantee was dropped, and the LNL
manager is worth a shared logic thread or pool across instances.

N2 is worth double-checking against deployment configs: the same empty `NetworkStackId` now means
iroh instead of LiteNetLib.
