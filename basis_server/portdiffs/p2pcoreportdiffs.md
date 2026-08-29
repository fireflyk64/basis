# P2P core — port diffs

C#: `Basis Server/BasisNetworkCore/P2P/` · Rust: `basis_server/basis_network_core/src/p2p/`

Scope is the core P2P types only: the link-health decision function and the introducer
interface. The server-side broker (`BasisNetworkServer/P2P/`) is a separate module.

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `P2P/BasisP2PLinkHealth.cs` | `p2p/basis_p2p_link_health.rs` | 73 → 61 | faithful |
| `P2P/IPeerIntroducer.cs` | `p2p/i_peer_introducer.rs` | 14 → 22 | extended |
| — | `p2p/mod.rs` | — → 5 | Rust-only module glue |

## Deviations

**1. `IPeerIntroducer.Introduce` takes two structs instead of four endpoints.**
C# `IPeerIntroducer.cs:8-10` is
`Introduce(IPEndPoint aInternal, IPEndPoint aExternal, IPEndPoint bInternal, IPEndPoint bExternal, string token)` —
four endpoints, positionally paired. Rust `i_peer_introducer.rs:19` is
`introduce(&self, a: &PeerIntroduction, b: &PeerIntroduction, token: &str)`, where
`PeerIntroduction` (`i_peer_introducer.rs:9-15`) carries `internal: Option<SocketAddr>`,
`external: Option<SocketAddr>` and a third field the C# has no equivalent for,
`iroh_addr: Vec<u8>` — the serialized iroh `EndpointAddr`.

Why: the C# introducer forwarded to LiteNetLib's `NatPunchModule.NatIntroduce`
(`BasisNetworkServer/P2P/LNLPeerIntroducer.cs:20`), which wants exactly those four
endpoints. The Rust transport is iroh, whose endpoints hole-punch themselves once each side
has the other's `EndpointAddr` (`basis_network_server/src/p2p/iroh_peer_introducer.rs:1-3`,
`:20-22`). The struct is the union of both transports' needs so one trait serves either. The
two `Option<SocketAddr>` fields also make "this peer reported no internal address" explicit,
where the C# passed a null `IPEndPoint`.

No test pins the introducer trait on either side; it has one implementation each
(`LNLPeerIntroducer.cs` / `iroh_peer_introducer.rs`) and no round-trip test.

**2. `Send + Sync` bound added.** Rust `i_peer_introducer.rs:17` requires
`IPeerIntroducer: Send + Sync`; the C# interface (`IPeerIntroducer.cs:5`) has no
thread-safety marker and relies on convention. Not observable behaviour, but it is a
constraint the C# did not impose. Not pinned by a test.

**3. Nothing else.** `BasisP2PLinkHealth` is line-for-line identical in ordering and
comparison operators:

| check | C# | Rust |
| --- | --- | --- |
| grace window (`<`, returns Healthy) | `BasisP2PLinkHealth.cs:51-52` | `basis_p2p_link_health.rs:41-43` |
| stale (`>`, DemoteStale) | `:54-55` | `:44-46` |
| unconfirmed (`!confirmed && >`) | `:57-58` | `:47-49` |
| dwell + flap (`>` and flag) | `:60-61` | `:50-52` |
| fallthrough Healthy | `:63` | `:53` |
| `PunchStalled` (`>`) | `:70-71` | `:58-60` |

`long` → `i64` throughout: same width, same signedness. `ConnectedVerdict` keeps the same
four variants in the same order, and the C#'s `: byte` backing becomes `#[repr(u8)]`
(`BasisP2PLinkHealth.cs:16` / `basis_p2p_link_health.rs:13-14`).

The tests are a 1:1 port: 12 C# test methods in
`Basis Server/BasisServerTests/Networking/P2PLinkHealthTests.cs` against 12 Rust tests in
`basis_server/basis_server_tests/tests/networking/p2p_link_health_tests.rs`, with the same
threshold constants (Grace 2500, Stale 1500, Confirm 4000, Dwell 30000, Punch 6000 —
`P2PLinkHealthTests.cs:17-21` / `p2p_link_health_tests.rs:8-11`) and the same boundary cases,
including the exclusive grace boundary and the stale-beats-unconfirmed priority.

## Corners cut

None in the link-health logic — it is complete, including the doc comment explaining the
recover-after-rejoin symptom (`BasisP2PLinkHealth.cs:3-13` / `basis_p2p_link_health.rs:1-9`).

The only trimming is documentation: the C#'s per-parameter `<param>` docs on
`EvaluateConnected` (`BasisP2PLinkHealth.cs:35-38`) are condensed to a two-line comment
(`basis_p2p_link_health.rs:26-27`). The parameter names carry the same information, so
nothing is lost that a reader needs, but the "a live peer P2P-broadcasts its avatar many
times a second even when standing still" rationale for the stale threshold
(`BasisP2PLinkHealth.cs:30-32`) is gone from the Rust.

## Improvements

* `i_peer_introducer.rs:11-12` — `Option<SocketAddr>` makes a missing endpoint a type-level
  state. The C# passed `IPEndPoint` references that could be null with no signal
  (`IPeerIntroducer.cs:8-10`), and `LNLPeerIntroducer.Introduce` forwards them straight into
  LiteNetLib without a null check (`LNLPeerIntroducer.cs:17-21`).
* `i_peer_introducer.rs:17` — the `Send + Sync` bound documents and enforces what the C#
  assumed: the introducer is called from the server's event threads.

## Verdict

`BasisP2PLinkHealth` is a faithful port: same four checks, same order, same comparison
operators, same tests with the same constants. `IPeerIntroducer` is deliberately reshaped for
a different transport — four positional endpoints became two structs that also carry an iroh
address — which is an extension rather than a loss, since the LiteNetLib fields survive as
`Option`s. Neither the C# nor the Rust introducer trait is pinned by a test, so that
signature change rests on the two implementations agreeing rather than on a test.
