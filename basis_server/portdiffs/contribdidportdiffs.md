# Contrib/Auth/Did — port diffs

C#: `Basis Server/Contrib/Auth/Did/` · Rust: `basis_server/contrib/did/src/`

The `did:key` method plus the challenge/response authentication built on it. C# uses SimpleBase
4.0.2 for base58-btc, the `VarInt` 1.2.2 package (`WojciechMikołajewicz.Base128`) for the
multicodec varint and Newtonsoft.Json for the JWK; Rust uses bs58 0.5, unsigned-varint 0.8, base64
0.22 and serde_json. Both are live: `BasisNetworkServer/Security/BasisDIDAuthIdentity.cs:50-51`
constructs a `DidAuthentication` and feeds it client-supplied DIDs, and `basis_network_server`
depends on `basis_did`.

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `DidKeyResolver.cs` | `did_key_resolver.rs` | 154→108 | ported + typed errors |
| `DidAuth.cs` | `did_auth.rs` | 246→222 | ported, async dropped |
| `Base64UrlSafe.cs` | `base64_url_safe.rs` | 42→41 | ported + typed errors |
| `JsonWebKey.cs` | `json_web_key.rs` | 80→58 | ported |
| `Newtypes.cs` | `newtypes.rs` | 28→49 | ported |
| `IDidMethod.cs` | `i_did_method.rs` | 33→27 | ported, `Task` dropped |
| `DidDocument.cs` | `did_document.rs` | 17→17 | ported |
| `Result.cs` | — | 45→0 | n/a (std `Result`) |
| `IsExternalInit.cs` | — | 14→0 | n/a (C# compiler shim) |
| — | `lib.rs` | 0→35 | new (module tree and re-exports) |
| `../Did.Tests/DidKeyTests.cs` | `tests/did_key_tests.rs` | 74→44 | ported, same w3c vector |
| `../Did.Tests/ServerExample.cs` | `tests/server_example.rs` | 158→131 | ported |
| `../Did.Tests/Base64UrlSafeTests.cs` | `tests/base64_url_safe_tests.rs` | 33→22 | ported |
| — | `tests/did_auth_errors.rs` | 0→144 | new (negative tests for the whole auth path) |

## Deviations

### 1. Non-minimal multicodec varints: C# accepts them, Rust rejects them

`DidKeyResolver.cs:59-67` decodes the multicodec prefix with `Base128.TryReadUInt16`, which is a
plain LEB128 reader with no minimality check. `did_key_resolver.rs:36-37` uses
`unsigned_varint::decode::u16`, which returns `Error::NotMinimal` when a multi-byte varint ends in a
zero byte (unsigned-varint 0.8.0 `decode.rs:66-86`), and the port maps every varint failure to
`DidKeyDecodeError::VarintWouldOverflow`.

Measured, resolving the same public key behind four different varint prefixes:

| multicodec prefix bytes | C# `ResolveDocument` | Rust `DidKeyResolver::resolve` |
| --- | --- | --- |
| `ED 01` (canonical) | resolves, fragment `z6MkiTBz1ymuep…` | resolves, same fragment, same `x` |
| `ED 81 00` | **resolves**, fragment `zQhVUWQ75Gmgfe…`, same `x` | `VarintWouldOverflow` |
| `ED 81 80 00` | **resolves**, fragment `z2obmZwJnSGZQY…`, same `x` | `VarintWouldOverflow` |
| `ED 80 80 00` | `UnsupportedPubkeyType` (decodes to `0x6D`) | `VarintWouldOverflow` |

So on the C# side three distinct DID strings denote the same Ed25519 public key. That matters
beyond parsing: `BasisDIDAuthIdentity` keys its admin list and its per-DID counters on the DID
string, and `ServerExample.cs:34` bans by DID string, so the aliases are separate identities to
every bookkeeping structure while being one key to the signature check. The Rust behaviour matches
the multiformats unsigned-varint spec and the `did:key` spec's canonical form.

Interop consequence: a non-canonical `did:key` authenticates against a C# server and is refused by a
Rust one. Nothing in either tree *produces* such a DID — `EncodePubkeyAsDid`
(`DidKeyResolver.cs:90-111`) and `encode_pubkey_as_did` (`did_key_resolver.rs:54-63`) both emit
`ED 01`, verified against the same w3c vector on both sides — so it is only reachable if a client
supplies one. No test on either side pins it.

### 2. Three client-triggerable unhandled exceptions in C#, typed errors in Rust

`DidAuth.cs:196-220` (`ResolveDid`) does not wrap the resolver call at `:219` in a `try`, and
`DidKeyResolver.ResolveDocument` runs `Helper` synchronously before `Task.FromResult`, so anything
thrown there escapes `VerifyResponse`. Measured through the full `VerifyResponse` /
`verify_response` path:

| DID | C# | Rust |
| --- | --- | --- |
| `did:key:` | **throws `IndexOutOfRangeException`** (`DidKeyResolver.cs:48`, `multibasePart[0]` on a zero-length split result) | `Err(Resolve(Other))` |
| `did:key:z<base58 of 80 80 80 01 …>` | **throws `OverflowException`** (`DidKeyResolver.cs:59`; `Base128.TryReadUInt16` documents `OverflowException` for values too big for `UInt16`, and returns `false` only for truncated input) | `Err(Resolve(Other))` |
| `did:key:z0OIl` | **throws `ArgumentException: Invalid character: 0`** (`DidKeyResolver.cs:57`, SimpleBase) | `Err(Resolve(Other))` |
| any well-formed but unsupported `did:key` | throws `DidKeyDecodeException` (`DidKeyResolver.cs:53,66,71,78`) | `Err(Resolve(Other))` |

The Rust equivalents are `did_key_resolver.rs:26-32` (empty method-specific id → `NotBase58Btc`),
`:33-35` (bs58 failure → `NotBase58Btc`), `:36-37` (varint failure → `VarintWouldOverflow`), and
`:76-79`, which maps any `DidKeyDecodeException` to `DidResolveErr::Other`.

Because the DID arrives from the client, each of these is a remote crash in the C# auth path. The
Rust behaviour is pinned by `did_auth_errors.rs:53-65`
(`a_garbled_did_key_is_a_resolve_error_not_a_panic`, which includes `"did:key:"` and
`"did:key:!!!not base58!!!"`) and `:67-87`. Nothing pins it on the C# side.

A side effect: `DidResolveErr.E.Other` (`DidAuth.cs:38`) is unreachable in C# — the only resolve
errors it can return are `InvalidPrefix` and `UnsupportedMethod` — whereas in Rust it is the *only*
error `did:key` resolution produces.

### 3. `Base64UrlSafe::decode` rejects non-canonical trailing bits that C# accepts

`Base64UrlSafe.cs:39` calls `Convert.FromBase64String`, which does not require the unused low bits
of the last symbol to be zero. `base64_url_safe.rs:29-31` uses `engine::general_purpose::STANDARD`,
whose `decode_allow_trailing_bits` is false. Measured:

| input | C# | Rust |
| --- | --- | --- |
| `3q2-7w` | `DEADBEEF` | `Ok("DEADBEEF")` |
| `QQ` | `41` | `Ok("41")` |
| `QR` | `41` | `Err(Invalid("Invalid last symbol 82, offset 1."))` |
| `3q2 -7w` (embedded space) | `FormatException` | `Err(Invalid("Invalid symbol 32, offset 3."))` |
| `A` (length ≡ 1 mod 4) | `FormatException` | `Err(InvalidLength)` |
| `3q2-7=` (padding inside) | `FormatException` | `Err(Invalid(…))` |

The padding-from-length logic (`Base64UrlSafe.cs:25-37` / `base64_url_safe.rs:23-28`) is identical,
including the `FormatException` / `InvalidLength` for a length of 1 mod 4. Only the trailing-bits
row differs. Not reachable through `did:key` resolution, which generates `x` itself
(`DidKeyResolver.cs:114-124` / `did_key_resolver.rs:65-73`); reachable through an externally
supplied JWK. Both sides' tests pin only the `3q2-7w` case
(`Base64UrlSafeTests.cs:21-31` / `base64_url_safe_tests.rs:14-22`).

One further nuance: `base64_url_safe.rs:23` measures `base64.len()` in UTF-8 bytes where
`Base64UrlSafe.cs:25` measures UTF-16 code units. For non-ASCII input the two can land in different
`% 4` buckets, but both reject such input either way.

### 4. A malformed JWK is an exception in C#, an empty key in Rust

`JsonWebKey.cs:65-73` (`DecodePubkey` / `DecodePrivkey`) lets `Base64UrlSafe.Decode` throw
`FormatException` up through `DidAuth.cs:161`. `json_web_key.rs:47-53` does
`Base64UrlSafe::decode(...).unwrap_or_default()`, producing an empty `PubKey`, which makes
`Ed25519::verify` return `false` and the caller see `DidSignatureErr::InvalidSignature`. Fail-closed
in both cases, but the Rust discards the reason. Likewise `JsonWebKey.Deserialize`
(`JsonWebKey.cs:50-53`) throws on malformed JSON where `json_web_key.rs:35-37` returns `None`.

### 5. Async dropped

`IDidMethod.cs:22` returns `Task<DidDocument>` and `DidAuth.cs:119` is `async`;
`i_did_method.rs:17` and `did_auth.rs:134` are synchronous, with the reasoning recorded at
`i_did_method.rs:15-16` — `did:key` needs no I/O, and a future `did:web` can resolve on a blocking
helper thread rather than making every caller async. Behaviourally identical today; a real change
in the trait's shape for anyone implementing a new method.

### 6. The RNG type no longer has to be cryptographic

`DidAuth.cs:17` types the challenge RNG as `System.Security.Cryptography.RandomNumberGenerator`, so
the compiler guarantees a CSPRNG. `did_auth.rs:16` types it as `Box<dyn Rng + Send>` and
`did_auth.rs:35` accepts `impl Rng + Send + 'static`, which admits any RNG, `SmallRng` included.
The default is sound (`did_auth.rs:25-29`: `StdRng`, ChaCha12, seeded from `rand::rng()`), and the
nonce length is unchanged at 32 bytes (`DidAuth.cs:89` / `did_auth.rs:105`, pinned by
`did_auth_errors.rs:27`). But the invariant that used to be enforced by the type is now a
convention. Adding `+ CryptoRng` to both bounds would restore it.

### 7. `DidFragmentErr` doc comments were swapped in C#; the port fixes them

`DidAuth.cs:55-62` documents `AmbiguousFragment` as "No such fragment was present in the DID
document" and `NoSuchFragment` as "The given fragment was ambiguous" — the two are transposed.
`did_auth.rs:53-59` keeps the same variants in the same order with the comments corrected. No
runtime difference; `AmbiguousFragment` is unreachable on both sides (`DidAuth.cs:180-194` /
`did_auth.rs:162-173` only ever return `NoSuchFragment`).

### 8. Smaller shape differences

* `Result.cs` has no counterpart; `lib.rs:4` records the decision. The C# `GetOk`
  (`Result.cs:21`) threw on a legitimately-null `Ok` value — a trap that disappears with the type.
* `IDidVerifyErr` was an empty marker interface (`DidAuth.cs:80`) that forced callers to downcast;
  `did_auth.rs:68-92` makes it a closed enum with `From` impls, so `verify_response` composes with
  `?`.
* `DidDocument.Pubkeys` is a `ReadOnlyDictionary` with `[UnorderedEquality]`
  (`DidDocument.cs:13-16`); `did_document.rs:9-11` is a bare `pub` `HashMap`, mutable by anyone
  holding the document.
* Prefix handling is equally lenient on both sides and was checked: `DidKeyResolver.cs:41-46`
  (split on `"did:key:"` with `RemoveEmptyEntries`) and `did_key_resolver.rs:26`
  (`strip_prefix(...).unwrap_or(&did.0)`) both resolve a bare `z…` string with no prefix at all,
  and both reject `foo:did:key:z…` with `NotBase58Btc`. The gate is in `ResolveDid`
  (`DidAuth.cs:198-206`) / `resolve_did` (`did_auth.rs:176-188`), which agree exactly: three
  colon-separated segments, first `did`, second `key`, and `did:key:` itself passes that gate on
  both sides.

## Corners cut

* Only `did:key`, and within it only Ed25519 — same on both sides (`IDidMethod.cs:31`,
  `DidKeyResolver.cs:26` / `i_did_method.rs:26`, `did_key_resolver.rs:20`). `did:web` is still a
  TODO in both.
* `did_key_resolver.rs:78` collapses all four `DidKeyDecodeError` values into
  `DidResolveErr::Other`, so a caller on the `verify_response` path cannot distinguish "unsupported
  key type" from "not base58". The specific error is only available by calling
  `DidKeyResolver::resolve` directly. The C# had the same information loss for a different reason —
  it threw instead of returning.
* `retrieve_key` returns the single key regardless of the requested fragment when the document has
  exactly one (`DidAuth.cs:185-188` / `did_auth.rs:166-168`). Inherited, and now pinned as intended
  behaviour by `did_auth_errors.rs:133-144`, which asserts that a response naming `"no-such-key"`
  still authenticates. Harmless for `did:key`, which always yields one key, but it is a rule that
  will need revisiting for multi-key methods.
* `make_challenge` takes a `Mutex` around the RNG on every call (`did_auth.rs:116-119`); the C#
  `RandomNumberGenerator` is thread-safe without one. Poisoning is handled
  (`unwrap_or_else(|p| p.into_inner())`), so it cannot panic, but it is a contention point the
  original did not have.
* Nothing on either side pins the non-minimal varint behaviour of deviation 1, and nothing pins the
  base64 trailing-bits behaviour of deviation 3.

## Improvements

* No unhandled exception can escape the auth path (deviation 2), and the tests say so:
  `did_auth_errors.rs:36-65` walks bad prefixes, unsupported methods and five garbled `did:key`
  strings and asserts a typed error for each.
* Non-minimal multicodec varints are rejected (deviation 1), which is both spec-correct and removes
  the DID-aliasing surface described there.
* A base58 decode failure is a `DidKeyDecodeError::NotBase58Btc` (`did_key_resolver.rs:33-35`)
  instead of a SimpleBase `ArgumentException` escaping the resolver.
* `tests/did_auth_errors.rs` is new: 144 lines covering the honest flow, nonce freshness, prefix and
  method errors, garbled `did:key` strings, per-error-kind resolver assertions, wrong-nonce and
  wrong-key signatures, replay against a fresh challenge, malformed signatures of length 0/63/65/64,
  and the single-key fragment rule. The C# tests covered the happy path only.
* `IDidVerifyErr` became a real sum type (deviation 8).
* The transposed `DidFragmentErr` doc comments are fixed (deviation 7).

## Verdict

For every DID either implementation produces, the two are interoperable: `encode_pubkey_as_did`
emits byte-identical strings, and the w3c `did:key` vector
`did:key:z6MkiTBz1ymuepAQ4HEHYSF1H8quG5GLVVQR3djdX3mDooWp` resolves to the same JWK on both sides
(pinned by `DidKeyTests.cs:32-54` and `did_key_tests.rs:18-32`). The challenge/response flow —
32-byte nonce, signature over the raw nonce bytes, Ed25519 via the shared crypto layer — is
unchanged, so a C# client authenticates against a Rust server and the reverse.

The accepted sets are not identical at the edges, in both directions. The C# accepts non-canonical
multicodec varints that the Rust rejects (deviation 1), and it accepts base64 with non-canonical
trailing bits that the Rust rejects (deviation 3); the Rust accepts non-canonically encoded Ed25519
public keys that the C# rejects (see the crypto report, deviation 1). None of these is reachable
with a DID or key that either implementation generates, and in each case the stricter side is the
one that is right. The port's real gain is deviation 2: three remote crashes in a client-facing auth
path became typed errors, with tests. The one thing that got weaker is the RNG bound in deviation 6.
