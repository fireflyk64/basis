# Porting `Basis Server` (C#) to `basis_server` (Rust)

This tree is a port of `../Basis Server` — the same server, the same protocol, the same tests —
with the LiteNetLib transport replaced by [iroh](https://iroh.computer) (QUIC) and tokio as the
async runtime. It is written so that someone who knows the C# server finds the same names in the
same places.

## Layout

| C# project                        | Rust crate                       | Notes |
|-----------------------------------|----------------------------------|-------|
| `Contrib/Crypto`                  | `contrib/crypto` (`basis_crypto`) | BouncyCastle → ed25519-dalek / x25519-dalek / hkdf / chacha20poly1305 |
| `Contrib/Auth/Did`                | `contrib/did` (`basis_did`)       | `Result.cs` dropped: Rust `Result` |
| `Contrib/Handles/Common`, `Dns`   | `contrib/handles_common`, `contrib/handles_dns` | DnsClient → hickory-resolver |
| `BasisNetworkCore`                | `basis_network_core`             | transport abstraction + iroh impl live in `transport/` |
| `BasisNetworkCompute`             | `basis_network_compute`          | CPU solver; ILGPU backend is a stub that reports unavailable |
| `BasisNetworkServer`              | `basis_network_server`           | |
| `BasisNetworkClient`              | `basis_network_client`           | |
| `BasisServerConsole`              | `basis_server_console` (bin `basis_network_console`) | |
| `BasisNetworkClientConsole`       | `basis_network_client_console`   | headless load client |
| `BasisBenchAgent`                 | `basis_bench_agent`              | |
| `BasisHelloWorldClient`           | `basis_hello_world_client`       | Rust port; the **C#** hello-world clients over iroh FFI live in `csharp/` |
| `LiteNetLib`                      | — (replaced by iroh)             | its unit tests map onto the iroh transport's framing tests where the concept exists |
| `BasisServerTests`                | `basis_server_tests`             | one test crate, one module per C# test file, same folder names |
| `BasisRestApi.Tests`              | `basis_rest_api_tests`           | |
| —                                 | `basis_iroh_ffi`                 | new: C ABI over iroh for the C# clients (P/Invoke) |
| —                                 | `basis_error`                    | new: the fault-classified, traceable error type every crate uses |

## Naming

* A C# `static class Foo` becomes `pub struct Foo;` with associated functions; the static
  fields become module-private `static`s behind atomics / `parking_lot` locks / `DashMap`.
* A C# interface keeps its `I` prefix as a trait (`IAuth`, `IPeerIntroducer`, `IDidMethod`) so a
  grep across both trees lands on the same thing.
* `PascalCase` members become `snake_case`; constants become `SCREAMING_SNAKE_CASE`.
* `Deserialize` that threw becomes `-> NetResult<()>`; `Deserialize` that logged and returned
  stays `Ok(())` after logging, exactly as before.
* The C# `out` parameters become tuples or `Option`.
* `SerializableBasis.X` is `basis_network_core::SerializableBasis::X` (a module alias).

## Error handling

The C# server leans on exceptions; a production Rust server must not panic. The rules the Rust
tree follows, enforced by `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, ...)]`
in every library crate and `#![deny(unused_must_use)]`:

* Every fallible operation returns a `Result`. Hot-path wire code uses small typed errors
  (`NetDataError`, `SendError`, `AeadError`, `ConfigFieldError`, ...) that carry the
  `file:line:column` where the fault was detected without allocating; everything above the
  packet layer uses `basis_error::BasisError` (`BasisResult<T>`).
* `BasisError` classifies every fault as **transient** (a retry can succeed: timeouts, refused
  dials, busy resources, a name server that did not answer) or **permanent** (malformed input,
  bad configuration, a missing file, a violated invariant), carries an `ErrorCode` category, the
  wrapped source error, and a trace of every `?`/`.context()` boundary it crossed. `{:?}` prints
  the full report; `RUST_BACKTRACE=1` adds a backtrace captured where the error was raised.
* `basis_error::retry::{retry_async, retry_blocking}` with a `RetryPolicy` retry only transient
  faults, with capped geometric backoff and jitter; permanent faults are returned at once.
* Absent values are `Option`; "not found" as a business outcome (an unknown player id, no such
  record) stays `Option`/`bool`, a fault becomes `Err`.
* Lengths read from the wire are checked against the remaining data before anything is
  allocated; frames over 1 MiB, datagrams over the path MTU, arrays over their `u16` prefix and
  every out-of-range slice are refused with an error rather than a panic.
* Transport event handlers run under `catch_unwind`: a bug in a message handler is logged and
  the peer's I/O keeps running instead of hanging.
* Negative tests for each of these paths live in `basis_network_core/tests/*_errors.rs`,
  `contrib/*/tests/*_errors.rs` and `basis_error`'s unit tests.

## Transport mapping (LiteNetLib → iroh/QUIC)

See the module docs of `basis_network_core::transport::iroh_network_impl`.

| LiteNetLib                         | iroh                                                        |
|------------------------------------|-------------------------------------------------------------|
| ReliableOrdered channel c          | one uni stream per (connection, direction, channel)         |
| ReliableSequenced                  | same stream as ReliableOrdered                              |
| ReliableUnordered                  | a fresh uni stream per message                              |
| Unreliable                         | datagram `[channel][payload]`                               |
| Sequenced                          | datagram `[channel|0x40][seq:u16][payload]`                 |
| connect request / accept / reject  | control bi-stream: CONNECT / ACCEPTED[peer id] / REJECTED   |
| ping / RemoteTimeDelta             | PING/PONG on the control stream                             |
| unconnected server-info probe      | short connection under ALPN `basis-probe/1`                 |
| NAT punch introduce                | `P2PSub_IntroduceRequest` / `P2PSub_Introduce` on the P2P channel carrying iroh `EndpointAddr`s |
| `MergeHold`, `CompactMerged`       | QUIC coalesces datagram frames itself                       |
| `MaxUnreliableQueuePerPeer` etc.   | per-peer bounded bulk + priority (voice) datagram queues    |

## The LiteNetLib protocol and the mixed world

The Rust server also speaks the LiteNetLib wire protocol itself, so the existing C# clients —
Unity and headless — connect to it unchanged: `basis_network_core::transport::lnl_network_impl`
is a file-for-file port of the `LiteNetLib` project (packet framing, the connect/accept/reject
handshake, reliable and sequenced channels with the 128-packet ack window, fragmentation,
ping/RTT, MTU discovery, the `Merged` and `CompactMerged` datagram framings, unconnected
messages, `SO_REUSEPORT` multi-socket receive). Not carried over: the NAT punch module (legacy
clients are never offered a direct link), the CRC/XOR packet layers (never enabled by Basis),
NTP requests and the debug latency/loss simulation.

Three stacks are registered:

| stack id     | listens on                                   | who connects                          |
|--------------|----------------------------------------------|---------------------------------------|
| `litenetlib` | `SetPort`                                    | the existing C# clients               |
| `iroh`       | `SetPort` (or `IrohTransportConfig.Port`)    | Rust clients, C# clients via the FFI  |
| `mixed`      | LiteNetLib on `SetPort`, iroh on `SetPort+1` | both at once — the server default     |

`mixed` runs both managers on one listener with one `PeerIdAllocator` (so a player id names one
player whichever transport carries them) and the process-wide peer identity counter. A legacy
peer reports `NetPeer::direct_link_capable() == false`; the P2P broker declines any session that
names one and the server keeps relaying between the two worlds — the server is always in the
middle for legacy clients, by design.

Cross-language tests (`basis_server_tests/tests/networking/csharp_interop_tests.rs`, needs
`dotnet` and the C# solution built; otherwise they say so and pass) spawn real processes both
ways: Rust LiteNetLib clients into the C# `BasisNetworkConsole`, and the C# hello-world clients
into the Rust server over LiteNetLib and over iroh through `basis_iroh_ffi`. The C# side has the
same from its end in `BasisServerTests/Networking/MixedWorldRustServerTests.cs`, which spawns
the Rust server from its release build.

## Status

- [x] contrib: crypto, did, handles (+ tests)
- [x] core: io, protocol, diagnostics, identity, pooling, math, p2p, encryption, statistics,
      sanitization, compute, compression, serializable, configuration, transport (iroh)
- [x] compute (host-vectorised solver behind the same `IBasisDistanceSolver` contract)
- [x] server (`basis_network_server`, every C# file mirrored; see "Server notes" below)
- [x] client library, Rust hello-world client (`basis_network_client`, `basis_hello_world_client`)
- [x] console (`basis_server_console`)
- [x] headless client console (`basis_network_client_console`), bench agent (`basis_bench_agent`)
- [x] iroh FFI (`basis_iroh_ffi`) + the C# hello-world clients on the `iroh` stack
- [x] server tests, REST API tests (see "Tests" below)
- [x] the LiteNetLib wire protocol (`lnl_network_impl`) and the `mixed` stack: legacy C# clients
      and iroh clients on one server (see "The LiteNetLib protocol and the mixed world")
- [x] cross-language interop tests, both directions, as spawned processes
- [x] benchmark comparison (C# server vs Rust server, same harness, same legacy crowd, plus the
      mixed and all-iroh crowds) — `benchmarks/`

## Tests

`basis_server_tests/tests/<folder>/` mirrors `BasisServerTests/<Folder>/`, one test binary per
C# folder and one module per C# file; `basis_rest_api_tests` mirrors `BasisRestApi.Tests`. The
shared fixtures live in `basis_server_tests/src/support/` (a real server on iroh, a recording
`FakePeer`, the lifecycle doubles — fake transport, recording connection request, map-backed
auth identity, `ServerStaticsScope` — and the avatar delta helpers). Tests that touch a
process-wide static run under a `serial_test` key; everything else runs in parallel.

| C# suite | Rust binary | tests |
| --- | --- | --- |
| Avatar (15 files) | `avatar` | 99 |
| Compression (7 of 12 files) | `compression` | 63 |
| Compute (2 of 3 files) | `compute` | 10 |
| Infrastructure (8 files) | `infrastructure` | 192 |
| Networking (14 files + LiteNetLib transport, mixed world, C# interop) | `networking` | 380 |
| Security (6 files) | `security` | 230 |
| Voice (2 files) | `voice` | 36 |
| BasisRestApi.Tests (2 files) | `basis_rest_api_tests` | 38 |
| Contrib (crypto, did, dns) | the contrib crates' `tests/` | 25 |

1071 Rust tests (plus 28 wire-level unit tests inside `lnl_network_impl`) against 1022 C# facts
+ 34 REST facts. Nothing in the C# tree was deleted; the C# suites still run against the C#
server, and `HelloWorldPeerStressTests` (C#) now needs the Rust server on the `iroh` stack (or
`--stack litenetlib` against the C# server).

Not ported, on purpose:

- `Compression/CompactMerge*Tests` — the framing tests are ported into `lnl_network_impl::compact_merge`
  and the transport tests into `networking/lnl_transport_tests.rs`; the wire-capture tests that
  needed a `PacketLayerBase` are covered by a hand-rolled UDP client instead.
- `Compression/{GpuLz4Experiment, PositionQuantizationExperiment, SimdCodecBenchmark,
  BundleCompressionExperiment, BundleDictionaryTrainer}` and `Compute/GpuLz4Experiment` —
  recorded measurements and tooling, not tests.
- `Voice/VoicePriorityQueueTests.{SaturatedBulkQueue_DoesNotShedVoice,
  VoiceOnPriorityQueue_ArrivesIntact}` and `Infrastructure/CoreBudgetTests.PeerUpdateSizing…` —
  they drive LiteNetLib's per-peer unreliable queues; voice rides its own QUIC stream on iroh.
- Cases that only exist because C# has `null` (null peers, null strings, null arrays) and the
  non-auto-resize `NetDataWriter` constructor, which the Rust writer does not have.
- `BasisServerBenchmark` (a tool, not a test) stays in C#: it measures both servers through
  `/health`, so porting it would only produce a second harness to keep in agreement with the
  first. `benchmarks/` drives it against either server.

Behavioural differences the port pins deliberately, each with a test:

- Reading a truncated or over-claimed message is an `Err` naming the field and position (the C#
  logged and carried on with a partial struct); writers refuse a payload that does not fit its
  length prefix instead of wrapping the count, and roll back a partially written array.
- A byte-wide player id past 255 is refused rather than truncated to another player.
- The net-id database reports a per-player cap or an exhausted id space as a `Limit` error
  (logged once per session), the caller drops the request.
- Numeric compute-device selectors out of range are refused with an "out of range" message; a
  padded platform id is not a headless platform; a target without an address formats as empty.

Bugs the port found on the way (all fixed): `encode_avatar_interval_byte` overflowed on extreme
intervals; the LNL transport sidecar carried no version stamp and was rewritten as "older" on
every boot; the health endpoint's BSR JSON was missing a brace; `try_apply_delta` overflowed on
a hostile length; a completed image was offered to the send snapshot rather than to every
authenticated peer; an undecided iroh connection request was rejected before the handler could
accept it.

## Server notes

`basis_network_server` mirrors `BasisNetworkServer` file for file (`core/`, `security/`,
`networking/`, `handlers/`, `messaging/`, `reduction/`, `resources/`, `rest_api/`, `diagnostics/`,
`p2p/`, `identity/`, `auth/`). Static C# classes are `pub struct X;` with associated functions
over module statics (`LazyLock` + `DashMap`/`parking_lot`/atomics); partial classes are one
`impl` block per submodule under `reduction/basis_server_reduction_system_events/`.

Where the transport changed the design, the port keeps the C# shape and documents the swap:

- **Reduction `PlayerState`** is split by writer: `SenderWork` (locked once per inbound frame),
  `ReceiverData` (locked once per receiver per phase) and an immutable `SenderFrame` published
  through `ArcSwap`, which the O(N²) send loop reads with no lock. Timing is in microseconds
  from process start (`now_ticks`, `MS_TO_TICK`).
- **Parallel.For** is a rayon pool rebuilt when the tuned degree changes; the widening trials,
  learned ceilings and budget-share controller are ported as-is.
- **P2P introduction**: LiteNetLib punched NAT holes from its own module; iroh endpoints
  hole-punch themselves once each side has the other's `EndpointAddr`, so the broker collects
  the two `IntroduceRequest` halves and sends each peer an `Introduce` (the initiator dials).
- **Send-socket growth** (SO_REUSEPORT) has no iroh equivalent; the pressure detection stays
  and warns once, pointing at the kernel buffer sysctls.
- **Memory reclaim**: no GC to force; the population-drop trigger is kept and the pass calls
  `malloc_trim` (glibc) and reports the working set. The health endpoint's `gc` block reports
  RSS and reclaim passes with `"runtime":"rust"`.
- **Health / REST**: axum on the shared `IrohRuntime`; the REST routes are a pure
  `dispatch(method, segments, body)` so they test without a socket. Bearer keys compare as
  SHA-256 digests in constant time.
- **Logging**: `BasisServerSideLogging` hooks the BNL sinks; a bounded queue and one writer
  thread batch lines into `logs/yyyy-MM-dd.log`; console lines use ANSI colours.
- Every fallible path returns `BasisResult`/`Option`; handlers log and drop malformed packets
  (the message processor counts protocol errors per peer and escalates exactly as the C# did),
  and a panicking handler is caught and counted rather than allowed to take a thread down.
