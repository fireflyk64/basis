# Contrib/Crypto — port diffs

C#: `Basis Server/Contrib/Crypto/` · Rust: `basis_server/contrib/crypto/src/`

Primitives: Ed25519 signing/verification, X25519 key agreement, HKDF-SHA256, ChaCha20-Poly1305.
The C# leans on BouncyCastle 2.5.0 (plus `System.Security.Cryptography.ChaCha20Poly1305` when the
runtime has it); the Rust uses ed25519-dalek 3.0.0, x25519-dalek 3.0.0, hkdf 0.13, sha2 0.11 and
chacha20poly1305 0.11 (curve25519-dalek 5.0.0 underneath).

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `Ed25519.cs` | `ed25519.rs` | 87→43 | ported |
| `Crypto.cs` | `lib.rs` | 34→85 | ported (Rust file also carries the module tree) |
| `BasisX25519.cs` | `basis_x25519.rs` | 48→50 | ported + typed errors |
| `BasisHkdf.cs` | `basis_hkdf.rs` | 23→29 | ported + typed errors |
| `BasisAeadCipher.cs` | `basis_aead_cipher.rs` | 145→84 | ported, API reshaped |
| `IsExternalInit.cs` | — | 14→0 | n/a (C# compiler shim for `init` on netstandard) |
| `Crypto.csproj`, `Crypto.asmdef`, `package.json` | `Cargo.toml` | — | n/a |
| `../Crypto.Tests/Ed25519Tests.cs` | `tests/ed25519_tests.rs` | 291→188 | ported, same five RFC 8032 vectors |
| — | `tests/crypto_errors.rs` | 0→107 | new (negative tests for AEAD, X25519, HKDF, Ed25519) |

## Deviations

### 1. The two Ed25519 implementations do not accept exactly the same public keys

`Ed25519.cs:41` constructs `Ed25519PublicKeyParameters(pubkey.V)`, and BouncyCastle 2.5.0 validates
the point in that constructor. `ed25519.rs:26` calls `VerifyingKey::from_bytes`, which only requires
that the encoding decompresses; the malleability checks happen later in `verify_strict`
(`ed25519.rs:33`).

Measured (both sides run against the same byte strings, C# through
`new Ed25519PublicKeyParameters(...)`, Rust through `VerifyingKey::from_bytes`):

| 32-byte key | C# / BouncyCastle | Rust / dalek |
| --- | --- | --- |
| valid prime-order key | accepted | accepted |
| identity `0100..00` | rejected, `ArgumentException: invalid public key` | parsed; rejected later by `verify_strict` |
| order-4 `0000..00`, order-2 `ECFF..7F`, both order-8 points | rejected in the constructor | parsed; rejected later by `verify_strict` |
| mixed-order (`A + T`, torsion component, not small order) | accepted | accepted |
| non-canonical `y` (`y + p` for `y ≤ 18`) | rejected | **accepted** |

So the only class where the accept/reject verdict actually differs is a non-canonically encoded
`y` coordinate: `ed25519.rs:26` treats `y` and `y + p` as the same key, `Ed25519.cs:41` rejects the
second form. This is reachable only for `y ≤ 18` (otherwise `y + p` overflows 32 bytes), and the
handful of curve points with such a `y` have no known discrete log, so no usable key is affected.
It is a real difference in the accepted byte set, not a practical forgery path.

Everything that matters for real keys matches. Verified on both sides with identical inputs:

* All five RFC 8032 §7.1 vectors sign to the identical 64 bytes and verify
  (`Ed25519Tests.cs:42-57` / `ed25519_tests.rs:88-119`).
* A signature whose `S` is not reduced mod ℓ (a valid vector with `S + ℓ` substituted) is rejected
  by both — BouncyCastle's `CheckScalarVar`, dalek's `check_scalar` via
  `Scalar::from_canonical_bytes`.
* A signature forged against the identity public key (`R = B`, `s = 1`, which satisfies the
  cofactorless equation `[s]B = R + [k]A` for *any* message because `[k]A = O`) is rejected by both
  — C# at key construction, Rust at `verify_strict`'s `is_small_order` check.
* Wrong-length keys and signatures return `false` on both sides without throwing
  (`Ed25519.cs:38-46` and `:50` for the signature, `ed25519.rs:23-31`).

No test on either side pins the key-validation edge cases; the RFC vectors pin the common path.

### 2. Key and signature byte layouts are identical — no deviation

`Ed25519.cs:14-15` takes its sizes from `Rfc8032.Ed25519` (32/32); `ed25519.rs:9-11` from
`ed25519_dalek` (32/32/64). Both treat the private key as the 32-byte seed, not the 64-byte
expanded form, and both derive the public key from it (`Ed25519.cs:19-33` /
`ed25519.rs:15-19`). The shared RFC vectors are the proof: identical seeds produce identical public
keys and identical signatures.

### 3. X25519 refuses the same inputs, by different mechanisms

* Low-order peer key: `BasisX25519.cs:44` calls `X25519Agreement.CalculateAgreement`, which throws
  `InvalidOperationException: X25519 agreement failed` on an all-zero shared secret (measured).
  `basis_x25519.rs:41-43` returns `X25519Error::NonContributory`. Same refusal, exception vs
  `Result`. Pinned on the Rust side by `crypto_errors.rs:68`.
* Wrong key length: `BasisX25519.cs:30,39,40` throw `ArgumentException` out of the BouncyCastle
  parameter constructors; `basis_x25519.rs:47-49` returns `X25519Error::KeyLength`. Pinned by
  `crypto_errors.rs:64-66`.

### 4. X25519 private keys come out clamped in C#, unclamped in Rust

`BasisX25519.cs:20-25` builds the key with `new X25519PrivateKeyParameters(Rng)`, which clamps
before storing; the encoded private key always has its low three bits clear and its top two bits
`01` (measured: `b8 … 5f`). `basis_x25519.rs:24-28` fills 32 raw random bytes and hands them to
`StaticSecret::from`, which stores them verbatim (x25519-dalek 3.0.0 `x25519.rs:247-252` — clamping
happens in `diffie_hellman`, not on construction), so `to_bytes()` returns unclamped bytes
(measured: `b0 … f3`, `a3 … 8a`, `ac … bb`).

This is interoperable — both `derive_public_key` and `agree` clamp on use, on both sides, so the
same seed yields the same public key and the same shared secret either way. It matters only if the
raw private key bytes are compared across implementations or fed to a scalar-multiply that does not
clamp. No test pins the byte pattern.

### 5. HKDF over-long output: exception vs typed error

`BasisHkdf.cs:19` lets BouncyCastle's `HkdfBytesGenerator.GenerateBytes` throw when
`length > 255 × 32`. `basis_hkdf.rs:20-22` checks the bound up front and returns
`HkdfLengthError`. Same limit (8160 bytes), pinned by `crypto_errors.rs:80-81`.

An empty salt behaves identically: `HkdfParameters` maps a null/empty salt to a zero-filled
`hashLen` HMAC key, and HMAC pads any short key to the block size with zeros, which is what the
`hkdf` crate's `Hkdf::new(Some(&[]), …)` does too.

### 6. AEAD: the offset parameters are gone

`BasisAeadCipher.cs:60` / `:90` take `(buffer, offset, length, tagDest, tagOffset)`;
`basis_aead_cipher.rs:59` / `:74` take `(&mut [u8], &mut [u8])` and let the caller slice. Same wire
format (12-byte nonce, one AAD byte, 16-byte tag), same in-place semantics. Callers must slice
themselves; nothing on the Rust side re-introduces the offsets.

### 7. AEAD: what the buffer holds after a failed `open`

`BasisAeadCipher.cs:88-89` documents the buffer as undefined on tag mismatch, and what actually
happens differs by path. The native path (`:104`) has .NET zero the caller's region. The
BouncyCastle fallback decrypts into `_scratchOut` and only copies back at `:118`, which is after
`DoFinal` at `:117` has thrown on a mac failure, so the caller's buffer is left untouched holding
ciphertext. `basis_aead_cipher.rs:72-73` documents the same "undefined", and chacha20poly1305 0.11
verifies the tag with a constant-time compare *before* applying the keystream (`cipher.rs:85-97`),
so the Rust also leaves the buffer holding ciphertext. Net: Rust matches the C# fallback path and is
one step less tidy than the C# native path, which scrubs. No unauthenticated plaintext is exposed on
any of the three.

### 8. Nonce and tag lengths are checked in Rust, partly unchecked in C#

`basis_aead_cipher.rs:50-55` and `:61-63,:76-79` reject a wrong-size nonce or an under-size tag
buffer with `AeadError::NonceLength` / `TagLength` (pinned by `crypto_errors.rs:18-21`). The C#
native path throws `ArgumentException` from `_native.Encrypt`, but the BouncyCastle fallback at
`BasisAeadCipher.cs:76` and `:108` does `nonce.CopyTo(_nonceBuf)` into a reused 12-byte instance
field — `Span.CopyTo` only throws when the source is *longer* than the destination, so a nonce
shorter than 12 bytes is silently padded with whatever the previous call left in `_nonceBuf`. No
caller in the tree passes a short nonce, so this is latent rather than live, but it is a hazard the
port removes.

### 9. Error style and small API differences

* `Ed25519.Sign` returns `bool` with an `out Signature?` (`Ed25519.cs:54`); `Ed25519::sign` returns
  `Option<Signature>` (`ed25519.rs:37`). The C# unreachable-failure branch (`Ed25519.cs:74-81`:
  `Debug.Fail` then rethrow) has no counterpart.
* `BasisAeadCipher` implements `IDisposable` (`BasisAeadCipher.cs:138-143`) to release the native
  handle; the Rust type owns no handle and needs no `Drop`.
* `basis_aead_cipher.rs:46-48` adds `try_new` as a pure alias of `new`. Redundant.
* The C# records are `Generator.Equals` `[Equatable]` records with `OrderedEquality`
  (`Crypto.cs:11-27`); the Rust newtypes are a macro (`lib.rs:29-79`) deriving `PartialEq`. Same
  value semantics.

## Corners cut

* Neither side zeroizes key material. `PrivKey` is a plain `byte[]` in C# (`Crypto.cs:23`) and a
  plain `Vec<u8>` in Rust (`lib.rs:72-75`); no `Zeroize`, no `SecureString`. The port inherits this
  rather than introducing it, but a security-critical crate is where it would have been worth
  fixing.
* `SharedSecretKey` (`Crypto.cs:27` / `lib.rs:76-79`) is declared and never used, on both sides.
* `SigningAlgorithm` still has exactly one variant on both sides (`Crypto.cs:30-33` /
  `lib.rs:82-85`), so `verify_signature`'s "unsupported algorithm" branch is unreachable except
  through an unrecognised JWK.
* The AAD is still a single byte on both sides (`BasisAeadCipher.cs:60` / `basis_aead_cipher.rs:59`).
  Not a regression, but it caps what can be bound into the tag.
* No test on either side covers the Ed25519 key-validation edge cases in deviation 1, X25519
  clamping, or the AEAD buffer state after a failed `open`.

## Improvements

* Every failure is a typed value. `AeadError`, `X25519Error` and `HkdfLengthError`
  (`basis_aead_cipher.rs:16-29`, `basis_x25519.rs:8-16`, `basis_hkdf.rs:8-13`) replace a mix of
  `bool`, `null` and thrown `ArgumentException` / `InvalidOperationException` /
  `DataLengthException`. Callers can tell a malformed packet from an authentication failure.
* Nonce and tag lengths are validated (deviation 8), closing the stale-nonce hazard in the C#
  BouncyCastle fallback.
* The `lock (_sync)` that `BasisAeadCipher.cs:64` and `:94` needed — because the native
  ChaCha20-Poly1305 context is not safe for the concurrent sends a busy server makes — is gone.
  `basis_aead_cipher.rs` takes `&self` and the underlying cipher has no shared mutable state, so
  seal and open run concurrently.
* `verify_strict` adds the small-order-`R` check that RFC 8032 §5.1.7 makes optional and that
  BouncyCastle's `ImplVerify` does not perform. No constructible input exercises it (an `s`
  satisfying the equation with a torsion `R` would require a discrete log of a torsion point with
  respect to `B`), but it is strictly more conservative.
* `tests/crypto_errors.rs` is new: 107 lines pinning malformed keys, nonces and tags, tampered
  ciphertext, AAD and key mismatch, X25519 low-order points, and HKDF bounds. The C# had negative
  tests for Ed25519 only.
* `BasisHkdf::MAX_OUTPUT_LENGTH` (`basis_hkdf.rs:17`) is public; the C# limit was implicit in
  BouncyCastle.
* Both sides' tag comparisons are constant-time — chacha20poly1305 uses `subtle`
  (`cipher.rs:89-90`), BouncyCastle uses `Arrays.ConstantTimeAreEqual`, .NET native uses the
  platform library. `was_contributory()` compares against zero in constant time, as does
  BouncyCastle's branchless `AreAllZeroes`. I found no operation on either side that is
  variable-time over secret data: dalek's vartime paths in `verify_strict` operate on the public
  key, signature and message only.

## Verdict

The port is faithful and interoperable. Ed25519 key, signature and seed layouts are byte-identical,
proven by the same five RFC 8032 vectors producing the same signatures on both sides. Both
implementations reject non-reduced `S`, wrong-length inputs, small-order public keys and the
identity-key forgery. X25519, HKDF-SHA256 and ChaCha20-Poly1305 are wire-compatible; a session
established by one side can be opened by the other.

The one measured difference in what verification accepts is non-canonically encoded public-key `y`
coordinates (deviation 1), which the Rust accepts and the C# rejects, and which no usable key can
have. It is worth recording but is not a weakness. The port is otherwise stronger than the
original: typed errors, validated nonce and tag lengths, no lock, and a new negative-test file. The
one thing neither side does, and should, is zeroize private key material.
