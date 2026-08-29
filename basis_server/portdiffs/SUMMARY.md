# Port audit — what 37 module comparisons found

Every C# file in `Basis Server/` was read against its Rust counterpart in `basis_server/`, module
by module. The per-module detail is in `<module>portdiffs.md`; this is what matters across all of
them, worst first.

## The headline: the wire is intact, with one exception

Five independent checks of the wire contract all came back clean:

| checked | result |
|---|---|
| 115 protocol constants (62 channel numbers, magics, `ServerVersion = 54`) | identical, name by name |
| every `Serialize`/`Deserialize` pair, field order, width, length prefix | identical |
| avatar bit packing, quantisation tables, packet sizes 74/83/97/159 | identical |
| zstd bundle dictionary | byte-identical (sha256 verified) |
| the LiteNetLib protocol port — header bits, handshake, 17-byte ack bitfield, fragmentation, `Merged` and `CompactMerged` | identical |
| Ed25519 / X25519 / did:key encodings | byte-identical for every canonical input |

**The exception, and the most serious finding in the audit: the server-statistics blob changed
from Brotli to raw DEFLATE.** The shipping Unity client decodes it as Brotli
(`BasisNetworkEvents.cs:737`), so that channel is broken against a real client, not merely
different between the two servers. Nothing in either test suite covers it because neither suite
contains the Unity client. This needs fixing before any deployment that serves statistics.

## Bugs the audit found in the C# server

The port is faithful, so several of these are live in production today:

1. **`NetDataReader` bounds-checks against the wrong length** — every multi-byte read and every
   `Peek*` checks `_data.Length` (the pooled buffer) rather than `_dataSize` (the packet). A
   truncated packet therefore reads stale bytes from a previously received packet. There is a
   reachable path: `BasisServerEventsRouter.cs:84` reads a `ushort` with no length check and
   broadcasts it to every client at `:94`. The Rust checks the packet length and is pinned by
   `io_errors.rs:23-50`.
2. **Unhandled exceptions in the live auth path** — `did:key:` with an empty body, an overflowing
   multicodec varint, and invalid base58 each throw out of `VerifyResponse`. The Rust returns
   typed errors.
3. **`IndexOutOfRangeException` in DNS handle resolution** — any TXT string without a non-empty
   value after `=` (`Dns.cs:47`, evaluated before the prefix check).
4. **`NetworkClient.Shutdown` never clears `IsInUse`** (`NetworkClient.cs:74-77`), wedging the
   object permanently.
5. **The ownership table is unbounded and client-keyed** — the Rust now caps it per player.

## Regressions and gaps in the Rust, by priority

| # | finding | module |
|---|---|---|
| 1 | Statistics blob Brotli → DEFLATE: breaks the shipping Unity client | statistics, core |
| 2 | GPU compute path not ported, **and** the fallback inverted: the factory now almost always returns a solver, which switches the distance sweep to the 32-tick interval while being single-threaded, displacing a SIMD+rayon path that ran at 125 ticks | computecrate |
| 3 | Send-socket count never fed back, so the send-worker ceiling sits at `min(cores, 8)` — 2-4× lower than the C# on a large host | reduction |
| 4 | The per-peer protocol-error budget is effectively unreachable: the C# counted thrown exceptions, the Rust handlers return early instead, so the 500-error kick never fires and a hostile client is never dropped | messaging, handlers, p2p |
| 5 | String settings are whitespace-trimmed on load and rewritten that way; the C# preserves them, so a padded password means different things to the two servers | configuration |
| 6 | The two load clients default to different transports and the Rust one has no Opus encoder, so benchmark runs are not comparing like with like | clientconsole |
| 7 | P2P broker keys its "already initialised" check on a raw `Arc::as_ptr` without holding the `Arc`; a restart reusing the allocation would skip the session reset | p2pserver |
| 8 | `PlayerIdentity.properties` is case-sensitive under a doc comment claiming otherwise (latent — nothing populates it yet) | identity |
| 9 | REST API buffers the request body before checking the API key | restapi |
| 10 | Voice `AudioSegmentDataMessage` is no longer pooled on the hot path | pooling |
| 11 | `sample_utilization` is Unix-only, so the pool-widening gate never trips on other platforms | protocol |
| 12 | `ContentShareType::from_byte` coerces an unknown type byte to `Avatar`, losing the C#'s rejection | serializable, networking |

## Fixed while writing this audit

* `switch_ownership` reported success even when the new per-player cap refused the implicit
  acquire, so the room would have been told about ownership the table did not record. The cap
  turned a branch the C# could only reach on a lost race into one a client can reach.
* `BasisSetupWizard::looks_like_did` sliced `value[..4]` on operator input and panicked on any
  non-ASCII entry, aborting first boot.
* The LiteNetLib logic thread had no panic guard where the C# `UpdateLogic` caught and logged; one
  peer's panic would have stopped acks, resends, pings and timeouts for every peer with the
  process still running.
* `register_for_tests` — which installs an authenticated entry without the challenge round trip —
  was a plain `pub fn` in the production crate. It is now behind the `test-seams` feature and is
  verifiably absent from `cargo build --release -p basis_network_console`.

## What the audit did not cover

Deliberate omissions are recorded per module with reasons: LiteNetLib's NAT punch module, the
CRC/XOR packet layers, NTP, the latency/loss simulation and native sockets; the ILGPU GPU
backend; Unity-only code. Neither tree tests the CPU-budget ceiling-discovery state machine, the
`gc` block of the health document, or the per-peer error counters.
