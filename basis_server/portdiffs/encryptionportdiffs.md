# Encryption — port diffs

C#: `Basis Server/BasisNetworkCore/Encryption/` · Rust: `basis_server/basis_network_core/src/encryption/`

Security-relevant module. The primitives it calls live one level down — C#
`Basis Server/Contrib/Crypto/` (BouncyCastle / `System.Security.Cryptography`) and Rust
`basis_server/contrib/crypto/` (RustCrypto + dalek) — and are covered here where the choice
of algorithm or size matters.

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `Encryption/BasisCryptoHandshake.cs` | `encryption/basis_crypto_handshake.rs` | 88 → 71 | faithful (hardened) |
| `Encryption/BasisCryptoLayer.cs` | `encryption/basis_crypto_layer.rs` | 199 → 215 | faithful (hardened) |
| — | `encryption/mod.rs` | — → 5 | Rust-only module glue |

### Cryptographic parameters — traced and identical

| parameter | C# | Rust |
| --- | --- | --- |
| KEX | X25519 (`BasisX25519.cs:41-44`, BouncyCastle `X25519Agreement`) | X25519 (`basis_x25519.rs:40`, `x25519_dalek`) |
| X25519 key / shared secret | 32 / 32 B (`BasisX25519.cs:13-14`) | 32 / 32 B (`basis_x25519.rs:19-20`) |
| KDF | HKDF-SHA256, RFC 5869 (`BasisHkdf.cs:16-19`) | HKDF-SHA256 (`basis_hkdf.rs:23`, `hkdf` + `sha2`) |
| HKDF salt | `aPub \|\| bPub`, lower public key first (`BasisCryptoHandshake.cs:43-47`) | same (`basis_crypto_handshake.rs:60-65`) |
| HKDF info | `"basis-crypto-v1-ab"` / `"basis-crypto-v1-ba"`, ASCII (`BasisCryptoHandshake.cs:20-21`) | byte-identical literals (`basis_crypto_handshake.rs:29-30`) |
| AEAD | ChaCha20-Poly1305 (`BasisAeadCipher.cs:43-53`) | ChaCha20-Poly1305 (`basis_aead_cipher.rs:11`, `chacha20poly1305`) |
| AEAD key / nonce / tag | 32 / 12 / 16 B (`BasisAeadCipher.cs:16-18`) | 32 / 12 / 16 B (`basis_aead_cipher.rs:32-34`) |
| derived key length | `BasisAeadCipher.KeySize` = 32 (`BasisCryptoHandshake.cs:18`, `:48-49`) | `BasisAeadCipher::KEY_SIZE` = 32 (`basis_crypto_handshake.rs:27`, `:66-67`) |
| AAD | the 1 cleartext LiteNetLib header byte (`BasisCryptoLayer.cs:116`) | same (`basis_crypto_layer.rs:131`) |
| datagram overhead | 16 + 8 = 24 B (`BasisCryptoLayer.cs:28-29`) | 16 + 8 = 24 B (`basis_crypto_layer.rs:43-44`) |

No key is shortened, no algorithm is downgraded, no parameter is weakened.

## Deviations

**1. Nonce construction and counter handling are identical — verified, no reuse introduced.**
This was the thing most worth getting wrong, so it is traced in full.

* Counter increment. C# `BasisCryptoLayer.cs:111`:
  `long counter = Interlocked.Increment(ref session.SendCounter);` — the *post*-increment
  value, so the first packet under a session installed with `initialSendCounter = 0` uses
  counter 1. Rust `basis_crypto_layer.rs:118`:
  `let counter = session.send_counter.fetch_add(1, Ordering::SeqCst) + 1;` — `fetch_add`
  returns the *pre*-increment value, and the explicit `+ 1` makes it the post-increment value.
  Same first counter, same sequence, both atomic, both unique per session.
* Nonce bytes. C# `WriteCounter` (`BasisCryptoLayer.cs:159-171`) clears all 12 bytes and
  writes the counter little-endian into bytes 0..8, leaving 8..12 zero. Rust `write_counter`
  (`basis_crypto_layer.rs:191-198`) starts from `[0u8; 12]` and zips in
  `(counter as u64).to_le_bytes()`, which writes exactly bytes 0..8 and leaves 8..12 zero.
  Byte-for-byte the same nonce for the same counter.
* Wire trailer. C# `WriteCounterBytes` (`:173-184`) / `ReadCounterBytes` (`:186-197`) are
  hand-unrolled little-endian; Rust (`:201-213`) uses `to_le_bytes` / `from_le_bytes`. Same
  bytes. `wire_format_layer_output_opens_with_raw_aead_cipher` and
  `wire_format_raw_aead_constructed_datagram_accepted_by_inbound`
  (`crypto_handshake_and_layer_tests.rs:322`, `:343`) pin the layout against a raw cipher on
  both sides, and `outbound_trailer_is_little_endian_send_counter` (`:261`, C#
  `CryptoHandshakeAndLayerTests.cs:334`) pins the endianness.
* Reinstall hazard is preserved, not fixed. Re-installing the same keys with the default
  counter restarts the nonce sequence and reuses (key, nonce). Both sides document it as the
  caller's obligation (C# `:73-77`, Rust `:66-68`) and both pin the hazard with a test named
  for it: `Reinstall_SameKeys_DefaultCounter_ReusesNonces`
  (`CryptoHandshakeAndLayerTests.cs:730`) / `reinstall_same_keys_default_counter_reuses_nonces`
  (`crypto_handshake_and_layer_tests.rs:602`). The port neither introduces nor closes it.

**2. Handshake validates key lengths up front; the C# relied on the primitive throwing.**
C# `DerivePeerKeys` (`BasisCryptoHandshake.cs:28-67`) does no length checking; it wraps the
whole body in `try/catch` (`:37`, `:63-66`) and returns `false` on any exception. Rust
(`basis_crypto_handshake.rs:44-53`) checks all three key lengths against 32 before doing
anything and returns `HandshakeError::KeyLength`.

The observable difference is one case: an **oversized `myPublic`** with a valid private and
peer public key. The C# would compare successfully (`Compare` at `:77-86` falls through to a
length tiebreak), agree successfully, and build the HKDF salt from the oversized key
(`:47`) — returning `true` with keys derived under a salt the peer cannot reproduce. The
Rust refuses. Both ends would in practice have failed to communicate, so this is a
fail-loud-instead-of-fail-silent change, not a break.

Pinned indirectly: `handshake_refuses_bad_key_lengths`
(`basis_network_core/tests/encryption_errors.rs:14-29`) pins the Rust behaviour; the C#
theory `DerivePeerKeys_OversizedPeerPublic_DoesNotThrow`
(`CryptoHandshakeAndLayerTests.cs:236-250`) deliberately accepts *either* answer, and its
Rust twin (`crypto_handshake_and_layer_tests.rs:190-201`) keeps that latitude. So the
oversized-`myPublic` case specifically is pinned on neither side.

**3. Role ordering is unsigned on both sides.** Worth stating because getting it wrong would
silently break interop: the C# `Compare` (`BasisCryptoHandshake.cs:77-86`) subtracts `byte`
values, which in C# are unsigned 0..255, so `a[i] - b[i]` is an unsigned comparison; Rust
uses `my_public.cmp(peer_public)` (`basis_crypto_handshake.rs:54`), a lexicographic unsigned
byte comparison with the same length tiebreak. Same A/B role for the same key pair.
`derive_peer_keys_matches_documented_hkdf_construction`
(`crypto_handshake_and_layer_tests.rs:142`, C# `:171`) recomputes the whole construction by
hand on both sides.

**4. Session map keys: `IPEndPoint` with a custom comparer → `SocketAddr`.**
C# `BasisCryptoLayer.cs:48-49` uses a `ConcurrentDictionary<IPEndPoint, Session>` with
`EndpointComparer` (`:53-69`) comparing address and port only. Rust `basis_crypto_layer.rs:27`
uses `DashMap<SocketAddr, Session>` with `SocketAddr`'s own `Eq`. For IPv4 these are
identical. For IPv6 they can differ at the margin: Rust's `SocketAddrV6` equality also
compares `flowinfo`, which `IPEndPoint` has no concept of, so two v6 endpoints differing only
in flowinfo would hit one session in C# and two in Rust. Both compare the v6 scope id.
Not pinned by a test on either side —
`Endpoints_MatchByAddressAndPort_NotByInstance` (`CryptoHandshakeAndLayerTests.cs:667`) and
its Rust twin (`:540`) both use IPv4 only.

**5. Rust returns `0` (drop) where the C# would throw.** C# `ProcessOutBoundPacket`
(`:104-119`) indexes and slices without bounds checks: a buffer with no room for the 24-byte
trailer would throw out of LiteNetLib's send path. Rust (`:104-139`) checks each slice
(`:108`, `:122`, `:125`, `:128`, `:135`) and returns `0`, documented at `:99-103` as "a packet
that should have been encrypted must never leave in the clear". Same for inbound: Rust adds
`length > data.len()` (`:157`) which the C# lacks (`:128`). Pinned by
`outbound_without_slack_or_without_header_is_dropped_not_sent_in_the_clear`
(`encryption_errors.rs:120-131`) and the `sent + 10` case at `encryption_errors.rs:97`.

**6. Rekey is atomic in Rust, has a cleartext window in C#.** C# `SetEndpointKeys`
(`:87-88`) does `TryRemove` then `_sessions[endpoint] = session` as two steps; a concurrent
`ProcessOutBoundPacket` landing between them finds no session and returns the packet
**unencrypted** (`:109` returns without touching the data). Rust does a single
`sessions.insert` (`:81`). Narrow and hard to hit, but it is a real difference in when
plaintext can reach the wire. Not pinned by a test on either side.

**7. Errors are values, not exceptions.** `set_endpoint_keys` returns `Result<(), AeadError>`
(`:69-83`) where the C# constructor throws `ArgumentException` (`BasisAeadCipher.cs:37-38`);
`derive_peer_keys` returns `Result<_, HandshakeError>` (`basis_crypto_handshake.rs:39-43`)
where the C# returned `bool` plus two `out` arrays (`BasisCryptoHandshake.cs:28-33`). Neither
loses information; the Rust gains a reason. Both leave existing state untouched on failure
(C# throws before the `TryRemove` at `:87`; Rust `?`-returns before the `insert` at `:81`) —
pinned by `SetEndpointKeys_RejectsWrongSizedKeys` (`:807`) /
`set_endpoint_keys_rejects_wrong_sized_keys` (`:646`) and
`layer_refuses_keys_of_the_wrong_length` (`encryption_errors.rs:54-67`).

**8. Counter overflow, theoretical.** Rust's `fetch_add(1) + 1` (`:118`) panics in a debug
build at `i64::MAX`; the C# `Interlocked.Increment` wraps. 2^63 packets on one session; not
reachable. Recorded for completeness, not as a finding.

## Corners cut

**The layer is no longer wired into the transport.** In C# `BasisCryptoLayer` extends
LiteNetLib's `PacketLayerBase` (`BasisCryptoLayer.cs:26`), so it can be installed under a
`NetManager` and encrypt every datagram. The Rust struct is a plain type with
`extra_packet_size_for_layer()` (`basis_crypto_layer.rs:57-60`) kept as a hook but nothing
calling it. The Rust doc comment states the reason (`:12-15`): the iroh transport is already
TLS-encrypted, so the layer survives as the pure codec for the wire format C# clients still
speak on their own direct links. Outside tests, the only Rust consumer of this module is
`basis_hello_world_client/src/hello_peer_client.rs:25,49,353`, which uses the *handshake*
only. This is a deliberate, documented reduction in reach, not a broken port — but it does
mean the Rust layer's integration with a real socket has never been exercised.

**No `Dispose`.** C# `DisposeSession` (`:153-157`) disposes both ciphers; the Rust relies on
`Drop`. Equivalent, and the Rust cannot leak the way C# `RemapEndpoint` (`:98-102`) can when
it overwrites an existing session at the new endpoint without disposing it.

**Compression of the C# fallback path.** `BasisAeadCipher.cs` carries two implementations
(native `System.Security.Cryptography.ChaCha20Poly1305` and a BouncyCastle fallback for
netstandard2.1, `:26-55`) plus a lock and scratch buffers to make the native context safe for
concurrent use (`:62-65`, `:92-94`). The Rust has one pure-Rust implementation and no lock
(`basis_aead_cipher.rs:7-9`). Nothing is lost; there is simply no Unity target to serve.

## Improvements

* `basis_x25519.rs:41-43` — explicit `was_contributory()` check rejecting low-order peer
  public keys with `X25519Error::NonContributory`, citing RFC 7748 §6.1 (`:12-14`). The C#
  gets the same refusal, but only because BouncyCastle's `X25519Agreement` happens to throw
  on an all-zero result (`BasisX25519.cs:44`), which `DerivePeerKeys` then swallows into a
  bare `false` (`BasisCryptoHandshake.cs:63-66`). Pinned by
  `handshake_refuses_identical_and_low_order_peer_keys` (`encryption_errors.rs:32-39`).
* `basis_crypto_layer.rs:99-103` + `:108-137` — an outbound packet that cannot be sealed is
  dropped rather than sent in the clear, and the failure is logged (`:132`). The C# had no
  such path because it would have thrown instead.
* `basis_crypto_layer.rs:157` — `length > data.len()` refuses an attacker-supplied length
  past the buffer instead of reading out of bounds.
* `basis_crypto_layer.rs:81` — atomic rekey, closing the cleartext window described in
  deviation 6.
* `basis_aead_cipher.rs:7-9` — no lock. The C# had to serialize every `Seal`/`Open` through
  one mutex per session (`BasisAeadCipher.cs:64`, `:94`) because the native cipher context is
  not concurrency-safe; the Rust cipher is stateless, so a busy server's sends do not
  contend. `concurrent_seals_claim_unique_counters_all_decrypt`
  (`crypto_handshake_and_layer_tests.rs:656`) covers it.
* `basis_hkdf.rs:19-27` — bounds the requested output at 255×32 and returns an error instead
  of relying on the generator to throw.

## Verdict

No weakness found. Every cryptographic parameter matches the C# exactly: X25519 with 32-byte
keys, HKDF-SHA256 over a `lowPub||highPub` salt with the same two ASCII info strings,
ChaCha20-Poly1305 with a 32-byte key, 12-byte nonce and 16-byte tag, and a 64-bit
little-endian per-session send counter placed in the low 8 bytes of the nonce with the rest
zero. The counter is post-increment on both sides, so the two implementations produce the
same nonce for the same packet number and neither introduces reuse; the one nonce-reuse
hazard that exists (reinstalling keys with a reset counter) is present in the C# too, is
documented identically, and is pinned by a test of that name on both sides. The Rust is
strictly harder to misuse: it validates key lengths before deriving, refuses low-order peer
keys explicitly, drops rather than throws on a short buffer, never emits a packet in the
clear when sealing fails, and rekeys atomically. The real reduction is reach, not strength —
the layer is no longer installed under a live socket, so its wire path is exercised only by
tests.
