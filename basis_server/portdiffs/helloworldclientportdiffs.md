# BasisHelloWorldClient — port diffs

C#: `Basis Server/BasisHelloWorldClient/` · Rust: `basis_server/basis_hello_world_client/src/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `Program.cs` | `main.rs` | 127 → 151 | ported; same flags, same ring, same output shape |
| `BasisHelloClient.cs` | `basis_hello_client.rs` | 500 → 436 | ported; pump thread removed, virtuals → `HelloExtension` |
| `HelloPeerClient.cs` | `hello_peer_client.rs` | 483 → 459 | ported; subclass → composition, same P2P sequence |
| — | `lib.rs` | 0 → 12 | new (the C# is an exe with no library root) |
| `BasisHelloWorldClient.csproj` | `Cargo.toml` | 46 → 30 | Rust needs no `basis_iroh_ffi` copy step |

Both trees now take a network-stack id, but differently: C# through a mutable static plus an
optional constructor argument (`BasisHelloClient.cs:54`, `:111-114`), Rust through the constructor
only (`basis_hello_client.rs:117-125`). See H5.

## Deviations

### H1. The shared pump thread is gone.

C# `BasisHelloClient.Pump` (`BasisHelloClient.cs:435-498`) is one background thread for every
client in the process, ticking at 5 ms (`:438`), calling `Tick` → `_client.Poll()` +
`_client.Update(ms)` + `OnTick(ms)` (`:277-291`) under a lock held across the whole sweep so a
`Disconnect` on another thread cannot stop a transport mid-poll (`:474-493`). It is gated by
`_pumping` (`:78`, `:176`, `:279`).

The Rust has none of it. `basis_hello_client.rs:10-11` records why: the transport delivers events
from its own runtime, so handlers fire on transport threads. The `OnTick` hook
(`BasisHelloClient.cs:370-372`) has no counterpart in the `HelloExtension` trait
(`basis_hello_client.rs:53-61`), and no extension needed one.

This is safe for the stacks that exist (see `networkclientportdiffs.md` N1) and the direct-link
stress test drives it hard (`basis_server_tests/tests/networking/hello_world_peer_stress_tests.rs:44`).

### H2. Ordering race on the auth challenge in `connect`. The C# is structurally safer here.

C# assigns `_peer` first (`BasisHelloClient.cs:168`), then subscribes `OnReceive` (`:174`), then
arms the pump (`:176`) — and because the transport is in manual mode, nothing is dispatched at all
until `_pumping` is true and `Tick` runs (`:279`). The comment at `:73-78` is explicit that this is
deliberate: "a tick that lands between StartClient and that subscription cannot dispatch the auth
challenge into nothing — which fails the handshake in a way that looks like a server-side reject".

The Rust reverses the order on a transport that is already live. `start_client` returns at
`basis_hello_client.rs:236` with the connect payload already on the wire; the receive subscription
goes in at `:242-246`; and `self.peer` is only populated at `:263`. `on_receive` decides "is this
the server?" by comparing the incoming peer against `self.peer` (`:330`). While that is still
`None`, `is_server` is false, so the message is routed to the extension (`:331-336`) — dropped
entirely for a plain `BasisHelloClient`, and dropped by `handle_peer_message` for a
`HelloPeerClient` because no session matches the peer (`hello_peer_client.rs:402-409`). An auth
challenge landing in that window is lost silently and `connect` returns `Ok(false)` only after the
full timeout.

The window is three `subscribe` calls wide, against a server that must first complete a QUIC or
LiteNetLib handshake, so it is unlikely — but it is real, it is not pinned by any test, and it is
the one place where the port removed a guarantee the original documented. Moving `:263` above
`:242` closes it.

### H3. `DirectEndpoint` was not ported.

C# `HelloPeerClient.DirectEndpoint` (`HelloPeerClient.cs:77`) returns
`(Transport as IrohNetManager)?.ConnectionString`, and the demo prints it on every successful link
(`Program.cs:86`: `direct link up (own endpoint …)`). Rust has `endpoint_addr_bytes()`
(`hello_peer_client.rs:145-151`, the JSON form the introducer needs) and a static
`connection_string` (`:320-326`), but no equivalent accessor, and `main.rs:121` prints only
`direct link up`. Cosmetic, and not pinned.

### H4. An unknown stack id is refused rather than silently downgraded.

C# never validates. `BasisHelloClient` stores whatever it is handed (`BasisHelloClient.cs:114`),
and `BasisNetworkStackRegistry.Create` logs a warning and falls back to the default
(`BasisNetworkCore/Transport/BasisNetworkStackRegistry.cs:161-166`), so `--stack nonsense` quietly
runs on LiteNetLib. Rust `with_stack` checks the registry up front and returns
`InvalidArgument` (`basis_hello_client.rs:123-125`), which `main.rs:64-78` turns into exit code 1.
Better, and a genuine behaviour change for anyone scripting the demo.

### H5. The `NetworkStackId` static is gone.

C# has a process-wide mutable default (`BasisHelloClient.cs:54`) that `Program.Main` sets from
`--stack` (`Program.cs:36`) and that the C# test fixture flips to LiteNetLib
(`BasisServerTests/Networking/HelloWorldPeerMessageTests.cs:41`). The per-instance `StackId`
(`:61`, `:114`) falls back to it. Rust has no static: `new` hard-codes `IROH_ID`
(`basis_hello_client.rs:117-119`) and `with_stack` takes the id (`:122`). Cleaner, but any C#
caller that set the static has to be rewritten to pass the id.

### H6. Subclassing became composition.

C# `HelloPeerClient : BasisHelloClient` overrides `OnTransportReady`, `HandleOtherChannel`,
`HandlePeerMessage` and `Disconnect` (`HelloPeerClient.cs:165`, `:171`, `:187`, `:208`). Rust
composes: `HelloPeerClient` holds an `Arc<BasisHelloClient>` and installs itself as that client's
`HelloExtension` (`hello_peer_client.rs:104-108`; trait at `basis_hello_client.rs:53-61`; dispatch
at `:332`, `:344`, `:252`, `:260`, `:322`). The base holds a `Weak` to itself for the event
closures (`:106`, `:127`, `:241`), which is what keeps the cycle from leaking.

`OnTransportReady` disappears because the base now subscribes the connection-request and
peer-connected events itself (`basis_hello_client.rs:248-262`) and forwards them to whatever
extension is installed, rather than handing the raw listener to a subclass. Two consequences worth
knowing: `set_extension` is public and last-writer-wins (`:180-182`), where C# fixed the subclass
at construction; and the extension's handlers now return `bool` (`:55-57`) that no caller reads.

### H7. Handshake, message framing and direct-link logic — checked line by line, no difference found.

* **Ready message.** Same display name, DID as `playerUUID`, `"Headless"` platform; same non-empty
  avatar blob `"basis-hello-world-no-avatar"` (needed so `ReadyMessage` validation passes); same
  all-zero pose sized `ConvertToSize(BitQuality.High)` with `DataQualityLevel = High`, scales 1.0,
  no additional avatar data. `BasisHelloClient.cs:135-162` vs `basis_hello_client.rs:199-224`.
* **Challenge response.** Deserialize a `BytesMessage` nonce, `Ed25519.Sign`, write
  `[signature][utf8 "N/A"]`, send on `AuthIdentityChannel` ReliableOrdered.
  `BasisHelloClient.cs:337-357` vs `basis_hello_client.rs:351-371`. Both hello clients skip the
  `Ed25519.Verify` round-trip that `BasisDIDAuthIdentityClient.IdentityMessage` does
  (`BasisNetworkClient/BasisDIDAuthIdentityClient.cs:71-75`), so that is parity, not a cut.
* **Join signal.** The metadata channel is what sets `joined` on both sides
  (`BasisHelloClient.cs:311-316` vs `basis_hello_client.rs:341`), with the same reasoning that the
  connection accept has already populated the player id.
* **Relay frame.** `SceneDataMessage { messageIndex = 0xE0C0, recipients = [target], payload }` on
  `SceneChannel`, ReliableOrdered (`BasisHelloClient.cs:222-236` vs `basis_hello_client.rs:302-315`).
  `HelloMessageIndex` matches (`BasisHelloClient.cs:47` / `basis_hello_client.rs:112`). The Rust
  sets `recipients_size` explicitly at `:307`, but `SceneDataMessage::serialize` recomputes it from
  `recipients.len()` (`basis_network_core/src/serializable/scene.rs:81`), so the two cannot drift.
* **Payload encoding.** `[0][i32 LE]` and `[1][utf8]` (`BasisHelloClient.cs:188-204` vs
  `basis_hello_client.rs:278-288`). Decode guards are equivalent: C# requires total `length >= 5`
  (`:399`), Rust requires `body.len() >= 4` after `split_first` (`:390`).
* **Direct frame.** `[ushort messageIndex][payload]` written raw on `DirectSceneChannel`, with the
  sender taken from the connection rather than from the bytes — explicitly to close a spoofing hole
  (`HelloPeerClient.cs:144-150` and `:187-206` vs `hello_peer_client.rs:192-197` and `:402-419`).
  The framing matches because C# `NetDataWriter.Put(byte[])` writes raw bytes with no length prefix
  (`LiteNetLib/Utils/NetDataWriter.cs:295-298`), as `put_bytes` does
  (`basis_network_core/src/io/net_data_writer.rs:196-198`).
* **Fallback when no link is up.** Same payload relayed by the server on
  `DirectSceneServerChannel`, and still reported as `ServerRelay` on receipt
  (`HelloPeerClient.cs:154-162` and `:179-183` vs `hello_peer_client.rs:199-202` and `:394-397`).
* **P2P signalling.** Same sub-ids in the same order: `Request` carrying the local X25519 public
  key, `Accept` with the peer's, `IntroduceRequest` carrying this endpoint's JSON address,
  `Introduce` with a `dial` flag, `LinkUp`, and `Offloaded` / `Decline` / `Cancel` / `LinkLost`;
  `ServerArmed` is acknowledged and ignored on both sides (`HelloPeerClient.cs:224-269` vs
  `hello_peer_client.rs:207-238`). `endpointAddr` is `byte[]` / `Vec<u8>` on both
  (`BasisNetworkCore/Serializable/Protocol/BasisP2PMessages.cs:58`, `hello_peer_client.rs:278`) and
  JSON on both (`BasisNetworkCore/Transport/IrohNetworkImpl.cs:328` vs `hello_peer_client.rs:150`).
* **Inbound direct connection admission.** Read the token, require a registered session that is
  currently `punching`, otherwise reject — so the endpoint does not accept anything that finds it
  (`HelloPeerClient.cs:363-383` vs `hello_peer_client.rs:421-427`).
* **Dial-once guard.** `Interlocked.Exchange(ref session.Dialed, 1)` vs
  `dialed.swap(true, AcqRel)`, and both reset it when the dial fails (`HelloPeerClient.cs:330`,
  `:342` vs `hello_peer_client.rs:293`, `:314`).
* **Simultaneous-open race.** First to claim the by-player slot wins; the loser waits on the
  winner's confirmation (`HelloPeerClient.cs:440-445` and `:113-123` vs
  `hello_peer_client.rs:337-346` and `:161-168`).
* **Per-pair keys.** Derived and stored on both sides and used by neither — iroh's QUIC carries the
  encryption; the exchange is kept for protocol parity (`HelloPeerClient.cs:35-36`, `:464-465` vs
  `hello_peer_client.rs:17-18`, `:355`).

## Corners cut

* No `OnTick` hook for an extension that needs periodic work (H1). Nothing needed one.
* No `direct_endpoint()` (H3), so the demo's link-up line lost the endpoint it used to print.
* `main.rs:51` and `:53-54` swallow an unparseable `--port`, `--clients` or `--hops` with
  `unwrap_or` and silently use the default, where C# `int.Parse` (`Program.cs:31-34`) throws and
  the process dies loudly. Neither is right, but the Rust hides a typo.
* On the `litenetlib` stack a direct-link attempt leaves the session `punching` forever on both
  sides: C# `BeginPunching` returns early after logging "can only open direct links on the iroh
  stack" (`HelloPeerClient.cs:300-303`) and Rust `begin_punching` returns early after logging "has
  no endpoint address to be introduced with" (`hello_peer_client.rs:274-277`). Neither clears the
  flag, so this is faithful — but it is faithful to a wart. The Rust at least documents the
  outcome at `hello_peer_client.rs:98-99`, and `mixed_world_hello_tests.rs:127-159` pins that a
  legacy client is never offloaded and that the sends fall back correctly.

## Improvements

* Send failures surface. C# `Send`, `SendVia` and `SendSignal` either throw or swallow
  (`BasisHelloClient.cs:206-236`, `HelloPeerClient.cs:346-359`); the Rust threads `BasisResult`
  through (`basis_hello_client.rs:269-315`, `hello_peer_client.rs:179-203`, `:328-335`). One
  behavioural consequence: `on_inbound_request` now aborts the punch when the `Accept` could not be
  sent (`hello_peer_client.rs:254-257`), where the C# sent it blind and punched anyway
  (`HelloPeerClient.cs:283-284`).
* `raise_payload` is total. `split_first` handles an empty payload (`basis_hello_client.rs:386-388`)
  where the C# needed an explicit null-and-length check (`BasisHelloClient.cs:395`), and
  `from_utf8_lossy` (`:397`) cannot throw where `Encoding.UTF8.GetString` (`:405`) can.
* `Drop for BasisHelloClient` (`basis_hello_client.rs:419-426`) guarantees the transport is torn
  down even on an early return or a panic; the C# relies on the caller's `finally`
  (`Program.cs:102-108`).
* `HelloTransport` implements `Display` (`basis_hello_client.rs:39-46`), so the per-hop path in the
  demo output does not depend on enum `ToString`.
* `handle_relayed_scene` checks the deserialize result before reading the message
  (`basis_hello_client.rs:376`); the C# discards it (`BasisHelloClient.cs:378`).

## Verdict

The wire protocol is a faithful port. Everything a second implementation has to agree on — the
ready message, the DID challenge/response, the scene-relay frame, the direct frame, and the full
five-step P2P introduce sequence — matches line for line, and both trees carry cross-language
tests that prove it end to end: the Rust suite runs the real C# hello client against the Rust
server (`csharp_interop_tests.rs:387`, `:406`, `:422`) and Rust clients against the C# server
(`:242`, `:327`), and the C# suite boots the Rust server and joins it with C# clients
(`BasisServerTests/Networking/MixedWorldRustServerTests.cs:13-21`).

The structural changes (H1, H6) are the right shape for a transport that drives itself, and the
error handling is better throughout.

One thing should be fixed rather than recorded: H2. Populating `self.peer` before subscribing the
receive handler is a two-line move (`basis_hello_client.rs:263` above `:242`) and restores a
guarantee the C# went out of its way to document. It is a narrow window and I have not observed it
fire, but the failure mode — a lost auth challenge that presents as a fifteen-second timeout and
looks like a server-side reject — is exactly the one the C# comment warns about.
