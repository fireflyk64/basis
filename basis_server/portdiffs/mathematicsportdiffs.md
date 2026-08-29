# Mathematics — port diffs

C#: `Basis Server/BasisNetworkCore/Mathematics/` · Rust: `basis_server/basis_network_core/src/mathematics/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `MathExtensions.cs` | `math_extensions.rs` | 78 → 92 | faithful |
| — | `mod.rs` | — → 2 | extended (Rust module wiring, no C# analogue) |

Every C# member is present: three `Clamp` overloads, `Vector3` (fields, constructor, `operator -`,
`operator +`, `SquaredMagnitude`), `Vector4`, `Quaternion`, `float3`. Nothing was dropped and
nothing new that changes a result was added.

## Deviations

None found.

Checked, specifically:

* **Clamp branch structure.** `MathExtensions.cs:5-10`, `:12-17`, `:19-24` are
  `if (value < min) return min; if (value > max) return max; return value;`. `math_extensions.rs:4-12`,
  `:14-22`, `:24-32` are the same three statements in the same order. Same predicates, same
  operand order, so the same results for every input including the degenerate ones.
* **NaN.** `NaN < min` and `NaN > max` are both false under IEEE-754, so NaN falls through both
  branches and is returned unchanged — on both sides. Pinned:
  `Basis Server/BasisServerTests/Compression/CorePrimitiveCompressionTests.cs:475` and
  `basis_server/basis_server_tests/tests/compression/core_primitive_compression_tests.rs:373`
  (`f32`), `:500` and `:395` (`f64`).
* **Infinities.** `+inf` clamps to `max` and `-inf` to `min` on both sides, from the same
  comparisons. Pinned at `CorePrimitiveCompressionTests.cs:473-474` /
  `core_primitive_compression_tests.rs:371-372`.
* **Inverted bounds (`min > max`).** Neither side validates the interval. `Clamp(3, 7, 7)`,
  `Clamp(int.MaxValue, -3, -1)` and friends take the first matching branch identically; the C#
  tests at `CorePrimitiveCompressionTests.cs:472,487,489` have exact Rust counterparts at
  `core_primitive_compression_tests.rs:370,384,386`. Note the Rust deliberately hand-writes the
  comparisons instead of calling `f32::clamp`/`Ord::clamp`, which would have panicked on
  `min > max` and changed this behaviour; the hand-written form is the faithful one.
* **Signed zero.** `-0.0 < 0.0` is false in both languages, so `clamp(-0.0, 0.0, 1.0)` returns
  `-0.0` on both sides — neither normalises the sign. Not pinned by either suite.
* **Float width.** All fields are 32-bit on both sides (`MathExtensions.cs:28-30,56-59` vs
  `math_extensions.rs:37-39,69-72`); the `double` overload is `f64` on both. No value is promoted
  to a wider type mid-expression: .NET Core evaluates `float` arithmetic in single precision, and
  Rust `f32` arithmetic is single precision, both round-to-nearest-even.
* **`SquaredMagnitude` bit-identity.** `MathExtensions.cs:50` is `x * x + y * y + z * z` and
  `math_extensions.rs:49` is the same expression; both associate left-to-right as
  `((x*x) + (y*y)) + (z*z)`. Neither RyuJIT nor rustc contracts `a*b + c` into an FMA on its own
  (Rust needs an explicit `mul_add`, and there is none here), so the two produce bit-identical
  `f32` results for identical inputs, including the intermediate rounding on values like
  `Vector3(1e20, 1e20, 1.0)`.
* **Operators.** `operator -` / `operator +` (`MathExtensions.cs:38-45`) and the `Sub`/`Add` impls
  (`math_extensions.rs:53-65`) subtract and add component-wise in x, y, z order. Identical.
* **`Quaternion` constructor.** `MathExtensions.cs:64-70` zero-initialises via `: this()` and then
  assigns the four components; `math_extensions.rs:81-83` constructs the `Vector4` with the same
  four values. Same result. Pinned at `CorePrimitiveCompressionTests.cs:524-527` /
  `core_primitive_compression_tests.rs:412-418`.
* **No rounding or conversion code exists in this module on either side**, so there is nothing
  here that could round differently; the ranged-float and quaternion codecs that do round live in
  `Compression/`, outside this file map.
* **Type identity.** The Rust tree defines `Vector3` exactly once (`math_extensions.rs:36`); there
  is no second, divergent copy for the server to drift onto, matching the C# where every consumer
  spells it `Basis.Scripts.Networking.Compression.Vector3`
  (e.g. `Basis Server/BasisNetworkServer/Reduction/PlayerState.cs:15`).

## Corners cut

None. The module is a complete port: three clamps, four structs, two operators and one magnitude
function, all present with the same shapes.

Two things are worth knowing but are not omissions:

* `Vector4` has no constructor on either side (`MathExtensions.cs:54-60`,
  `math_extensions.rs:67-73`) — callers use field assignment / a struct literal. Faithful.
* `float3` keeps its C# lowercase name in Rust behind `#[allow(non_camel_case_types)]`
  (`math_extensions.rs:86-92`) rather than being renamed, which keeps the file map obvious. It is
  unused on both sides.

## Improvements

* `Vector3::new` and `Quaternion::new` are `const fn` (`math_extensions.rs:43`, `:81`), so vectors
  can be built in const context; the C# constructors cannot.
* `Copy + Clone + Debug + Default + PartialEq` are derived on all four structs
  (`math_extensions.rs:35,67,75,87`). The C# structs have no `==` at all (a `Vector3 == Vector3`
  does not compile there) and inherit `ValueType.Equals`. Caveat, since the semantics are not the
  same: derived `PartialEq` on `f32` is IEEE equality, so `NaN != NaN`, whereas C#'s
  `ValueType.Equals` uses `float.Equals` where `NaN.Equals(NaN)` is true. No call site on either
  side compares these structs, so this is latent API surface, not a live difference.
* The dead local in the C# clamp path has no equivalent to go wrong: nothing here allocates or
  boxes, and the Rust takes `&self` for `squared_magnitude` on a `Copy` struct
  (`math_extensions.rs:48`).

## Verdict

This is the cleanest of the four modules: a line-for-line port with identical arithmetic. Every
input class that could diverge — NaN, ±infinity, inverted bounds, signed zero, float width,
expression association — was checked and matches, and all five C# test methods have exact Rust
counterparts asserting the same values (`core_primitive_compression_tests.rs:363-418`). For the
same inputs the Rust produces bit-identical `f32`/`f64` results, and nothing was simplified away.
