# Contrib/Handles — port diffs

C#: `Basis Server/Contrib/Handles/Common/` and `Basis Server/Contrib/Handles/Dns/` ·
Rust: `basis_server/contrib/handles_common/src/` and `basis_server/contrib/handles_dns/src/`

A handle is a human-readable name that can be proved to point at a machine-readable identity. Only
the DNS kind is implemented: a TXT record at `_nexus-handles.<name>` holding `did=<identity>`,
following ATProto's handle scheme. C# uses DnsClient 1.8.0, Rust uses hickory-resolver 0.26.1.
Neither side is wired into a server — nothing outside `Handles/Dns.Tests` references
`Basis.Contrib.Auth.Handles`, and no crate depends on `basis_handles_dns` — so both are library plus
tests today.

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `Common/Common.cs` | `handles_common/src/lib.rs` (traits, enums) | 65→~60 of 131 | ported, interfaces → traits |
| `Common/Newtypes.cs` | `handles_common/src/lib.rs:20-34` | 15→15 | ported |
| `Common/Verifier.cs` | `handles_common/src/lib.rs:94-130` | 58→37 | ported |
| `Common/IsExternalInit.cs` | — | 14→0 | n/a (C# compiler shim) |
| `Dns/Dns.cs` | `handles_dns/src/lib.rs` | 85→155 | ported + typed errors |
| `Dns/IsExternalInit.cs` | — | 14→0 | n/a |
| `Common.csproj`, `Dns.csproj`, `*.asmdef`, `package.json` | two `Cargo.toml` | — | n/a |
| `Dns.Tests/DnsTests.cs` | `handles_dns/tests/dns_tests.rs` | 49→24 | ported, still network-dependent |
| — | `handles_dns/tests/dns_errors.rs` | 0→57 | new (network-free negative tests) |

The three `Common/*.cs` files collapse into one `lib.rs`; the totals are 138 C# lines (excluding the
shim) against 131 Rust.

## Deviations

### 1. A TXT record without `=` crashes the C# verifier; the Rust treats it as a non-match

`Dns.cs:45-49` splits each TXT string on `"="` with `StringSplitOptions.RemoveEmptyEntries`, then
reads `parts[0]` at `:46` and `parts[1]` at `:47` — both *before* the `prefix != "did"` check at
`:52`. `RemoveEmptyEntries` means either side of the `=` being empty collapses the array. Measured:

| TXT string | C# `attr.Split("=", 2, RemoveEmptyEntries)` | C# result | Rust `lib.rs:65-67` |
| --- | --- | --- | --- |
| `did=did:web:x` | `[did, did:web:x]` | compares, matches | matches |
| `did=` | `[did]` | **`IndexOutOfRangeException` at `Dns.cs:47`** | prefix `did`, suffix `""` → no match, continue |
| `did` (no `=`) | `[did]` | **`IndexOutOfRangeException`** | prefix `did`, suffix `""` → no match, continue |
| `=did:web:x` | `[did:web:x]` | **`IndexOutOfRangeException`** | prefix `""` → `Err(Format)` |
| `` (empty string) | `[]` | **`IndexOutOfRangeException`** | prefix `""` → `Err(Format)` |
| `v=spf1 -all` | `[v, spf1 -all]` | `throw new Exception("dns txt record did not match expected format 2")` (`Dns.cs:55-57`) | `Err(DnsHandleError::Format)`, same message (`lib.rs:69,114`) |
| `did=a=b` | `[did, a=b]` | compares `a=b` | compares `a=b` |

The handle is supplied by the player and the TXT record is under that domain's control, so in the
C# a player can point the verifier at a record they wrote and get an `IndexOutOfRangeException` out
of the verification call. It is latent rather than live only because nothing calls the verifier
yet.

The Rust is not a strict superset either: a bare `did` with no `=` is accepted as a well-formed
record with an empty value (`unwrap_or("")` at `lib.rs:67`) rather than being called malformed. That
is more permissive than the C# author's evident intent, though it cannot make a handle verify —
`identity.v()` is never empty in practice. Pinned by nothing on either side; `dns_errors.rs` covers
lookup faults, not record shapes.

### 2. Lookup failures are swallowed by the trait path in Rust, propagated in C#

`Dns.cs:26-67` lets any DnsClient failure — and the `Exception` at `:55` — propagate out through
`Verifier.cs:55` to the caller. `handles_dns/src/lib.rs:85-89` implements the trait as
`self.handle_points_to_identity_async(...).await.unwrap_or(false)`, so a timeout, an unparsable
name, a resolver failure and a malformed record all become plain `false`.

Both fail closed (`false` means "not verified"), so this is not a spoofing hole, but the Rust
trait path cannot distinguish "this handle does not point at you" from "DNS was down". The port
keeps the information available: `handle_points_to_identity_async` (`lib.rs:43-76`) returns
`Result<bool, DnsHandleError>`, and `DnsHandleError::is_transient` (`lib.rs:125-127`) says whether a
retry could help. Pinned by `dns_errors.rs:31-45` (permanent) and `:48-57` (transient), including
`:45` which asserts the bool form answers `false`.

### 3. Only the first TXT record is inspected, on both sides

`Dns.cs:36` takes `Answers.TxtRecords().FirstOrDefault()?.Text`; `lib.rs:56-61` takes the first
`RData::TXT` in the answer section. Both then iterate every character-string inside that one record
(`Dns.cs:42` / `lib.rs:63`). If `_nexus-handles.<name>` holds several TXT records and the handle
record is not the one the resolver returns first, both implementations fail — the C# by throwing,
the Rust with `Err(Format)` folded to `false`. The port faithfully reproduces the limitation; it is
worth calling out because RRset ordering is not stable.

### 4. Record name, query type, matching rule: no deviation

`Dns.cs:18` `TXT_RECORD_PREFIX = "_nexus-handles"` and `:31-34` build `"_nexus-handles." +
handle.DisplayName` and issue a `QueryType.TXT`. `lib.rs:30` and `:48-49` build the identical name
and call `txt_lookup`. The value comparison is an exact string equality against the identity on both
sides (`Dns.cs:60` / `lib.rs:71`) — no case folding, no trimming, no trailing-dot normalisation on
either side.

### 5. Timeouts and retries: no deviation

Both take a caller-constructed client, so the effective settings are each library's defaults unless
the caller overrides them. Measured C# `new LookupClientOptions(NameServer.Cloudflare)`:
`Timeout=00:00:05`, `Retries=2`, `UseCache=True`, `UseTcpFallback=True`, `ThrowDnsErrors=False`,
`ContinueOnDnsError=True`, `Recursion=True`. hickory-resolver 0.26.1 `ResolverOpts::default()`:
`timeout` 5 s (`config.rs:666-668`), `attempts` 2, documented as "number of retries after lookup
failure" (`config.rs:508-510`), caching on, and a truncated UDP answer retried over TCP regardless
of `try_tcp_on_error` (`name_server_pool.rs:361-364`). Same 5-second budget, same two retries, same
caching, same TCP fallback for truncation.

### 6. NXDOMAIN and empty answers: no deviation, but expressed differently

C# relies on DnsClient's `ThrowDnsErrors = false` default: an NXDOMAIN comes back as a result with
no answers, `TxtRecords().FirstOrDefault()?.Text` is `null` at `Dns.cs:36-40`, and the method
returns `false`. Rust makes it explicit at `lib.rs:51-52` — `e.is_no_records_found() ||
e.is_nx_domain()` maps to `Ok(false)`, with the reasoning in the comment ("a name with no such
record does not point at anyone; that is an answer, not a fault"). Same outcome, and the Rust holds
even if a caller hands it a resolver configured to surface those as errors.

### 7. The Cloudflare test helper uses more servers than the C# test did

`DnsTests.cs:18` uses `NameServer.Cloudflare`, which is `1.1.1.1:53` and nothing else (measured).
`lib.rs:37-41` `cloudflare_client()` builds `ResolverConfig::udp_and_tcp(&CLOUDFLARE)`, which is
Cloudflare's full set (both IPv4 addresses and the IPv6 pair) over both transports. Test-helper
only; it makes `dns_tests.rs` more robust and slightly less deterministic than its C# counterpart.

### 8. Debug logging removed

`Dns.cs:44` does `Console.WriteLine(attr)` for every character-string of every TXT record it reads,
on every verification. There is no counterpart in the Rust.

### 9. TXT strings are decoded lossily in Rust

`lib.rs:64` uses `String::from_utf8_lossy`, which substitutes U+FFFD for invalid bytes instead of
failing; DnsClient hands `TxtRecord.Text` back already decoded. A TXT record containing invalid
UTF-8 therefore compares as a mangled string in Rust rather than erroring. It can only cause a
non-match, never a false match.

### 10. Interface and type mechanics

* `Common.cs:14` `Task<bool> HandlePointsToIdentity(IHandle, Identity)` becomes
  `lib.rs:43-47`, returning the `BoxFuture` alias declared at `lib.rs:38`, because an `async fn` in a
  trait is not `dyn`-safe and the verifier map stores `Box<dyn IHandleVerifier>`. `Verifier.cs:46-56`
  stays a plain `async fn` at `lib.rs:125-130`.
* `HandleProperties` is a record in C# (`Common.cs:36-40`) and a plain `Copy` struct in Rust
  (`lib.rs:67-72`); `DnsHandle` is a `readonly struct` with an `init` property (`Dns.cs:71-84`) and
  an owned-`String` struct in Rust (`lib.rs:130-143`). `PROPERTIES` and `KIND` go from
  `static readonly` (`Dns.cs:73-78`) to `const` (`lib.rs:137-142`).
* `Common/Newtypes.cs:14` `Identity` record becomes the newtype at `lib.rs:23-33`, keeping the
  `.v()` accessor named after the C# `V` property.
* `Config.Verifiers` (`Verifier.cs:15`) is a `Dictionary<HandleKind, IHandleVerifier>`;
  `lib.rs:96-97` is a `HashMap<HandleKind, Box<dyn IHandleVerifier>>`. An unknown kind returns
  `false` on both sides (`Verifier.cs:51-54` / `lib.rs:126-128`).

## Corners cut

* Only the DNS kind exists. `Local`, `HttpWellKnown` and `Steam` are enum variants and TODOs on both
  sides (`Common.cs:58-64` / `lib.rs:86-92`), and `Contrib/Handles/README.md:8-20` describes Local
  and HTTPS Well-Known as part of the design. The port neither adds nor removes any of that.
* `HandleMutability` (`Common.cs:44-55` / `lib.rs:75-83`) is carried through on both sides and never
  read by any code.
* `handles_common` has no test file at all. The only exercise `HandleVerifier` gets is
  `dns_tests.rs:17-23`, which needs outbound DNS. The unknown-kind branch at `lib.rs:126-128` is
  untested.
* `dns_tests.rs` is still an integration test against live DNS and a live `example.socialvr.net`
  record, exactly as `DnsTests.cs:31-46` was; its own doc comment says so (`dns_tests.rs:1`). Nothing
  was added to let the happy path be tested offline — `dns_errors.rs` covers only failure modes.
* Neither side validates DNSSEC (hickory's `validate` defaults to false; DnsClient does not
  validate), so the whole scheme rests on the resolver being trusted. The security note about
  needing a bidirectional `handle <-> identity` mapping (`Verifier.cs:30-45`, carried over verbatim
  to `lib.rs:112-124`) is the only defence documented, and it remains the caller's job on both
  sides.
* The first-TXT-record-only rule of deviation 3 was reproduced rather than fixed.

## Improvements

* No `IndexOutOfRangeException`: the Rust's `splitn` plus `unwrap_or("")` (`lib.rs:65-67`) cannot
  panic on any record shape (deviation 1).
* `DnsHandleError` (`lib.rs:101-128`) replaces the `TODO: introduce custom exception type` at
  `Dns.cs:54` with a real type, and splits faults into transient and permanent
  (`lib.rs:118-127`: timeout, busy, no connections and I/O are retryable; an unparsable name or a
  resolver that will not build is not). A caller can now retry a timeout and reject a bad handle,
  which the C# gave no way to distinguish.
* `handle_points_to_identity_async` (`lib.rs:43`) exists alongside the bool-returning trait method,
  so callers who want the reason can have it.
* NXDOMAIN and no-records handling is explicit rather than a consequence of the DNS client's default
  configuration (deviation 6).
* `tests/dns_errors.rs` is new: 57 network-free lines that stand up a resolver pointed at TEST-NET-1
  (`dns_errors.rs:15-28`) and assert that an unparsable name is a permanent lookup fault, that a name
  server which never answers is a transient one, that the error carries a source, and that the trait
  form degrades to `false` instead of failing.
* The per-record `Console.WriteLine` is gone (deviation 8).
* `cloudflare_client()` gives the tests a named constructor instead of open-coding resolver options.

## Verdict

The record format, the query, the timeout budget and the matching rule are the same on both sides:
a TXT lookup of `_nexus-handles.<display name>`, first TXT record only, each character-string parsed
as `did=<identity>` and compared for exact equality, with a 5-second timeout and two retries. A
domain whose TXT record satisfies one implementation satisfies the other.

Two real behavioural differences. The C# throws `IndexOutOfRangeException` on any TXT string that
lacks a non-empty value after `=` — reachable by whoever controls the domain a player names, and
fixed by the port. And the Rust trait method turns every lookup fault into `false`, where the C#
propagated the exception; that is fail-closed and the typed error is still reachable through
`handle_points_to_identity_async`, but a caller using the trait cannot tell a DNS outage from a
negative answer. The port also drops per-lookup stdout logging and adds the first offline tests this
module has had. Both modules remain unused by either server, so none of this is live yet.
