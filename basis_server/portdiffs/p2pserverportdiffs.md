# p2p (server) — port diffs

C#: `Basis Server/BasisNetworkServer/P2P/` · Rust: `basis_server/basis_network_server/src/p2p/`

The direct-connection broker: session signalling between two clients, the peer introduction that
gets them talking directly, and the offload table the relay consults to decide it may stop
forwarding voice and avatar traffic for a pair.

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BasisServerP2PBroker.cs` | `basis_server_p2p_broker.rs` | 498→489 | ported, cap and bookkeeping identical, 5 deviations |
| `LNLPeerIntroducer.cs` | `iroh_peer_introducer.rs` | 28→27 | rewritten for a different transport (deviation 1) |
| — | `mod.rs` | —→6 | new (module wiring / re-exports) |

Totals: 526 C# lines across 2 files → 522 Rust lines across 3 files.

## Session cap and offload bookkeeping

| Item | C# | Rust | same? |
| --- | --- | --- | --- |
| Per-peer outstanding session cap | `BasisServerP2PBroker.cs:41` `MaxSessionsPerPeer = 4096` | `basis_server_p2p_broker.rs:57` `MAX_SESSIONS_PER_PEER = 4096` | yes |
| Cap only counts *distinct* tokens | `:218-220` `Count >= cap && !ContainsKey(token)` | `:221-223` `len() >= cap && !contains(token)` | yes |
| Response when over cap | `:222-224` log + `P2PSub_Cancel` to sender | `:225-227` log + `P2P_SUB_CANCEL` to sender | yes |
| Pair key | `:43-48` `((long)lo << 32) \| (uint)hi`, order-normalised | `:59-62` same, order-normalised | yes |
| Fast-path counter | `:56-58` `_offloadedPairCount`, `Volatile` read | `:48,67-69` `AtomicI64`, `Relaxed` read | yes |
| Self-pair never offloaded | `:62` `a == b` → false | `:72` `a == b` → false | yes |
| Increment only on a new pair | `:173-176` on `TryAdd` success | `:172-174` on `insert(..).is_none()` | yes |
| Decrement only on a real removal | `:289-292`, `:433-436` on `TryRemove` success | `:294-296`, `:428-430` on `remove(..).is_some()` | yes |
| Offload requires both LinkUps | `:171-186` | `:169-184` | yes |
| LinkLost clears offload, keeps session | `:278-298` | `:284-306` | yes |
| Disconnect clears both | `:410-437` | `:397-431` | yes |
| Restart clears sessions + offloads | `:101-104,115-121` | `:91-103` | yes (but see deviation 5) |

The bookkeeping is a literal match: `pack_pair` normalises the order the same way and produces
identical values (including the sign-extend-lo / zero-extend-hi asymmetry), the counter is only
touched when the map actually changed, and every path that tears a session down
(`remove_session`, disconnect, decline, cancel) also removes the pair. The
`has_offloaded_pairs()` fast path exists for the same stated reason on both sides — the avatar
send loop tests it per pair, so on a server with no direct sessions it is one atomic load rather
than a map lookup.

## Deviations

**1. Peer introduction: LiteNetLib NAT punching → iroh `EndpointAddr` exchange.**
This is the core of the module and it is a rewrite, not a translation.

C#: the broker installs an `EventBasedNatPunchListener` on the transport's `NatPunchModule`
(`BasisServerP2PBroker.cs:106-110`), and clients drive it out-of-band by sending NAT-introduce
requests straight at the socket. `OnNatIntroductionRequest` (`:312-389`) collects two
`(internal, external)` `IPEndPoint` pairs in arrival order, then calls
`NatPunchModule.NatIntroduce(aInternal, aExternal, spray, bInternal, bExternal, spray, token)`
(`:378-385`). Around that sit three LiteNetLib-specific behaviours with no Rust counterpart:
port-prediction spray sized from `LNLTransportConfig.NatPortPredictionRange`
(`:354`, `GetPredictionRange` at `:397-408`); same-NAT detection by comparing external addresses
(`:346-349`); and the same-host loopback rewrite, which replaces both internal endpoints with
`127.0.0.1` when two clients share a machine because punching a host's own LAN address is
commonly dropped by the OS (`:356-373`). `LNLPeerIntroducer.cs:14-21` forwards
`IPeerIntroducer.Introduce` straight to `NatIntroduce`.

Rust: iroh endpoints hole-punch themselves once each side knows the other's `EndpointAddr`, so
the broker only has to swap addresses. Clients send an in-band `P2P_SUB_INTRODUCE_REQUEST`
(sub-type 8) carrying a serialized `EndpointAddr`; `handle_p2p_message` routes it at
`basis_server_p2p_broker.rs:109-121` and `on_introduce_request` (`:319-357`) fills the same
two arrival-ordered slots. When both are in, it answers each side with a
`P2P_SUB_INTRODUCE` (sub-type 9) carrying the other's address plus a `dial` flag so exactly one
side dials (`:346-356`, `send_introduce` at `:372-388`). `IrohPeerIntroducer::introduce`
(`iroh_peer_introducer.rs:18-20`) delegates to the broker rather than to a NAT module.

Why: the transport changed. iroh does its own hole punching and relay fallback, so spray,
same-NAT detection and the loopback rewrite have nothing to act on — there are no `IPEndPoint`s
in the flow at all (`PeerIntroduction.internal`/`.external` are set to `None` at
`basis_server_p2p_broker.rs:336`; only `iroh_addr` is used). Note the sub-type numbers 8 and 9
already exist in the shared protocol file (`BasisNetworkCore/Protocol/BasisNetworkCommons.cs:1235,1237`),
but the C# broker's switch (`BasisServerP2PBroker.cs:130-153`) handles only 0–6, so a C# server
answers an introduce request with `[P2P] Unknown sub-type 8`.

Pinned by tests: `basis_server_tests/tests/networking/basis_p2p_connection_lifecycle_tests.rs`
drives request → accept → link-up → link-lost → disconnect through the real
`handle_p2p_message`; the end-to-end introduce path is exercised by the direct-link tests in
`mixed_world_hello_tests.rs` (`:127-162`), which open a real link between two iroh clients.

**2. Sessions naming a legacy LiteNetLib peer are declined.**
Rust `handle_request` refuses at `basis_server_p2p_broker.rs:210-218`: if either the sender or
the target reports `!direct_link_capable()`, the broker logs which side is legacy and returns a
`P2P_SUB_CANCEL` to the requester instead of registering a session. `handle_p2p_message` applies
the same rule to an introduce request from a legacy peer (`:110-113`). The capability comes from
the transport: the default is `true` (`basis_network_core/src/transport/basis_network_shell.rs:257-259`)
and the LiteNetLib peer overrides it to `false`
(`basis_network_core/src/transport/lnl_network_impl/net_manager.rs:984-987`).

The C# has no concept of this — `NetPeer` has no capability flag (grepping the C# tree for
`DirectLinkCapable` finds nothing), because a C# server only ever spoke LiteNetLib, so every
peer could punch. Why the Rust needs it: the Rust server serves a mixed world — legacy
LiteNetLib clients and iroh clients in one instance — and a legacy client has no way to
hole-punch to an iroh one. Declining immediately, rather than letting the session sit until the
client's confirm-timeout, is what lets the requester fall back to the server relay at once.

Pinned by a test: `mixed_world_hello_tests.rs:127-162`
(`legacy_clients_are_never_offloaded_to_direct_links`) asserts both directions come back false,
asserts the decline is immediate (`started.elapsed() < 20s`, `:140`), asserts traffic between
them still lands tagged `HelloTransport::ServerRelay` (`:154-155`), and keeps an iroh-to-iroh
pair as the control that still offloads (`:158`).

**3. Introduce requests from outside the session pair are rejected.**
Rust `basis_server_p2p_broker.rs:332-335` drops an introduce request whose sender is neither the
initiator nor the target. The C# cannot make this check — `OnNatIntroductionRequest`
(`:312-389`) is handed endpoints by the NAT module, not a peer id, so any host that learns a
token can fill a slot. Why: the Rust request arrives in-band on an authenticated peer's channel,
so the sender is known and worth checking. Related: the Rust refreshes a slot when the same peer
re-sends (`:338-344`), where the C# would put a second request from one side into the *other*
side's slot (`:330-341`) and then introduce a peer to itself. Not directly pinned by a test.

**4. Malformed signal frames are dropped explicitly instead of throwing.**
C# `HandleP2PMessage:125-128` reads the sub-byte and deserializes unguarded; a short frame
throws out of the handler into `BasisNetworkMessageProcessor.cs:55-65`, which counts a protocol
error against the peer and can escalate to a disconnect. Rust `:106-126` returns early on both
the sub-byte read and a failed `deserialize`, logging `[P2P] Malformed signal message …`.
Why: the port replaced exceptions with `Result`. Consequence: a client can send malformed P2P
frames without accruing protocol errors — the same cross-cutting gap recorded in
`handlersportdiffs.md` deviation 2. Pinned on the Rust side by
`basis_p2p_connection_lifecycle_tests.rs:358` (`truncated_signal_frames_are_dropped_without_a_reply`),
which has no C# counterpart.

**5. Transport identity is a raw address rather than a held reference.**
C# guards re-initialisation on `ReferenceEquals(_natManager, manager)` (`:90`) and stores the
manager itself in a static field (`:79,110`), so the object stays alive and its identity can
never be reused. Rust stores only `Arc::as_ptr(&server) as *const () as u64`
(`basis_server_p2p_broker.rs:86-94`) in `INITIALIZED_MANAGER` (`:49`) and does not keep the
`Arc`. `stop_server` drops the old manager (`core/network_server.rs:284-288`) without touching
the broker, and `initialize()` is the only thing that clears stale sessions across a restart
(called from `core/network_server.rs:458`, the same place the C# calls it from,
`Core/NetworkServer.cs:266`). If the allocator reuses the freed block for the new manager — the
same size and layout, so entirely plausible — `initialize()` sees a matching identity, returns
at `:88-90`, and skips `reset_sessions()`. The C# comment at `BasisServerP2PBroker.cs:97-100`
spells out why that matters: peer ids are reissued to different players after a restart, and a
surviving offloaded pair makes the send loop skip relaying between two new peers who have no
direct link, so neither ever hears the other. Why the port did it this way: `NetManager` has no
`identity()` method (`basis_network_core/src/transport/basis_network_shell.rs:272-313`) and the
pointer was the available stand-in. **I have not observed this in practice** — it needs a
restart plus an address reuse — but the mechanism is real and the C# is immune to it by
construction. Not pinned by a test; `reset_for_tests()` (`:456-459`) clears
`INITIALIZED_MANAGER`, so the test suite never exercises the guard.

Also dropped with the transport (no Rust counterpart, inapplicable rather than missing): the
`NatPunchEnabled=false` startup warning (`BasisServerP2PBroker.cs:92-95`), `UnsyncedEvents`
setup (`:109`), and `SessionState.Punched` being reached via `NatIntroduce` — the Rust reaches
the same state via `send_introduce` (`:349`).

Compared and found identical (no deviation): the sub-type dispatch table for 0–6 and the
unknown-sub-type log (`:130-153` / `:127-135`); `HandleRequest`'s full deny ladder in the same
order — empty token, `DirectConnectLocked` for a non-admin (`PermNodes.ModerationGlobalLock` /
`PermNodes::MODERATION_GLOBAL_LOCK`), self-request, target offline, over cap — and which of
those send a `Cancel` versus dropping silently (`:189-243` / `:187-249`); the `ServerArmed`
confirmation sent to the initiator after registration (`:242` / `:248`); `HandleAccept`'s
pair-match check, the `ReadyForPunch` transition, and dropping the session when the initiator
has already left (`:245-270` / `:251-280`); `ApplyLinkUp`'s sender-must-be-in-the-pair check and
the `Offloaded` confirmation to both peers (`:162-187` / `:148-185`); `ApplyLinkLost` re-arming
the session rather than dropping it, and forwarding to the other peer even when the session is
unknown (`:278-298` / `:284-306`); `ForwardAndDrop` for Decline and Cancel (`:300-310` /
`:308-315`); `RemovePeer` notifying each survivor by `Cancel` before tearing the session down
(`:410-426` / `:397-416`); the per-peer token tracking helpers (`:439-451` / `:433-441`); and
`Preview`'s 8-character token elision including the `(empty)` sentinel (`:391-395` / `:390-395`).

## Corners cut

* **The NAT-punching heuristics are gone with the transport.** Port-prediction spray, same-NAT
  detection and the same-host loopback rewrite (`BasisServerP2PBroker.cs:346-386`) have no Rust
  equivalent. This is correct — they are LiteNetLib mechanics and iroh does its own hole
  punching — but it is ~40 lines of hard-won field knowledge that now lives only in the C#. If a
  future transport ever needs UDP punching again, it has to be rebuilt.
* **The restart guard is weaker than the C#'s** (deviation 5).
* No test drives `on_introduce_request` directly; the introduce path is only covered end-to-end
  through real iroh clients (`mixed_world_hello_tests.rs`), which means the slot-filling,
  refresh and outside-the-pair branches (`:332-344`) are not individually pinned.

## Improvements

* **Legacy peers are declined immediately** (deviation 2), with a test pinning that the decline
  is fast and that the pair still communicates via the relay.
* **Introduce requests are authorised against the session pair** (deviation 3); the C# NAT path
  structurally could not do this.
* **Per-session locking.** C# `Session` (`:15-31`) is a bag of mutable fields; only
  `OnNatIntroductionRequest` takes `lock (s)` (`:328`), while `ApplyLinkUp` (`:166-167`),
  `HandleAccept` (`:258`) and `ApplyLinkLost` (`:284-288`) mutate the same fields with no lock at
  all. Rust wraps each session in `Arc<Mutex<Session>>` (`:45`) and every mutation takes it
  (`:153,257,286,327`).
* **Malformed frames are reported rather than thrown** (deviation 4), with a test the C# suite
  lacks.
* Test parity is otherwise exact: all 14 C# `P2PBrokerOffloadTests` map one-to-one onto
  `p2p_broker_offload_tests.rs`, and all 14 C# `BasisP2PConnectionLifecycleTests` onto
  `basis_p2p_connection_lifecycle_tests.rs`, which adds the truncated-frame case.

## Verdict

The signalling half is a faithful port — the deny ladder, the state machine, the cap and every
line of the offload bookkeeping match, and the offload lifecycle tests map one-to-one. The
introduction half is a deliberate rewrite for a different transport, and it is the right one:
iroh's `EndpointAddr` exchange replaces NAT punching, and the mixed-world decline for legacy
peers is a rule the C# never needed because it only ever served one protocol. Both are pinned by
tests. The one thing worth acting on is deviation 5: `INITIALIZED_MANAGER` holds a raw address
without keeping the `Arc` alive, so a restart that reuses the allocation would skip the session
reset that the C# comment identifies as the dangerous case — two new players inheriting a stale
offload and never hearing each other. Storing the `NetManagerRef` (or adding `identity()` to the
trait) would close it.
