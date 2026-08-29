# Compute core — port diffs

C#: `Basis Server/BasisNetworkCore/Compute/` · Rust: `basis_server/basis_network_core/src/compute/`

This module is the *contract* only — two plain data structs and one backend interface. The
backends that implement it live elsewhere (`Basis Server/BasisNetworkCompute/` and
`basis_server/basis_network_compute/`) and are outside this file's scope, but the interface
cannot be judged without them, so they appear in the verdict.

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `Compute/BasisDistanceSolver.cs` | `compute/basis_distance_solver.rs` | 60 → 41 | deviates |
| — | `compute/mod.rs` | — → 2 | Rust-only module glue |

One C# file holds all three types (`BasisDistanceSolveParameters` `:8-16`,
`BasisDistanceSolveRequest` `:26-38`, `IBasisDistanceSolver` `:52-59`); the Rust keeps them in
one file too (`:3-10`, `:15-33`, `:37-41`).

## Deviations

**1. `SliceLength` and `ResultLength` changed signedness, losing the C#'s two guards.**

C# (`BasisDistanceSolver.cs:36-37`):

```
public int SliceLength => SliceEnd - SliceStart;
public long ResultLength => (long)SliceLength * PlayerCount;
```

Rust (`basis_distance_solver.rs:26-32`):

```
pub fn slice_length(&self) -> usize { self.slice_end - self.slice_start }
pub fn result_length(&self) -> usize { self.slice_length() * self.player_count }
```

Two things moved:

* **Underflow.** `SliceEnd < SliceStart` gives a negative `int` in C# — nonsense, but it
  propagates as a negative and a `Parallel.For(0, negative)` simply does no work
  (`CpuDistanceSolver.cs:36`). In Rust `slice_end - slice_start` on `usize` **panics in a
  debug build and wraps to a near-`usize::MAX` value in release**, which the caller would
  then use to size a buffer. The inputs are all server-generated, so this is a latent
  robustness difference rather than a live bug, but it is the kind of inversion worth
  recording: the C# degraded, the Rust aborts or explodes.
* **The deliberate widening was dropped.** The C# cast to `long` before multiplying
  (`:37`) specifically so a large slice × large roster could not overflow a 32-bit `int`.
  `GpuDistanceSolver.cs:198` reads it as `long resultLength = request.ResultLength;`. The Rust
  multiplies `usize` by `usize` (`:31`), which is 64-bit on the targets this builds for, so
  the headroom is in practice the same — but on a 32-bit target it would be 32-bit again, and
  in a debug build the multiplication panics rather than wrapping. The C#'s intent survives
  only by accident of pointer width.

Neither accessor is used by the Rust server: `distance.rs:233` recomputes
`(slice_end - slice_start) * player_count` inline rather than calling `result_length()`, so
`slice_length()` / `result_length()` currently have **no caller at all** in the Rust tree.
Not pinned by a test on either side — `grep SliceLength\|ResultLength\|slice_length\|result_length`
over both test suites returns nothing.

**2. `IDisposable` is gone from the interface.** C# `IBasisDistanceSolver : IDisposable`
(`BasisDistanceSolver.cs:52`) — the tests rely on it, `using IBasisDistanceSolver? solver = …`
(`DistanceOffloadTests.cs:36`, `:54`, `:126`, …), and the GPU backend needs it to release the
ILGPU accelerator. Rust `pub trait IBasisDistanceSolver: Send + Sync` (`:37`) has no `Drop`
bound; cleanup rides on the implementing type's own `Drop` when the `Box<dyn …>` is dropped
(`basis_compute_backend.rs:22`, `distance.rs:27`). That is the idiomatic and equivalent
answer — it just means the interface no longer *states* that a solver owns a resource.

**3. `Send + Sync` added.** Rust `:37`. The C# interface imposes nothing; the solver is in
fact shared across the reduction system's threads in both ports
(`BasisServerReductionSystemEvents.Distance.cs:36`, `distance.rs:27`). The Rust makes the
existing requirement checkable.

**4. `Solve` signature.** C# `void Solve(ref BasisDistanceSolveRequest request, byte[]
intervalByte, byte[] quality)` (`:58`) — `ref` for the struct copy, not for mutation, and two
arrays the callee writes into. Rust `fn solve(&self, request: &BasisDistanceSolveRequest,
interval_byte: &mut [u8], quality: &mut [u8])` (`:40`). Same data flow; the Rust makes
"request is read-only, outputs are written" explicit, and `&self` states the solver is not
mutated.

**5. Field types.** `float[]` → `Vec<f32>` (`:28-30` → `:16-18`), `int` player/slice indices →
`usize` (`:31-33` → `:19-21`). `BasisDistanceSolveParameters` is unchanged field-for-field:
six fields, five `float`/`f32` and one `int`/`i32` `BaseIntervalMs`/`base_interval_ms`
(`:10-15` → `:4-9`). `Backend`/`DeviceName` `string` properties become `backend()`/
`device_name()` returning `&str` (`:54-56` → `:38-39`).

## Corners cut

**The interface's rationale comments were dropped.** The C# carries three explanatory
paragraphs the Rust does not:

* `:22-24` — why positions are dense and in roster order rather than keyed by peer id ("the
  server's own arrays are keyed by peer id and therefore full of holes; compacting once on the
  way in costs one pass"). Rust keeps one clause of this (`:13`) and loses the reason.
* `:42-46` — why the interface lives in this assembly at all ("the GPU backend carries ILGPU,
  and this assembly is compiled by Unity"). Gone entirely. The Rust reason would differ
  anyway — there is no Unity build — but the crate-split rationale is not restated.
* `:47-50` — why `CachedIntervalTicks` is *not* in the result ("a pure function of the
  interval byte … sending it would triple the bytes crossing the bus to carry no
  information"). The Rust keeps the result layout sentence (`:35-36`) and drops the reason,
  so a future maintainer has nothing telling them why the third field is absent.

These are comments, not behaviour. They are the sort of thing a port loses quietly and a
reader later has to reconstruct.

**No GPU backend behind the interface.** Not this module, but it is what the interface exists
for. C# ships `GpuDistanceSolver` (ILGPU, `Backend = "gpu"`,
`BasisNetworkCompute/GpuDistanceSolver.cs:15`, `:34-35`) alongside `CpuDistanceSolver`
(`Backend = "cpu"`, `CpuDistanceSolver.cs:23`). The Rust ships one backend,
`CpuSimdDistanceSolver` (`basis_network_compute/src/lib.rs:45`, `:113-119`), and says so
plainly at `lib.rs:4-8`: "The C# backend compiled the sweep to a GPU through ILGPU. This
crate offers the same contract … A GPU backend can slot in." The abstraction is intact and a
GPU implementation can be added without touching this module; the capability is simply not
there today.

## Improvements

* `basis_distance_solver.rs:37` — `Send + Sync` on the trait, making the cross-thread sharing
  the reduction system already does a compile-time fact instead of a convention.
* `basis_distance_solver.rs:40` — `&BasisDistanceSolveRequest` and `&self` state at the type
  level what the C# `ref`-plus-comment only implied: the solver reads the request and does not
  mutate itself.
* `basis_distance_solver.rs:16-18` — `Vec<f32>` carries its own length, where the C# `float[]`
  fields (`:28-30`) had a separate `PlayerCount` (`:31`) that callers had to keep consistent
  with the array lengths by hand.

## Verdict

This is a small, mostly mechanical port of a contract, and the shapes line up: same six
parameter fields, same request fields, same two-bytes-per-pair result layout at
`(sliceIndex * PlayerCount) + j`, same three interface members. The one substantive change is
signedness — `int`/`long` became `usize`, which turns the C#'s harmless negative slice into a
debug panic or a release wraparound and drops the explicit `long` widening the C# added on
purpose; neither accessor has a Rust caller or a test today, so nothing catches it. Beyond
that the losses are documentation: three rationale paragraphs, and (outside this module) the
GPU backend the interface was extracted to accommodate.
