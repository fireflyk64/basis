# No holds barred: the Basis server, C# and Rust, as built

Written after porting the whole C# server to Rust, adding the LiteNetLib wire protocol and the
mixed stack, running the cross-language interop matrix, and benchmarking both servers with the
same harness. Everything below is something I read in the code, measured, or hit while doing
that work. Where I am guessing I say so.

The short version: **neither server is an industrial-grade design yet.** Both are a
single-process, static-singleton, quadratic-broadcast server with an elaborate control system
bolted on to keep the quadratic part upright, and a legacy wire protocol that is plaintext.
The Rust port is operationally sturdier — a quarter of the memory, no collector, typed errors
with a trace, no panics by policy, bounded queues everywhere the C# had them and a few places
it did not — but it deliberately mirrors the C# structure, so it inherits every architectural
debt the C# has. That was the brief ("feel at home"), and it was the right call for a port
whose job was to be provably the same server. It is not the right end state.

---

## 1. What the C# server actually is

### 1.1 The good

* **The domain logic is complete and battle-tested.** Auth (DID challenge/response), the
  reduction system with its distance tiers, delta and bundle compression, voice priority,
  ownership, resource loading, moderation, permissions, the REST API: this is a lot of working
  product, and its edge cases are encoded in a thousand tests. The port found only a handful of
  real bugs (an unversioned transport sidecar, an image-cache offer to the wrong table, a few
  deserialisers that carried on with a half-filled struct) in ~40k lines. That is a good sign
  for the C# team's care.
* **It measures itself.** `BasisCpuBudget` leasing cores between pools with measured ceilings,
  `BasisPopulationScale` sizing queues from population and memory, the `/health` document with
  the BSR profiler window, the benchmark that fits settings to a machine and refuses to write
  ones the topology cannot judge — this is more operational self-awareness than most game
  servers ever get, and it is why the comparison in `benchmarks/` could be done at all.
* **The transport was hardened where it hurt.** The priority queue for voice, the bounded
  unreliable queues (the 40 GB backlog story in `NetPeer.cs`), `SO_REUSEPORT` multi-socket
  receive, the compact merged framing: each is a scar from a real incident, documented in
  place.

### 1.2 The bad

* **Everything is a static.** `NetworkServer`, the reduction system, the P2P broker, the ID
  database, the permission integration, the stack registry, the transport config store, the
  CPU budget, the population scaler: process-wide state with process-wide lifetimes. The
  consequences are concrete: one instance per process (no multi-tenancy, no warm standby, no
  blue/green inside a host), tests that must run under a "shared network statics" collection
  because two servers cannot coexist, restart as the only reconfiguration for half the
  settings, and dependencies that are invisible at every call site. The Rust port has the same
  statics because the brief was fidelity; see §3.
* **The core is O(N²) and everything else exists to hide that.** Every reduction tick visits
  every (sender, receiver) pair; distance tiers change the *rate* per pair, not the number of
  pairs. Slicing, shedding tiers, the adaptive tick, the send-budget percentage, the
  peers-per-worker constant fitted on one machine, the capacity ladder in the benchmark — all
  of it is a control system for a quadratic loop. It works, impressively, up to a few thousand
  players per instance. It will never work past that, and the control system's oscillation
  (documented: slice 4/5/6 swings with CPU tracking inversely across a 2.2× range) is the
  quadratic core showing through the controllers. Industrial scale means interest management:
  a spatial index so a receiver's candidate set is its neighbourhood, not the roster.
* **The legacy wire is plaintext.** LiteNetLib carries positions, voice, chat and the auth
  exchange unencrypted; the `Crc32c`/`XorEncrypt` layers exist but are not enabled. DID
  signatures prove identity; nothing protects confidentiality or integrity of the traffic
  after it. For a hobby VR world that is a known trade-off; for "a very serious business" it
  is a finding. iroh (QUIC/TLS 1.3, endpoint keys) fixes it — which is the strongest reason the
  new clients must move, and a reason the legacy path should have a sunset date.
* **The message layer has no schema.** ~100 hand-written `Serialize`/`Deserialize` pairs with
  `ushort`-prefixed arrays, a single `ushort` protocol version gating the whole connect, and
  no per-message versioning. Every field added is a flag day. The port pinned several
  deserialisers that silently produced partial structs on short input; that class of bug is
  structural until there is a schema and a generated codec.
* **Exceptions are the error model, and they are swallowed.** Handlers catch, log and
  continue; a deserialiser that throws mid-struct leaves whatever it filled in. Transient and
  permanent faults are indistinguishable at the call site. (The Rust side has `BasisError`
  with kind, code and frames precisely because of this.)
* **Peer identity is an address.** `NetPeer : IPEndPoint`, tables keyed by endpoint, and a
  `PeerNotFound`/`NetworkChanged` dance to survive a client whose NAT mapping moved. QUIC
  gives connection identity for free; the LiteNetLib design pays for its absence in every
  code path that touches a peer.
* **Manual memory management inside a managed runtime.** Packet pools with hand-written
  recycle, "double recycle sets `evt.Next = evt`" guard comments, `[ThreadStatic]` send
  batchers with finalizers: this is the CLR being fought, not used, and it is where the
  subtle bugs live. It bought real GC wins at 1000+ players; it is also unmaintainable by
  anyone who did not write it.
* **Two protocol dialects inside one library.** `Merged` and `CompactMerged`, `NetConstants.
  ProtocolId = 14` as the only capability gate, "peers that predate it are kept out": the
  transport was forked and the fork became the protocol. It works; it also means the only
  reference for the wire is this repository's copy of LiteNetLib.

### 1.3 Verdict

A capable, well-instrumented product server with a scaling ceiling, a plaintext legacy wire,
and an architecture (statics + quadratic core + schemaless messages) that resists change. Good
enough to run the business it runs today; not what I would call excellent for industrial
workloads, and not something to add another five years of features to as it stands.

---

## 2. What the Rust server actually is

### 2.1 The good

* **It is the same server, provably.** Every file mirrored, 1073 tests against 1056 C# facts,
  the C# LiteNetLib clients joining it unmodified with the same egress, the same packet counts
  and the same voice delivery as against the C# server, and the C# hello-world clients and
  the C# server each talking to the Rust side in spawned-process tests. Nothing about the
  rollout has to be taken on faith.
* **Operationally sturdier by construction.** 19 MB idle against 74 MB; no collector, so no
  pause-time column; `unwrap`/`expect`/`panic` denied outside tests; every fault a
  `BasisError` with a `FaultKind` (transient/permanent), an `ErrorCode` and a frame trace back
  to the origin; listener handlers run under `catch_unwind` so a bug in one message handler
  cannot take a transport task down; every wire length checked against the bytes present
  before allocation; the coalesced-datagram, merged-datagram and compact-merge parsers all
  fuzzed with garbage and truncations from a raw socket.
* **It is cheaper where it counts.** With the legacy crowd on one core: equal at 50 players,
  10 % cheaper at 200 with a shorter tick and no overrun, 9 % more delivered at 400 with
  less slicing — after one measured fix. The transport is a work-stealing runtime rather
  than one receive thread plus one logic thread, so it has somewhere to go on a many-core
  host that LiteNetLib does not.
* **Two transports behind one abstraction, mixed on one listener.** `NetManager`/`NetPeer`
  held; the LiteNetLib protocol, iroh and the mixed stack all sit behind it, one id space, the
  broker refusing direct links to legacy peers. That the whole server ran unchanged when a
  second transport appeared underneath it is the abstraction earning its keep.

### 2.2 The bad (this is the part that matters)

* **It copies the statics.** `NetworkServer::server()`, `STATE`, `BasisServerP2PBroker`'s
  `LazyLock` maps, the registry, the config store: all process-wide, exactly as in C#, with the
  same `serial_test` keys standing in for the C# collection attribute. Every consequence in
  §1.2 applies. This was chosen, and it is the first thing to undo.
* **The transport abstraction is LiteNetLib's, with the seams showing.** `Tag: Option<Arc<dyn
  Any + Send + Sync>>` for the authenticated marker; `i32` ids that are `u16` on the wire;
  `send` to a departed peer silently succeeding because that is what LiteNetLib did; a
  `SendError` that is a caller error by definition; `DisconnectInfo` carrying a socket error
  code no Rust transport produces. The trait should say what the *server* needs — a peer is a
  connection identity plus a channel/delivery API plus a bounded send budget — not what one
  UDP library happened to expose.
* ~~**Reliable send queues are unbounded.**~~ **Fixed** (commit `bound every queue a client
  can grow, by size rather than by time`). Both transports now carry a per-peer byte budget,
  scaled from memory and population; a send past it returns `SendError::QueueFull` and a peer
  whose queue has not drained for the grace period is disconnected with
  `DisconnectReason::SendQueueOverBudget`. The same pass bounded incomplete fragment sets by
  bytes, pending connect requests, rejected-connection state, iroh's pending handshakes and
  probe replies, QUIC's own per-connection windows, and the ownership table (per-player cap).
  The C# server still has every one of these holes: it is the same design, and this is now the
  clearest concrete argument for the Rust server in a security review.
* **Events are raised on transport threads.** The listener's handlers — the whole server —
  run on tokio workers and on the LiteNetLib receive tasks, taking the server's locks from
  there. The port had to learn the hard way where a handler may re-enter the transport (the
  reliable channel delivers outside its lock for that reason). It works; it is also the
  design that makes every new lock a deadlock audit. The transport should hand messages to
  the server over a bounded channel and the server should own its threads.
* **Per-message allocation.** A `Vec` per datagram, a `Vec` per merged entry, a `Bytes` copy
  per send. LiteNetLib pools (badly, §1.2); the Rust port does not pool at all. It did not
  show at 400 players on one core; it will show at 4000 on 32.
* **The iroh path is not yet the cheap one.** Measured: 1.4× the LiteNetLib path's server CPU
  at 100 players and 1.8× at 200, the cost inside quinn's per-connection processing, and it
  did not move with either framing change I made (`benchmarks/results/2026-08-29-two-core`).
  The benchmark could not profile it (no `perf` here) and could not let it spread across
  cores. The plan in that document is the plan; until it is executed the honest statement
  is "correct, secure, not yet cheaper".
* **The reduction system's threading was ported, not redesigned.** Its own tick thread, its
  own send pool leased from the CPU budget, rayon underneath for the LiteNetLib peer pass,
  tokio underneath for iroh: four schedulers in one process, each with a knob. It matches the
  C# and it measured fine; it is also three schedulers too many.
* **What is still not tested.** No long soak (hours, churn, reconnect storms); no adversarial
  client fuzzing at the *Basis message* layer (the raw-socket fuzz stops at the transport
  framing); no chaos on the mixed stack (one transport failing under the other); no test that
  the reliable-queue growth above is bounded, because it is not.

### 2.3 Verdict

The Rust server is the better artefact: smaller, faster from 200 players up on the legacy
path, safer by construction, and now the only server that can serve both client populations.
It is not an excellent industrial design, because it is the C# design. It needs the plan
below, and the plan is internal — none of it touches the wire.

---

## 3. The improvement plan (core data structures and abstractions)

Ordered by what unblocks what. Each is internal to the server; the LiteNetLib wire, the iroh
wire and the message formats stay as they are until item 6.

1. **A `ServerInstance` that owns everything.** Replace the statics with one struct: the
   transports, the listener, the peer tables, the reduction system, the broker, the ID
   database, permissions, configuration. Constructed by the console, passed by reference (or
   `Arc`) to handlers. This is the largest change and the most mechanical; the port's
   file-per-file structure makes it a rename campaign rather than a rewrite. It buys:
   multiple instances per process, tests that construct a server and drop it, hot
   reconfiguration by constructing a new instance beside the old, and every dependency
   visible in a signature.

2. ~~**Bounded reliable queues with a disconnect policy.**~~ **Done.** Per peer: a byte budget
   for queued-but-unsent reliable data (population-scaled like the unreliable bound), a grace
   period, then a disconnect with a reason the client can display. The principle applied
   throughout: the bound is a size, never a duration — a timer only decides when a stuck slot
   is reclaimed, so memory is bounded at every instant regardless of timing. What remains in
   this area is the same treatment for the C# server, if it is to stay in production.

3. **A message channel between transport and server.** The transport delivers
   `(peer, channel, delivery, Bytes)` into a bounded MPSC; the server drains it on threads it
   owns, in the order it chooses, with back-pressure visible as a metric. Handlers stop running
   on transport threads; the deadlock audit becomes a property of one consumer loop. Voice can
   take a separate lane so bulk never delays it, which is the priority queue idea applied on
   the receive side.

4. **A peer trait for this server.** `PeerId(u16)` newtype (it is a `u16` on the wire);
   connection identity from the transport; a typed slot for the authenticated state instead of
   `dyn Any`; send methods that return `Full` when the budget from item 2 is exhausted instead
   of silently succeeding. The transports already implement everything this needs.

5. **Interest management in the reduction system.** A spatial hash keyed on the distance
   tiers that already exist; a receiver's candidate set becomes its cells, the pair loop
   becomes candidates × receivers, and slicing/shedding become a safety net rather than the
   steady state. This is the change that moves the ceiling from thousands to tens of thousands
   per instance, and it can be done behind the existing `IBasisDistanceSolver` contract.

6. **A message schema.** Generate the ~100 codecs from a description (a small DSL or a
   `#[derive]`), with a per-message version and a compatibility test that replays every old
   encoding. Only then can a field be added without a flag day. The wire bytes for existing
   messages must not change — the generated codec has to reproduce them, which the round-trip
   suites already assert.

7. **Pool the packet path.** After 1–3, a per-transport buffer pool for datagram receive and
   merged-entry decode; measure before and after on the many-core host.

8. **Observability as a first-class output.** The `/health` document is good; it should be
   joined by per-peer queue depths, per-transport packet/byte counters (now real for iroh),
   and structured logs with the peer id on every line, so a production incident can be read
   without a debugger.

---

## 4. The rollout question

The plan on the table: **Rust server against the legacy clients first, then the new clients.**
The alternative: **Rust server plus the architecture work first, then anything.** And the
worry: does the first paint us into a corner?

My assessment: **the first order is right, with one condition — the architecture work is
scheduled behind it, not after "later".** Reasoning:

* The wire is the asset that moves slowest. Clients are Unity builds in players' hands; a
  server is a process an operator restarts. Everything in §3 is internal to that process. The
  Rust server can go to production speaking LiteNetLib to today's clients, the `mixed` stack
  can admit iroh clients as they ship, and none of items 1–7 changes what either client sees.
  Shipping the mixed server does not commit to any of the internal shapes it has now.
* The comparison is done and it favours shipping. The Rust server serves the existing crowd at
  least as well as the C# one at every rung measured, with less memory and no GC, and it is
  the only server that can also serve iroh clients. Waiting for the refactor delays the
  security fix (iroh) for new clients and keeps the plaintext path as the only path for longer.
* The corner is real but it is a product corner, not an engineering one. Once both populations
  share instances, every iroh-only capability (direct links, encryption-dependent features)
  splits the room. The rule that keeps that manageable is the one already built in: the server
  is always in the middle for a legacy peer, and any feature must degrade to "relayed by the
  server" for them. Set a sunset date for the LiteNetLib listener at the point the client base
  has moved; do not let it become permanent by default.
* What *would* corner us: adding features to the static-singleton design while it is in
  production with two transports and a 1000-test surface. Each feature makes item 1 harder.
  So: ship the mixed server as measured; make item 1 (the `ServerInstance`) and item 2 (bounded
  reliable queues) the next two engineering tasks before any feature work; run the many-core
  benchmark and the iroh profile in the same window so the iroh plan is grounded before iroh
  clients are the majority; then items 3–6 in order.

The one thing I would not do is call either server "excellent for robust industrial
workloads" in a review. The Rust one is the better foundation and the safer process; it is a
foundation, and the plan above is what turns it into the building.
