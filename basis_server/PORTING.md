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

The `litenetlib` stack id, its config sidecar and its connection-string parser are kept: the
API-compatible LiteNetLib-protocol transport planned for the C# clients registers under it.

## Status

- [x] contrib: crypto, did, handles (+ tests)
- [x] core: io, protocol, diagnostics, identity, pooling, math, p2p, encryption, statistics,
      sanitization, compute, compression, serializable, configuration, transport (iroh)
- [ ] core: tests ported
- [ ] compute
- [ ] server
- [ ] client, hello world (Rust)
- [ ] console
- [ ] headless client console, bench agent
- [ ] iroh FFI + C# hello world clients
- [ ] server tests, REST API tests
