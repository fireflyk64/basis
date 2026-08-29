# compression — port diffs

C#: `BasisNetworkCore/Compression/` · Rust: `basis_network_core/src/compression/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BasisAvatarBitPacking.cs` | `basis_avatar_bit_packing.rs` | 164 → 185 | faithful |
| `BasisAvatarBundleCodec.cs` | `basis_avatar_bundle_codec.rs` | 163 → 141 | faithful |
| `BasisAvatarBundleDictionary.cs` | `basis_avatar_bundle_dictionary.rs` (+ `.bin`) | 218 → 252 | deviates |
| `BasisAvatarBundleZstd.cs` | `basis_avatar_bundle_zstd.rs` | 309 → 214 | deviates |
| `BasisAvatarChannelMap.cs` | `basis_avatar_channel_map.rs` | 324 → 295 | faithful |
| `BasisAvatarDeadband.cs` | `basis_avatar_deadband.rs` | 81 → 59 | faithful |
| `BasisAvatarDeltaCompression.cs` | `basis_avatar_delta_compression.rs` | 287 → 269 | faithful |
| `BasisAvatarIdleSuppression.cs` | `basis_avatar_idle_suppression.rs` | 55 → 43 | faithful |
| `BasisBitCodec.cs` | `basis_bit_codec.rs` | 166 → 105 | extended (fail-soft bounds) |
| `BasisBoneRotationCompression.cs` | `basis_bone_rotation_compression.rs` | 705 → 533 | faithful |
| `BasisGenericBoneRotation.cs` | `basis_generic_bone_rotation.rs` | 233 → 158 | faithful |
| `BasisNetworkCompressionExtensions.cs` | `basis_network_compression_extensions.rs` | 32 → 26 | faithful |
| `BasisNetworkPrimitiveCompression.cs` | `basis_network_primitive_compression.rs` | 58 → 54 | faithful |
| `BasisObjectPool.cs` | `basis_object_pool.rs` | 37 → 24 | faithful |
| `BasisPayloadDiff.cs` | `basis_payload_diff.rs` | 91 → 76 | faithful |
| `BasisResidualCodec.cs` | `basis_residual_codec.rs` | 178 → 194 | deviates (one edge case) |
| `BasisNetworkCompressionExtensions.cs.meta` | — | 1 → — | not ported (Unity asset metadata) |
| `BasisNetworkPrimitiveCompression.cs.meta` | — | 1 → — | not ported (Unity asset metadata) |
| — | `mod.rs` | — → 37 | extended (Rust module wiring, no C# analogue) |

Every public and private member of every C# file has a counterpart. Enumerated: all 24 members of
`BasisAvatarBitPacking`, all 44 of `BasisBoneRotationCompression` (including every private table),
all 10 of `BasisBitCodec`, all of `BasisAvatarChannelMap`/`BasisAvatarChannelLayout`/
`BasisAvatarChannel`, all 12 of `BasisAvatarDeltaCompression`, all 3 of `BasisAvatarBundleCodec`,
all 28 of `BasisAvatarBundleZstd` (test seams included), all 10 of `BasisAvatarDeadband`, both of
`BasisAvatarIdleSuppression`, all 15 of `BasisGenericBoneRotation`, both of
`BasisNetworkCompressionExtensions`, all 12 of `BasisRangedUshortFloatData`, all 3 of
`BasisObjectPool`, all 4 of `BasisPayloadDiff`, all 13 of `BasisResidualCodec` (both bit cursors
in full). Nothing was dropped.

The task brief said the C# uses `System.IO.Compression`/Deflate. It does not: `grep` for
`System.IO.Compression|Deflate|GZip|Brotli` over `BasisNetworkCore/Compression/` returns nothing.
The module's only compressor is Zstd via `ZstdSharp` (`BasisAvatarBundleZstd.cs:3-4`), and the LZ4
half is named here (`BasisAvatarBundleZstd.cs:59`) but implemented outside this module, in the
reduction system. The Rust `bundle_deflate_ms` counter in
`basis_network_server/src/diagnostics/basis_network_health_check.rs:252` is a legacy metric name
carried over from the C#, not a Deflate codec.

## Deviations

**1. `SignedEgBits(int.MinValue)`: C# returns −1, Rust panics.**
`BasisResidualCodec.cs:172-176` computes `2 * BitLength(zz + 1) - 1` in `int`. For
`value == int.MinValue`, `zz` is `0xFFFFFFFF`, `zz + 1` wraps to 0, `BitLength(0)` is 0, and the
C# returns `-1`. `basis_residual_codec.rs:28-31` computes the same expression in `u32`:
`2 * 0u32 - 1` underflows, which panics in a debug build ("attempt to subtract with overflow" at
`basis_residual_codec.rs:30`, confirmed by running it) and wraps to `u32::MAX` in release.

Why it does not reach the wire: the only caller is
`basis_avatar_delta_compression.rs:120-124`, which feeds it `wrap_signed(diff, ch.width)` with
`ch.width <= BasisResidualCodec::MAX_WIDTH` (24), bounding `diff` to `[-2^23, 2^23)`. The
companion `write_signed_eg` (`basis_residual_codec.rs:86-97`) mirrors the C#
(`BasisResidualCodec.cs:78-86`) exactly and is equally wrong for `int.MinValue` on both sides —
both emit a bare `1` bit that decodes as 0 — so this is a divergence in the *cost* function only.
Not pinned: `residual_codec_tests.rs:97-107` covers 0, ±1, ±2 and a monotonicity sweep, none of
which reach `i32::MIN`.

**2. A zstd context is destroyed on the "does not fit" path instead of re-pooled.**
`BasisAvatarBundleZstd.cs:256-279`: `TryWrap` returning `false` for a too-small destination is a
normal return, so the `finally` at `:275-278` puts the `Compressor` back in the bag; only a thrown
exception disposes it (`:266-274`). `basis_avatar_bundle_zstd.rs:188-196` has one arm for both:
`Err(_) => None` at `:195` (and `:211` for the decompressor) drops the `CCtx`, freeing it. The
next bundle then re-enters `rent_compressor` (`:151-162`), builds a fresh context and re-digests
the 16 KiB dictionary through `load_dictionary` at `:160`.

Why it matters: the doc comment at `BasisAvatarBundleZstd.cs:252-255` describes the not-fitting
result as the packer's designed, recurring "overshoot, retry with a smaller chunk" signal, not as
an error. Verified live: `try_compress` into a 4-byte buffer returns `None`. Same wire bytes,
different steady-state cost when the packer overshoots. No test pins the pooling behaviour.

**3. Out-of-bounds bit access is fail-soft in Rust and throwing in C#.**
C# indexes the array directly and raises `IndexOutOfRangeException`:
`BasisBitCodec.cs:122` (read), `:140` (or), `:158` (replace); `BasisResidualCodec.cs:66`
(`BitWriter.WriteBits`), `:131` (`BitReader.ReadBits`). Rust reads a missing byte as zero
(`basis_bit_codec.rs:61`), drops a write past the end (`:80`, `:97`), latches
`BitWriter::overflowed` (`basis_residual_codec.rs:68-71`) and latches `failed`
(`basis_residual_codec.rs:149-152`).

Wire-neutral for well-formed input — every field of every layout is inside the payload by
construction (`residual_codec_tests.rs:12-26` proves the channel list totally partitions the
payload) — but it changes what a bug or a short buffer does: silent zeros instead of a stack
trace. Pinned deliberately by `basis_bit_codec_tests.rs:153-162`
(`a_field_past_the_end_is_clipped_not_overrun`), which asserts the clipping is the intended
contract, and by `residual_codec_tests.rs:112-125`.

**4. `WordDiffMask` clamps its length to the shorter buffer.**
`basis_payload_diff.rs:22` does `length.min(current.len()).min(baseline.len())` before scanning.
`BasisPayloadDiff.cs:48-71` has no such clamp: a short buffer faults in the `Vector<byte>`
constructor at `:60` or the indexer at `:86`. If a caller ever passed a length longer than a
buffer, the Rust would return a mask with *fewer* dirty bits than reality, which the delta encoder
uses to skip fields — a silently wrong delta rather than a crash. Not live: the sole production
caller validates first (`basis_avatar_delta_compression.rs:65-70`). Not pinned as such;
`basis_payload_diff_tests.rs:83-91` pins the adjacent "only the first `length` bytes count"
contract.

**5. `Quat::Display` rounds ties differently from `Quat.ToString`.**
`BasisGenericBoneRotation.cs:105` uses `{x:F6}`, which rounds half away from zero in .NET;
`basis_generic_bone_rotation.rs:27` uses `{:.6}`, which rounds half to even. Diagnostic output
only — no caller in either tree feeds it back into a codec.

### Checked and found identical

* **Every quantiser produces the same integer.** Each is expression-for-expression the same, in
  the same float/double widths:
  * `EncodeSignedUnit` (`BasisBoneRotationCompression.cs:517-524`) →
    `encode_signed_unit` (`basis_bone_rotation_compression.rs:379-386`). `Math.Round(float)` has no
    `float` overload in .NET, so it binds to `Math.Round(double)` — banker's rounding. The widening
    `f32→f64` is exact, so rounding-half-to-even on the widened value equals
    `f32::round_ties_even`, which is what `round_half_even` at `basis_avatar_bit_packing.rs:177-180`
    is. The C# `Clamp(…, 0, maxQ)` and the Rust `.min(max_q)` after a saturating `as u32` agree
    because the argument is already in `[0, maxQ]`.
  * `EncodeSmallestThree` (`:586-626`) → `encode_smallest_three` (`:428-475`): same
    largest-component search with the same `>` (not `>=`) tie-breaks at `:593-595` / `:434-443`,
    same negation rule, same component permutation, same `1/maxRange` reciprocal, same
    `clamp(-1,1)` then `*0.5+0.5` then `*maxQ` then round, same
    `maxIdx | qa<<2 | qb<<(2+bpc) | qc<<(2+2*bpc)` packing.
  * `EncodeAxisMm` (`BasisAvatarBitPacking.cs:81-91`) → `:78-94`, and
    `QuantizeHipsAxis` (`:147-155`) → `quantize_hips_axis` (`:153-165`): same NaN→0, same
    saturation at `±PositionMmLimit` / `±HipsDeltaMaxQ`, same banker's rounding. Confirmed live:
    `encode_axis_mm(0.0005) → 0`, `encode_axis_mm(0.0025) → 2` — ties to even, matching
    `Math.Round`.
* **Bit widths and range mapping.** `BPC_HIGH/MEDIUM/LOW/VERY_LOW` (`:95-149` → `:61-102`),
  `MAX_COMPONENT` (`:183-220` → `:106-122`), `BONE_DOF` (`:246-257` → `:126-131`),
  `BONE_AXIS_A/B` (`:265-286` → `:138-157`), `BONE_RANGE_A/B` (`:290-311` → `:163-183`),
  `HINGE/TWIST/SINGLE_BITS` (`:316-318` → `:186-188`), `CURL/SPLAY_BITS` (`:473-480` →
  `:339-340`), `BONE_WRITE_ORDER` (`:47-63` → `:31-46`) — all entry-for-entry identical,
  compared elementwise. `InvSqrt2 = 0.70710678118f` (`:28`) and
  `std::f32::consts::FRAC_1_SQRT_2` (`:19`) are the same `f32`, bits `0x3F3504F3` — verified
  numerically, not assumed. The Rust deliberately keeps the C# decimal literals for
  `BONE_RANGE_A/B` (`:160-162` comment) rather than substituting `PI`-derived constants.
* **Resulting packet geometry.** Rotation bytes 44/53/67/94 and payload bytes 74/83/97/159 across
  the ladder, matching the C# doc comments at `BasisBoneRotationCompression.cs:92`, `:115`,
  `:127`, `:139`. Verified by running, and pinned at
  `core_primitive_compression_tests.rs:277-306`.
* **Double-precision intermediates.** Everywhere the C# wrote `(float)Math.Sqrt(x)` or
  `(float)Math.Atan2(a,b)` — computing in `double` and narrowing once — the Rust writes
  `f64::from(x).sqrt() as f32` / `f64::from(a).atan2(f64::from(b)) as f32`:
  `:352` → `:227`, `:356` → `:229`, `:377` → `:253`, `:384-385` → `:259-262`, `:398` → `:288`,
  `:430-431` → `:311-312`, `:647` → `:493`, `:658` → `:503`,
  `BasisGenericBoneRotation.cs:132` → `basis_generic_bone_rotation.rs:72`. Doing the arithmetic in
  `f32` would have changed results; the port did not.
* **Bit-stream layout.** `BasisBitCodec` read/or/replace are the same LSB-first byte loop with the
  same `MaxWideBits = 57` single-load fast path (`:47`, `:73-78` → `:12`, `:29-38`). The Rust's
  `u64::from_le_bytes` (`:19`) and the C#'s `ReadUnaligned` + conditional
  `ReverseEndianness` (`:55-57`) both yield a little-endian word.
* **Delta codec.** Dirty-mask width and field indices (`BasisAvatarDeltaCompression.cs:45-47` →
  `:44-46`), the word-mask prefilter, the per-field raw-vs-residual mode-bit decision
  (`rawBits < residualBits` at `:151` → `:127`), the channel iteration order, the zig-zag
  Exp-Golomb form, the end-of-body padding and the exact-length check (`:225` → `:207`) all match.
  `BasisAvatarChannelMap` builds the identical channel partition, including the rotation tail pad
  (`:250-252` → `:224-227`) and the spare 40th hips-delta bit (`:275-279` → `:253-260`).
* **Bundle grouping.** `TryClassify` and `TryFlatten` (`BasisAvatarBundleCodec.cs:47-161`) are
  reproduced statement for statement at `basis_avatar_bundle_codec.rs:19-140`, including the
  column-major un-transpose walk order and the `src != read + bodyTotal` consistency check
  (`:152` → `:128`). `DELTA_AVATAR_CHANNEL = 30` matches
  `BasisNetworkCommons.cs:1081`.
* **Zstd frame parameters.** `contentSizeFlag 0`, `checksumFlag 0`, `dictIDFlag 0`,
  `windowLog 17`, `format = magicless`, level `-2`, dictionary via `loadDictionary`:
  `BasisAvatarBundleZstd.cs:168-185` → `basis_avatar_bundle_zstd.rs:114-134`. Verified live that
  a Rust frame starts with no zstd magic and round-trips (1400 B → 271 B → 1400 B identical).
  `ZstdSharp` is a managed port of the reference encoder and the Rust `zstd` crate binds libzstd,
  so the emitted bytes are not guaranteed identical between them at the same level, but both emit
  a conformant zstd frame with the same header suppressions, so each side decodes the other's.
* **The dictionary itself.** The C# base64 (`BasisAvatarBundleDictionary.cs:24-206`) and the Rust
  `BASE64_LINES` (`:21-239`) concatenate to the same 21848-character string, decoding to the same
  16384 bytes (sha256 `8ccdddfe990a0b07ac1c41f9af708b84e0ef02d0146792b4dee760dde1074484`), which
  is also byte-identical to `basis_avatar_bundle_dictionary.bin`. `GENERATION = 1` on both sides,
  and `pack_flags`/`codec_of`/`dict_generation_of` use the same 3-bit / 5-bit split
  (`BasisAvatarBundleZstd.cs:64-76` → `:43-59`).
* **SIMD and scalar agree, in both languages.** `BasisPayloadDiff.cs:55-66` steps by
  `Vector<byte>.Count` (16, 32 or 64 depending on the host, or skips the vector pass entirely when
  `Vector.IsHardwareAccelerated` is false); `basis_payload_diff.rs:34-46` steps by a fixed 32 via
  `fearless_simd`'s `u8x32`, with `dispatch!` at `:24` selecting the detected level and falling
  back to scalar emulation. Every candidate step (16/32/64) is a multiple of 8, so both loops
  leave `i` word-aligned and both fall through to the identical `word_bit` loop (`:68` → `:48-51`)
  and ragged `tail_bit`. The vector pass is used only to *skip*: on a mismatching block both
  languages recompute every word of that block scalar-ly, so the emitted mask is independent of
  the width the host happens to have. Pinned in Rust against a bit-exact oracle for every length
  1..=200 and every single-byte position — which covers all three block boundaries —
  at `basis_payload_diff_tests.rs:29-40` and `:42-56`.
* **`FastLog2` de Bruijn table replaced by `leading_zeros`.**
  `BasisNetworkPrimitiveCompression.cs:42-56` versus
  `basis_network_primitive_compression.rs:51-53`. `0` maps to `0` on both (the de Bruijn multiply
  of 0 indexes entry 0, which is 0); every nonzero input gives `floor(log2)`. Pinned across the
  boundaries at `core_primitive_compression_tests.rs:354-359`.
* **Deadband arithmetic.** `MinAbsDotForAngleDegrees` promotes to `double` on both sides
  (`BasisAvatarDeadband.cs:44-45` → `:25-27`). `QuatsWithin` accumulates the dot in `double` in
  the same left-to-right order (`.sum()` over an `f64` iterator folds from `0.0`, and `0.0 + a == a`),
  and the Rust's `dot.is_nan() || dot.abs() < min_abs_dot` at `:38` is exactly the C#'s
  `!(Math.Abs(dot) >= minAbsDot)` at `:61` for every input including a NaN threshold.
* **Trig ULP exposure is not a live wire risk.** `Math.Sin/Cos/Atan2` are not correctly rounded in
  either runtime, so a value sitting exactly on a quantisation boundary could in principle encode
  differently. In the Rust tree the trig-bearing entry points (`extract_hinge_twist`,
  `extract_single_axis`, `compose_hinge_twist`, and therefore `encode_restricted`) have no
  production caller: `grep` finds only `basis_network_client_console/src/avatar/fake_pose_generator.rs:166`
  and the test suite. The server repacks tiers with the integer `QuantRescaleTable`, exhaustively
  verified against exact division at `quant_rescale_table_tests.rs:9-23`. The same hazard already
  existed between the C# server and the Unity Burst job it mirrors
  (`BasisBoneRotationCompression.cs:335`).

## Corners cut

**1. The dictionary now exists twice with nothing checking the copies agree.** The C# has one
source of truth: `BasisAvatarBundleDictionary.cs:209` decodes the base64 constant declared two
lines above it. The Rust has the base64 at `basis_avatar_bundle_dictionary.rs:20-240` *and* a
pre-decoded `basis_avatar_bundle_dictionary.bin` that `bytes()` pulls in with `include_bytes!`
at `:249`. There is no `build.rs` in `basis_network_core/`, `base64()` at `:242` has zero callers
in the whole workspace, and no test decodes it and compares against `bytes()`. Today the two
agree (checked by hand). A regenerated dictionary that updates one and not the other is precisely
the silent failure the file's own header warns about at
`BasisAvatarBundleDictionary.cs:9-12`: frames carry no dictionary id, so zstd will not catch a
mismatch — the receiver decodes against the wrong dictionary and produces garbage that then
parses as a malformed bundle.

**2. `available()` allocates and copies 16 KiB on every call.**
`basis_avatar_bundle_zstd.rs:91-93` calls `dictionary()` purely to ask `is_empty()`, and
`dictionary()` at `:78-83` unconditionally `to_vec()`s the whole embedded dictionary (`:82`) or
clones the override (`:80`). The C# equivalent at `BasisAvatarBundleZstd.cs:125` reads
`_dictionary.Length`. The production path calls it twice per bundle —
`basis_network_server/src/reduction/basis_server_reduction_system_events/bundling.rs:343` and
again inside `try_compress` at `basis_avatar_bundle_zstd.rs:184` — so a busy server does two
16 KiB heap allocations and memcpys per bundle to answer a question the C# answers with a field
load.

**3. The context pool re-locks per iteration.** `basis_avatar_bundle_zstd.rs:153` and `:166`
acquire the mutex fresh on each turn of the drain loop, where the C# uses a lock-free
`ConcurrentBag.TryTake` (`BasisAvatarBundleZstd.cs:215`, `:230`). Correct, just more contended;
the loop only spins when the epoch has changed, which is rare.

**4. `BitWriter::overflowed()` is never consulted.** It is defined at
`basis_residual_codec.rs:53-55` and set at `:69`, but `build_delta`
(`basis_avatar_delta_compression.rs:102-148`) ignores it and returns a length computed from
`bit_position()` regardless. Not live — the `dst` bound is validated at `:68-70` — but where the
C# would have thrown out of `BasisAvatarDeltaCompression.cs:132`, a future layout change that
undersized the buffer would produce a truncated delta that the length check at the receiver
(`:207`) would reject as corruption rather than pointing at the encoder.

**5. Four C# characterization tools were not ported**, recorded honestly at
`basis_server_tests/tests/compression/main.rs:1-6`: `PositionQuantizationExperiment`,
`SimdCodecBenchmark`, `BundleCompressionExperiment` and `BundleDictionaryTrainer`. Two of those
are load-bearing for maintenance rather than for correctness. `BundleDictionaryTrainer.cs` is the
only thing that can regenerate a dictionary generation, so a Rust-only tree cannot retrain one.
`SimdCodecBenchmark` is what `BasisBitCodec.cs:32-33` names as the way to re-derive the
reads-go-wide / writes-stay-narrow split; the Rust inherits that decision as a comment
(`basis_bit_codec.rs:4-7`) with no way to re-measure it in-tree.

## Improvements

* **Invalid qualities are unrepresentable.** In C# `(BitQuality)200` is a legal `byte`-backed enum
  value with three different fates: `Geo[(int)q]` at `BasisAvatarDeltaCompression.cs:95` and
  `HINGE_BITS[(int)q]` at `BasisBoneRotationCompression.cs:320` throw, while `GetBpcTable` at
  `:536-543` silently falls back to `BPC_HIGH` — so a bad quality can produce a *wrong-width*
  encode rather than a failure. `basis_avatar_bit_packing.rs:6-24` makes the enum closed with
  `from_byte` returning `Option`, and `get_bpc_table` at
  `basis_bone_rotation_compression.rs:394-401` is exhaustive with no fallback arm at all.
* **No `unsafe` anywhere in the module.** `BasisBitCodec.cs:55` and `BasisPayloadDiff.cs:76-77`
  both use `Unsafe.ReadUnaligned<ulong>` against a raw element reference;
  `basis_bit_codec.rs:16-20` and `basis_payload_diff.rs:58-66` get the same result from
  `slice::get` plus `u64::from_le_bytes` and a slice equality, with the endianness handled by
  construction rather than by the `BitConverter.IsLittleEndian` branch at `BasisBitCodec.cs:57`.
* **Partial writes are refused rather than half-committed.**
  `BasisAvatarBitPacking.EncodePosition` (`:103-108`) calls `EncodeAxisMm` three times and would
  throw on the second or third, leaving the first axis written into the payload.
  `basis_avatar_bit_packing.rs:107-112` checks room for all nine bytes before writing any, and
  `encode_hips_delta` at `:130-132` does the same for its five.
* **The caller-supplied delta window cannot overflow.**
  `BasisAvatarDeltaCompression.cs:189` tests `deltaStart + deltaLen > delta.Length` in `int`
  arithmetic, which wraps negative for large inputs and passes the guard;
  `basis_avatar_delta_compression.rs:162-164` uses `checked_add` and returns false. Same at
  `delta_body_length` (`:213`).
* **`Math.Round`'s banker's rounding is made explicit.** The C# depends on overload resolution
  binding `Math.Round(float)` to the `double` overload — silent, and easy to break by inserting a
  cast. `round_half_even` at `basis_avatar_bit_packing.rs:174-180` names the requirement and
  documents why, and every quantiser calls it.
* **NaN in the quantisers is defined rather than unspecified.** `(uint)Math.Round(double.NaN)` at
  `BasisBoneRotationCompression.cs:621-623` is unspecified behaviour per the C# spec (it happens
  to give 0 on .NET Core x64); the Rust `as u32` at `basis_bone_rotation_compression.rs:471` is
  defined to be 0. Pinned at `restricted_dof_codec_tests.rs:131-138`.
* **Bit access can no longer fault on a hostile payload.** See deviation 3 — this is the same
  change, and on the receive path it is strictly the safer end. `BasisBitCodec.cs:35-39` explains
  that payload buffers are exact-sized and often rented, so a read past the logical end would read
  another tenant's bytes; the Rust cannot do that even when a length is wrong. Pinned at
  `basis_bit_codec_tests.rs:153-162`.
* **`fearless_simd` gives a compiled fallback path rather than a runtime branch.**
  `BasisPayloadDiff.cs:55` re-tests `Vector.IsHardwareAccelerated` per call and, when false, runs
  a byte loop that is a different code path from the vector one;
  `basis_payload_diff.rs:24` monomorphises `word_diff_mask_impl` per level, so the fallback runs
  the *same* algorithm with emulated vectors and the oracle tests at
  `basis_payload_diff_tests.rs:42-56` cover whichever one the host selected.

## Verdict

The encoded bytes are compatible with the C# clients. Every bit width, quantiser table, range
mapping and rounding mode is identical — verified elementwise on the tables, numerically on
`InvSqrt2` and the rounding boundaries, and end-to-end on the packet sizes (74/83/97/159), which
the Rust test suite pins as a protocol contract. The bundle layer matches too: the zstd dictionary
is byte-identical, the frame suppressions are the same four, and both sides emit conformant
magicless zstd that the other decodes. The five behavioural deviations are all off the happy path
— an `i32::MIN` cost query that panics instead of returning −1, a discarded compressor context on
overshoot, and three places where a bounds violation now returns zeros instead of throwing — and
none of them changes a byte for well-formed input. The two things worth fixing are the
unverified duplicate copy of the zstd dictionary, which is exactly the silent-mismatch failure the
file warns about, and the 16 KiB allocation `available()` performs twice per bundle on the send
path.
