# BasisNetworkCompute — port diffs

C#: `Basis Server/BasisNetworkCompute/` · Rust: `basis_server/basis_network_compute/src/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BasisComputeFactory.cs` | `lib.rs` (`BasisComputeFactory`, 21–52) | 36 → 32 | ported; selector grammar narrowed, drift guard replaced |
| `GpuDistanceSolver.cs` | — | 260 → 0 | **not ported** |
| `CpuDistanceSolver.cs` | `lib.rs` (`CpuSimdDistanceSolver`, 55–126) | 59 → 72 | replaced: SIMD instead of `Parallel.For`, and now the only backend |
| `DistanceMath.cs` | — (folded into `basis_network_core::BasisNetworkCommons`) | 72 → 0 | deliberately dropped |
| `BasisNetworkCompute.csproj` | `Cargo.toml` | 31 → 12 | ILGPU dependency gone; `rayon` declared and unused |

The contract itself lives outside both trees: `Basis Server/BasisNetworkCore/Compute/BasisDistanceSolver.cs`
(59 lines) → `basis_server/basis_network_core/src/compute/basis_distance_solver.rs` (41 lines).

## Deviations

### D1. GPU execution was not ported. This is the whole of `GpuDistanceSolver.cs`.

**What the C# had, and Rust does not.** `GpuDistanceSolver.cs:15-260` is an ILGPU backend:
`Context.Create` (`:50`), device enumeration that deliberately excludes ILGPU's own CPU
accelerator (`:97-101`), a CUDA accelerator built with `ScheduleBlockingSync` rather than the
spinning default (`:76-78`, with the measurement in the comment at `:68-75`), a kernel compiled
from IL by `LoadAutoGroupedStreamKernel` (`:30-32`), a 2-D kernel over (receiver, sender)
(`:166-188`), device buffers kept and grown rather than reallocated per sweep (`:226-246`),
explicit host→device and device→host copies (`:208-223`) and a `Synchronize` (`:220`).

The Rust has no counterpart to any of it. `lib.rs:29-46` can only ever return a
`CpuSimdDistanceSolver`; `lib.rs:24` (`CPU_SIMD_BACKEND = "cpu-simd"`) is the only backend id in
the crate. So what was not ported is: CUDA and OpenCL execution, device enumeration, per-device
selection, device memory management, and the kernel-compilation failure paths.
`BasisComputeFactory::describe_devices` (`lib.rs:49-51`) always prints exactly one line.

**Do the results agree numerically? Yes, exactly — better than the C# required.** The C# kernel
could not call into the protocol assembly, so `DistanceMath.Encode` (`DistanceMath.cs:20-29`) was
a hand transcription of `BasisNetworkCommons.EncodeAvatarIntervalByte`
(`BasisNetworkCore/Protocol/BasisNetworkCommons.cs:951-960`), and `VerifyAgainstProtocol`
(`DistanceMath.cs:60-71`) existed to catch the two drifting apart. The device was additionally
allowed a ±1 interval-byte difference because a GPU contracting the three squared terms into
fused multiply-adds rounds once where the CPU rounds three times
(`BasisNetworkServer/Reduction/BasisServerReductionSystemEvents.Distance.cs:446-449`).

The Rust needs neither. `write_pair` (`lib.rs:94-110`) calls the protocol encoder itself
(`basis_network_core/src/protocol/basis_network_commons.rs:49-65`, byte-for-byte the same
algorithm as the C# original), and the vector pass computes only `dx*dx + dy*dy + dz*dz` per lane
(`lib.rs:74-77`) with no FP contraction, so the eight-wide path and the scalar tail (`lib.rs:83-89`)
produce bit-identical `f32`. Measured on this host (AVX2), `cargo test -p basis_server_tests
--test compute` prints, from `distance_offload_tests.rs:98`:

```
cpu-simd (256-bit vectors (32 B/op) [AVX2 SSE4.2 BMI2] ...) over 261632 pairs:
tier mismatches 0, interval differing 0, interval beyond one step 0
```

The ±1 tolerance the ported test still carries (`distance_offload_tests.rs:100`, mirroring
`BasisServerTests/Compute/DistanceOffloadTests.cs:116`) is therefore dead slack — it pins nothing
that can currently fail.

**Fallback behaviour, C#.** Any of: no `BasisNetworkCompute.dll` beside the server
(`BasisNetworkServer/Reduction/BasisComputeBackend.cs:33-37`), no non-CPU device
(`GpuDistanceSolver.cs:54-58`), a selector matching nothing (`:60-66`), or any exception at all
(`:83-89`) yields no solver and a reason string. On a host without a GPU — which is most of them —
there is no solver, and the reduction system keeps the sweep on the CPU at
`DistanceUpdateIntervalTicks`.

**Fallback behaviour, Rust.** The only refusal that is not a bad selector is
`!BasisSimdCapabilities::hardware_accelerated()` (`lib.rs:42-44`), which is true only when
`Level::new().is_fallback()` (`basis_network_core/src/basis_simd_capabilities.rs:46-48`) — i.e. on
a target with no vector unit at all. On x86-64 (SSE2 baseline) and aarch64 (NEON) the factory
**always** succeeds. The practical inversion: where the C# almost never had a solver, the Rust
almost always does. See D2 for what that costs.

Pinned by tests only in the weak sense that `backend_loads_or_explains_why_not`
(`distance_offload_tests.rs:38-47`) accepts either outcome, exactly as the C# original did
(`DistanceOffloadTests.cs:34-45`).

### D2. The "offload" is now strictly more work than the CPU sweep it displaces, and it is on by default.

The Rust CPU fallback sweep is *already* vectorised and parallel:
`BasisServerReductionSystemEvents::run_distance_slice`
(`basis_network_server/src/reduction/basis_server_reduction_system_events/distance.rs:156-177`)
dispatches `distance_row` (`distance.rs:116-154`) — the same `f32x8` formulation and the same
`encode_avatar_interval_byte` per lane — across receivers through `parallel_for`, which is a rayon
pool (`.../parallelism.rs:112-127`).

The offload path does the same arithmetic single-threaded (`lib.rs:61-91`, a plain `for` over
`slice_start..slice_end`), and then walks the slice a second time in parallel to scatter the
result (`distance.rs:322-341`). It also copies the three position arrays into fresh `Vec`s on
every slice (`distance.rs:239-241`) because `BasisDistanceSolveRequest` owns `Vec<f32>`
(`basis_distance_solver.rs:16-18`), where the C# struct held `float[]` references to `_denseX`
(`BasisNetworkCore/Compute/BasisDistanceSolver.cs:28-30`, filled at
`BasisServerReductionSystemEvents.Distance.cs:410-412`) and copied nothing.

Meanwhile holding a solver flips the refresh period from `DistanceUpdateIntervalTicks` to
`ComputeDistanceUpdateIntervalTicks` (`distance.rs:40-41`), whose defaults are 125 and 32
(`basis_network_core/src/configuration/basis_server_configuration.rs:32,35`, matching
`BasisNetworkCore/Configuration/BasisServerConfiguration.cs:59,62`). `EnableComputeOffload`
defaults to `true` (`basis_server_configuration.rs:33`). So out of the box a Rust server runs the
sweep through the single-threaded solver roughly 3.9× more often than the C# ran its parallel one.

I have not benchmarked this and do not claim a number. What the code establishes is: same
arithmetic, fewer threads, one extra pass, one extra copy per slice, at ~3.9× the frequency.

Not pinned. `refresh_period_tracks_whether_a_device_is_actually_carrying_the_sweep`
(`distance_offload_tests.rs:177-201`) asserts the period follows the solver; nothing asserts that
having the solver is an improvement.

### D3. Device-selector grammar narrowed; `auto` is now refused.

`GpuDistanceSolver.Select` (`GpuDistanceSolver.cs:141-165`): empty **or the literal `auto`** picks
the best device (`:144-147`); an integer picks by index (`:150-155`); anything else is a
case-insensitive **substring** match against the device name (`:157-160`), which is why the config
documentation promises that `"4090"` or `"Radeon"` is enough.

Rust (`lib.rs:33-41`): an integer must be `0`; otherwise the selector must be empty or equal
(`eq_ignore_ascii_case`) to `"cpu-simd"` or `"cpu"`. `"auto"` parses as neither, so it falls to
`lib.rs:39-41` and returns `Err("no compute device matches 'auto'; …")`. A deployment carrying
`ComputeDevice=auto` in `config.xml` silently loses the offload on Rust where C# honoured it.

Not pinned: `device_selector_unknown_refuses_rather_than_falling_back`
(`distance_offload_tests.rs:149-159`) only tries `"no-such-device-xyz"`.

### D4. The operator-facing documentation still describes the GPU that is gone.

`basis_network_core/src/configuration/basis_config_xml_docs.rs:248-250` still tells an operator
that the sweep "may run on a GPU when one is present", that an empty `ComputeDevice` picks "the
best device present, preferring a CUDA one and then the largest memory", that `"4090"` or
`"Radeon"` will match, and that `ComputeDistanceUpdateIntervalTicks` applies "while the sweep is
running on a compute device". None of that is true of this crate as ported. (The file is in
`basis_network_core`, not this module, but it is this module's contract with the operator.)

### D5. The factory's precondition changed.

C# `BasisComputeFactory.TryCreateDistanceSolver` refuses to build anything when the transcribed
encoder disagrees with the protocol at any interval (`BasisComputeFactory.cs:28-32`). Rust instead
refuses a non-positive `base_interval_ms` (`lib.rs:30-32`), which the C# never checked. The drift
check is structurally unnecessary now that there is exactly one encoder (`lib.rs:96`), so this is
a fair trade — but the two factories reject different inputs and neither rejection is pinned by a
test on either side.

### D6. `IDisposable` dropped from the solver contract.

C# `IBasisDistanceSolver : IDisposable` (`BasisNetworkCore/Compute/BasisDistanceSolver.cs:52`);
`GpuDistanceSolver.Dispose` (`GpuDistanceSolver.cs:248-259`) freed five device buffers, the
accelerator and the ILGPU context, and the reduction system called it when dropping a refused
backend (`BasisServerReductionSystemEvents.Distance.cs:507-515`). The Rust trait has no dispose
(`basis_distance_solver.rs:37-41`) and `disable_distance_solver` simply drops the `Box`
(`distance.rs:317`). Correct for a solver that owns no OS resources; it has to come back if a GPU
backend ever lands.

### D7. Out-of-range writes are dropped rather than thrown.

`write_pair` writes through `interval_byte.get_mut(slot)` / `quality.get_mut(slot)`
(`lib.rs:106`), so a caller passing an output buffer that is too short gets stale bytes rather
than an error; and `solve_impl` clamps `n` to the shortest position array (`lib.rs:63`) and
`break`s out of the receiver loop when `i >= n` (`lib.rs:65-67`), leaving the tail of that row
untouched. The C# would have thrown — `CopyToCPU` with a mismatched span
(`GpuDistanceSolver.cs:222-223`), or an index-out-of-range in the CPU loop
(`CpuDistanceSolver.cs:52-53`) — and the caller's `catch` would have disabled the solver for the
process (`BasisServerReductionSystemEvents.Distance.cs:423-429`). The Rust caller sizes its
buffers correctly (`distance.rs:233-237`), so nothing exercises this today.

## Corners cut

* The GPU backend in its entirety (D1). No CUDA, no OpenCL, no kernel compilation, no device
  enumeration, no device memory pool, no `ScheduleBlockingSync` equivalent.
* `CpuDistanceSolver`'s `Parallel.For` over receivers (`CpuDistanceSolver.cs:36-55`) has no
  counterpart: `solve_impl` is serial (`lib.rs:64`). `rayon` is declared as a dependency
  (`Cargo.toml:11`) and is not used anywhere in the crate.
* `DistanceMath.BuildIntervalTickTable` (`DistanceMath.cs:46-54`) is not in the crate. The
  equivalent 256-entry table is built inline by the caller (`distance.rs:186`). The only C# user
  of the helper was the benchmark (`BasisServerBenchmark/Micro/GpuBench.cs:252`), and `GpuBench`
  itself has no Rust port — so the whole GPU-vs-CPU measurement harness is gone with it.
* `Level::new()` is re-resolved on every `solve` call (`lib.rs:123`), where the C# compiled its
  kernel once in the constructor (`GpuDistanceSolver.cs:30-32`). Small, but it is per-call work
  the original did not have.
* Because `describe_devices` can only ever emit `[0]` (`lib.rs:49-51`), the server's
  "this host has more than one compute device" hint (`distance.rs:205-208`, ported from
  `BasisServerReductionSystemEvents.Distance.cs:365-370`) is now unreachable code.

## Improvements

* One encoder instead of two. `write_pair` calls `BasisNetworkCommons::encode_avatar_interval_byte`
  directly (`lib.rs:96`), which deletes the entire class of bug that `DistanceMath.Encode`
  (`DistanceMath.cs:20-29`) plus `VerifyAgainstProtocol` (`:60-71`) existed to manage.
* Exact agreement with the scalar reference rather than "within one step": 0 differing interval
  bytes over 261,632 pairs, measured above.
* `Result<Box<dyn IBasisDistanceSolver>, String>` (`lib.rs:29`) instead of a nullable return plus
  `out string? failure` (`BasisComputeFactory.cs:26`) — the failure reason cannot be dropped.
* The crate is linked directly (`basis_compute_backend.rs:22-33`) rather than resolved out of a
  DLL by type and method name (`BasisComputeBackend.cs:41-58`), so a rename is a compile error
  instead of a runtime "the backend returned no solver".
* `(… ) as i32` on the raw interval (`lib.rs:95`) saturates and is defined for every input,
  including NaN; the C# `(int)` cast (`DistanceMath.cs:40`) is an unchecked conversion whose result
  for an out-of-range float is unspecified.
* A non-positive base interval is rejected (`lib.rs:30-32`) where the C# would have produced
  nonsense intervals.

## Verdict

The contract is ported faithfully and the arithmetic is not merely equivalent but exact — better
than the C# could achieve, because the Rust kernel can call the protocol encoder the C# kernel had
to transcribe. Everything else about this module is a downgrade in capability that the code is
honest about in its own doc comment (`lib.rs:4-10`) but that the configuration documentation
(D4) is not.

Two things need attention before this is trusted in production. First, D2: the offload is on by
default, always engages, does the same work as the fallback with less parallelism and an extra
copy, and buys a 3.9× faster refresh schedule with it. Either the solver should be made
rayon-parallel (the dependency is already declared) or `EnableComputeOffload` should default off
until a real device backend exists. Second, D3: `ComputeDevice=auto` is a plausible existing
setting and now silently disables the feature.

The GPU path is not "temporarily missing"; nothing in the crate is shaped to receive it back. The
factory returns a concrete type behind a `Box<dyn>` and would accommodate a second backend, but
device enumeration, selection-by-name and the per-device failure reporting would all have to be
rebuilt from the C# — which is the strongest argument for keeping `GpuDistanceSolver.cs` around as
the specification.
