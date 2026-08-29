# reduction — port diffs

C#: `BasisNetworkServer/Reduction/` · Rust: `basis_network_server/src/reduction/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `AvatarQualityRepacker.cs` | `avatar_quality_repacker.rs` | 261→218 | complete |
| `BasisComputeBackend.cs` | `basis_compute_backend.rs` | 106→40 | reworked (direct link, no reflection) |
| `BasisServerReductionSystemEvents.Bundling.cs` | `…events/bundling.rs` | 463→358 | complete |
| `…Configuration.cs` | `…events/mod.rs` (`Statics`, accessors) | 118→(544) | complete, merged |
| `…Distance.cs` | `…events/distance.rs` | 561→343 | complete; vector path weaker (D4) |
| `…Inbound.cs` | `…events/inbound.rs` | 201→165 | complete |
| `…Lifecycle.cs` | `…events/tick.rs` (`process_pending_removals`) + `mod.rs` | 101→(342/544) | complete, merged |
| `…LoadControl.cs` | `…events/load_control.rs` + `tick.rs` (`run_load_controller`) | 389→143+(342) | socket growth stubbed (C1) |
| `…MessageProcessing.cs` | `…events/message_processing.rs` | 320→229 | complete |
| `…Parallelism.cs` | `…events/parallelism.rs` | 433→323 | complete; own pool (D8) |
| `…Quality.cs` | `…events/mod.rs:268-312` | 64→(544) | complete, merged |
| `…SendLoop.cs` | `…events/send_loop.rs` | 543→313 | complete |
| `…Serialization.cs` | `…events/serialization.rs` | 414→361 | complete + `publish_frame` |
| `…Startup.cs` | `…events/mod.rs:218-226` (`ensure_started`) | 34→(544) | partial (D15) |
| `…State.cs` | `…events/mod.rs` (`Statics`) + `tick.rs` (`TickState`) | 81→(544/342) | complete, merged |
| `…TestSeams.cs` | `…events/test_seams.rs` | 82→126 | complete; one seam vacuous (C4) |
| `…Tick.cs` | `…events/tick.rs` | 321→342 | phase 4 shortened (D3) |
| `PeerTrackingData.cs` | `peer_tracking_data.rs` | 19→17 | complete |
| `PendingAvatarSend.cs` | `pending_avatar_send.rs` | 11→14 | complete (`byte[]`→`Arc<[u8]>`) |
| `PlayerState.cs` | `player_state.rs` | 137→186 | restructured into `SenderWork`/`ReceiverData`/`SenderFrame` |
| `Profiling.cs` | `profiling.rs` | 388→397 | complete |
| `QuantRescaleTable.cs` | `quant_rescale_table.rs` | 114→91 | complete |
| `QueuedMessage.cs` + `QueuedMessagePool.cs` | `queued_message.rs` | 12+48→44 | complete |
| `ShardedConcurrentDictionary.cs` | `sharded_concurrent_dictionary.rs` | 92→117 | complete; `Mutex<HashMap>` shards (D11) |
| — | `reduction/mod.rs` | —→23 | re-exports |
| **total** | | **5313→4394** | |

Every tunable constant in the module was checked value-for-value and they all match: the three
distance thresholds (100/900/2500 m², `State.cs:55-57` / `mod.rs:153-155`), `BSRBaseMultiplier`
1.0, `BSRSIncreaseRate` 0.01, `BSRSMillisecondDefaultInterval` 50, `intervalMs` 10, `MaxSpinMs`
2.5, `Min/MaxTickIntervalMs` 4/20, `TicksPerSendInterval` 4, `DistanceUpdateIntervalTicks` 125,
`ComputeDistanceUpdateIntervalTicks` 32, `MinDistanceSliceReceivers` 128, `TickControlWindow` 16,
overrun escalate/recover/panic 0.25/0.05/0.75, `PanicEscalationSteps` 4, `MaxLoadShedTier` 2,
`MaxShedIntervalDoublings` 3, drop escalate/recover 1.0/0.125, the whole bundle-economics block
(0.85/0.60/0.95/0.75/0.05/0.01/0.98/600/2/128/40/4/32/3), `PendingShrinkWindowTicks` 256,
`PendingMinCapacity` 64, `RetainedScratchBytes` 16 KiB, `InitialPlayerArrayCapacity` 256,
`MaxRemovalsPerTick` 8, and the pool controller's 0.6/0.25/0.70/24/1.05/30 s/100 ms.

## Deviations

### D1. The send-worker ceiling is pinned at 8 on every host (most important)

The pool's hard ceiling is `min(totalCores, 8 * max(1, sendSocketCount))` on both sides — C#
`BasisNetworkCommons.cs:277-278`, Rust `basis_cpu_budget.rs:371-374` — and `sendSocketCount`
starts at 1 on both (`BasisNetworkCommons.cs:288`, `basis_cpu_budget.rs:261`).

The C# raises it: `LoadControl.cs:69` calls `BasisCpuBudget.SetSendSocketCount(lnl.BoundSendSocketCount)`
on **every** rebalance (~100 ms), and again at `LoadControl.cs:213` after growing a socket. With
`MultiSocketCount = 4` a 32-core host reaches a 32-worker ceiling.

The Rust never calls it. `rebalance_cpu_budget` (`load_control.rs:42-76`) has no equivalent line,
and `grep -rn set_send_socket_count` over the whole workspace finds only `basis_cpu_budget.rs:383`
and two tests. So `send_socket_count` stays 1 for the process lifetime and
`reduction_send_cap()` (`basis_cpu_budget.rs:823`) can never exceed 8, no matter how many cores
the box has. `degree_for` (`parallelism.rs:135`) clamps to that ceiling, so on a 16- or 32-thread
host the Rust send pass runs at a quarter to an eighth of the width the C# would reach.

This is partly explained by the transport swap — the note at `load_control.rs:91-93` says the iroh
endpoint has one socket per address family — but the Rust LiteNetLib impl *does* still bind
`MultiSocketCount` SO_REUSEPORT sockets on Linux (`net_manager.rs:632-670`) and there is no
`bound_send_socket_count` accessor anywhere for the budget to read. On that transport the pool is
sized for capacity that exists and is not reported.

Not pinned by a test: `send_pool_widen_trial_tests.rs` drives `resolve_widen_trial` and
`expire_learned_ceiling` directly and never exercises the ceiling.

### D2. Peer-update pressure is hardcoded to zero

C# `LoadControl.cs:34` reads `lnl.PeerUpdatePressure` and `:38-49` differentiates the transport's
`PeersUpdatedTotal`/`PeerUpdateBusyMicros` into `BasisCpuBudget.PeerUpdateLease`, then
`:51` reports both pressures. Rust `load_control.rs:49-51` passes a literal `0.0`. The allocator's
pressure-driven split (gain 4, `basis_cpu_budget.rs:322`) therefore always reads the peer pool as
idle and biases toward the send pool — the opposite direction from D1, but D1's cap binds first on
any host with more than 8 cores. `MachineUtilization` and `PeerUpdateWorkerCap` are also no longer
pushed back into the transport (C# `LoadControl.cs:58-66`).

### D3. Phase 4 no longer kicks the transport

C# `Tick.cs:129-132` calls `manager.TriggerUpdate()` at the end of every tick so merged unreliable
packets flush at the tick rate. Rust `tick.rs:183-188` does only the PIP update. Mitigated: the
Rust LiteNetLib logic thread self-drives at `update_time_ms`, default 2 ms
(`net_manager.rs:90`, `:723-724`), which is shorter than the 4-20 ms tick period, so the flush
still happens at least as often. Side effect: the profiler's `trig` phase (`tick.rs:186`) now
times only the PIP pass, so that figure is not comparable across the two ports.

### D4. The distance sweep's vector path only vectorises the distance

C# `Distance.cs:274-304` runs eight lanes through squared distance, then vectorised interval
encoding (`Distance.cs:193-212`, a multiply-shift transcription of the protocol encoder) and a
vectorised three-way tier select (`Distance.cs:288-291`), writing tracking from the lanes.

Rust `distance.rs:131-143` computes the eight squared distances with `f32x8`, then drops to scalar
per lane: `encode_avatar_interval_byte` + `decode_avatar_interval_ms` (branch + divide each) and
`get_quality_index`. `get_quality_index` (`mod.rs:271-281`) reads up to three relaxed atomics per
call, so a full sweep does up to 3N² atomic loads that the C# hoisted into locals before its
`Parallel.For` (`Distance.cs:231-237`). Same results — the Rust is by construction the protocol
encoder — but the per-lane work is much heavier than the C#'s. No test covers the cost.

### D5. Per-receiver and per-slice allocation in the sweep

`distance.rs:161` allocates `vec![(0,0,0); player_count]` inside the `parallel_for` body — one heap
allocation per receiver per slice (≈8 KB × 1000 receivers per sweep at 1000 players). The C#
(`Distance.cs:239-326`) writes straight into `tracking[jId]`.

`distance.rs:239-241` copies all three position arrays (`dense_x[..n].to_vec()` ×3) into
`BasisDistanceSolveRequest` on every device slice; C# `Distance.cs:410-412` passes the arrays by
reference.

### D6. The message-processing phase allocates per tick and loses its reusable buffer

Rust `tick.rs:140-141`: `std::mem::take(&mut tick.messages_snapshot)` leaves a `Vec::new()` behind,
so the 1024-capacity drain buffer allocated at `tick.rs:51` is discarded on the first tick that
carries traffic and re-grown from zero on every subsequent tick. The taken `Vec` is then rebuilt as
`Vec<parking_lot::Mutex<Option<QueuedMessage>>>` (a second allocation) purely so the `Fn(usize)`
body can take ownership, adding a mutex acquire per message. C# `Tick.cs:74-88` drains into the
long-lived `_messagesSnapshot` (`State.cs:25`) and indexes it directly (`State.cs:42-52`).

### D7. `sort_pending_by_channel` refcounts every entry twice

Rust `bundling.rs:254-259` does `dst.extend(pending[..count].iter().cloned())` then
`pending[*slot] = p.clone()`. `PendingAvatarSend.source` is an `Arc<[u8]>`
(`pending_avatar_send.rs:211`), so each flush costs 2N atomic increments plus 2N decrements. C#
`Bundling.cs:326-329` copies plain structs holding a raw `byte[]` reference — no atomics. The C#'s
own note (`Bundling.cs:282-295`) puts the whole sort at 7.3 ns/entry, which is the budget this
change is spending against.

### D8. Own thread pool, rebuilt whenever the degree moves

C# `Parallelism.cs:255` changes `parallelOptions.MaxDegreeOfParallelism`, an int store on the
shared thread pool, and `Parallelism.cs:361-364` explicitly records that a dedicated pool was tried
in place of `Parallel.For` and did not pay for itself.

Rust `parallelism.rs:198` stores the degree, and `pool()` (`parallelism.rs:85-109`) then rebuilds a
whole `rayon::ThreadPool` — spawning `degree` OS threads and joining the previous pool's threads
when the last `Arc` drops. Bounded to the rebalance cadence (100 ms, `parallelism.rs:74`) and only
when the degree actually changes, so steady state is free; a ramp (join storm, `degree_for`
doubling per step at `parallelism.rs:168`) pays a spawn/teardown per step. The same pool serves the
message phase, both distance paths and the send loop, matching the C#'s single `parallelOptions`.

### D9. `bypass_reduction` is read from the frame snapshot, not live

Rust `send_loop.rs:145` reads `frame.bypass_reduction`, frozen when the sender's last inbound frame
was published (`serialization.rs:245`). `set_bypass_reduction` (`mod.rs:238-248`) updates the
atomic but does not republish the frame, so an admin toggle takes effect on the sender's *next*
frame rather than on the next tick, as in C# `SendLoop.cs:180`. Low impact in practice: the
new-data gate (`send_loop.rs:136`) means a sender with no new frame sends nothing either way, so
the visible delay is one send interval.

### D10. Locking model in the send loop

Rust holds `state_i.receiver.lock()` from `send_loop.rs:113` through the entire sender scan and the
flush at `:225`, i.e. across deflate and the transport send. C# holds nothing
(`SendLoop.cs:91-321`) except a brief `lock (stateI)` to grow the tracking array
(`SendLoop.cs:146`).

I traced the other holders of that lock — the distance sweep (`distance.rs:163`, `:328`), removals
(`tick.rs:330`) and keyframe requests (`serialization.rs:292`) — and all of them run in earlier,
serialised phases of the same tick, so there is no cross-phase contention and no lock cycle. Within
the send loop each worker holds a different receiver's lock. No race or deadlock found; the cost is
one uncontended acquire per receiver per tick plus a 40-byte `PeerTrackingData` copy per pair at
`send_loop.rs:146` that the borrow checker forces and the C# did not need.

### D11. Sharded map shards are plain mutexes

Rust `sharded_concurrent_dictionary.rs:270` is `Vec<Mutex<HashMap<i32, V>>>`; every `get_cloned`
(`:311`) and `contains_key` (`:318`) takes the shard lock. C#
`ShardedConcurrentDictionary.cs:235` shards a `ConcurrentDictionary`, whose reads are lock-free.
`process_message` (`message_processing.rs:106`) runs in parallel across senders and takes a shard
lock per message, where the C# took none.

### D12. Minor behavioural differences

- `send_loop.rs:268-271` counts a tail send only when `send_unreliable_raw_merge` returns `Ok`;
  C# `SendLoop.cs:466-468` counts unconditionally (its overload is `void`). Statistics only.
- `tick.rs:110-112` spins on `now_ticks()` with `spin_loop()` between reads; C# `Tick.cs:51-54`
  puts `Thread.SpinWait(20)` between clock reads, so the Rust reads the clock far more often
  during the up-to-2.5 ms spin window.
- `mod.rs:221` spawns the tick thread at default priority; C# `Startup.cs:29` sets
  `ThreadPriority.AboveNormal`, and `Startup.cs:19-23` raises the Windows timer resolution with
  `timeBeginPeriod(1)`. Neither has a Rust counterpart.
- `profiling.rs:113` seeds `LAST_PRINT_TICK` at 0; C# `Profiling.cs:169` seeds it at the current
  timestamp. Enabling the profiler on a process older than 5 s closes a window immediately.
- `send_loop.rs:120` computes the sender rotation in `usize`; C# `SendLoop.cs:130` wraps in
  `uint` first. Different offsets only once `senderRotation + id` exceeds 2³², and the value is
  only a fairness permutation.

## Corners cut

### Send-socket growth is gone, not ported

C# `MaybeGrowSendSockets` (`LoadControl.cs:143-227`) is a full controller: sample the drop rate,
add a socket under sustained pressure, put drop-driven growth on a 30 s trial, declare growth
helpless if the drop rate does not improve by 20 % (`LoadControl.cs:118, 166-180`), and retry once
drops double (`:186-191`). Rust `maybe_grow_send_sockets` (`load_control.rs:94-114`) keeps the
drop sampler and the pressure test, then discards its own constants with
`let _ = (Self::SOCKET_GROW_SETTLE_TICKS, Self::SOCKET_PROBE_WINDOW_TICKS, now_tick);` at
`load_control.rs:99` and ends at a one-shot warning (`:127-135`). `SocketProbeMustImproveBy`,
`_probePending`, `_socketGrowthHelpless` and `_dropRateAtGiveUp` have no counterpart. Defensible
for a QUIC endpoint; it is what leaves D1 with nothing to raise the ceiling.

### Peer-update lease accounting is gone

C# `LoadControl.cs:38-49` fed `BasisCpuBudget.PeerUpdateLease.AddWork(...)` from the transport's
counters and `:56-69` pushed utilisation, the peer worker cap and the socket count back. None of it
survives (`load_control.rs:48-54`), so the two-pool allocator is running on one pool's numbers.

### The pool-load log lost four fields

C# `LoadControl.cs:90-99` prints peer-update workers, peer pass ms against target, and the
effective unreliable queue per peer alongside the send figures; Rust `load_control.rs:60-74` prints
neither. The line exists to tell an operator which pool is hot, and it can now only answer for one.

### One test seam is now vacuous

`test_only_encode_avatar_intervals` (`test_seams.rs:281-290`) implements the reference by calling
`encode_avatar_interval_byte` / `decode_avatar_interval_ms` — the same two functions
`vector_interval_encoding_matches_the_protocol`
(`basis_server_tests/tests/compute/distance_sweep_tests.rs:97-115`) asserts against. The C# seam
(`TestSeams.cs:264-280`) drove the hand-written SIMD transcription, which is what the test existed
to police. No risk is left behind — the transcription is gone (D4) — but the test now proves
nothing and reads as if it does.

### Small dead state

`PoolTuning::last_send_workers` (`parallelism.rs:22`) is written at `parallelism.rs:245` and never
read; the send loop passes `workers` into `note_send_pass_cost` explicitly
(`tick.rs:178`). The C# field it mirrors (`Parallelism.cs:63`) *was* the input
(`Parallelism.cs:335`).

### Compute-backend diagnostics narrowed

C# `BasisComputeBackend.cs:409-443` reports "no `BasisNetworkCompute.dll` beside the server", "the
DLL has no factory type", "no factory method" and unwraps `TargetInvocationException`. The Rust
links `basis_network_compute` directly (`basis_compute_backend.rs:331-342`), so those states cannot
occur and `status()` carries only what the factory returns. A deliberate simplification, listed
because the boot log is less informative when a device is refused.

Not cut: the tick-period / shed-tier / slicing escalation ladder, its hysteresis, the drop-based
second control signal, the widen trial, the learned ceiling and its expiry, the amortised sweep
with its pinned roster, the device verification-and-refusal path, the greedy bundler with its
retry and fill-margin adaptation, the channel flush order and its per-tick id-width swap, the
adaptive keyframe stretch, and the sticky `UsedQualities` mask are all present and match.

## Improvements

- **Torn reads on the join path are fixed.** C# `TryGetJoinSnapshot` (`Quality.cs:18-54`) runs on
  the network thread and reads `subject.Position` (a 12-byte struct) and
  `subject.AvatarMedium`/`AvatarLow`/`AvatarVeryLow` (multi-field structs) with no synchronisation
  while `ProcessMessage` rewrites them on the tick thread — `array` and `DataQualityLevel` can come
  from different frames. Rust stores the position as three atomics
  (`player_state.rs:102`, `:151-163`) and `try_get_join_snapshot` (`mod.rs:285-304`) takes the
  sender lock.
- **The displaced inbound message is recycled.** C# `Inbound.cs:47-52` deliberately leaks the
  previous pending message to the GC because it cannot prove the drain does not still hold it. Rust
  `inbound.rs:47-50` gets the displaced value back from `insert` by move, which *is* that proof, and
  returns it to the pool.
- **The sender/receiver split is compiler-enforced.** C# `PlayerState.cs:94-97` documents "only
  this player's own receive thread writes here" as a comment; `player_state.rs:37-94` splits the
  same state into `SenderWork`, `ReceiverData` and an immutable `SenderFrame`
  (`serialization.rs:239-250`) so the invariant is checked rather than asserted.
- **Panic/exception paths the C# could actually take are closed.**
  `send_loop.rs:147` clamps the cached quality index with `.min(3)` — C# `SendLoop.cs:184` indexes
  `SerializedKeyframe[qi]` directly, and a compute backend that wrote a tier above 3 would throw
  inside a `Parallel.For` body. `distance.rs:73` applies `.max(1)` to the refresh period, where C#
  `Distance.cs:114-116` divides by `EffectiveDistanceIntervalTicks` — a public field
  (`LoadControl.cs:387`, `Distance.cs:50`) that config can set to 0; the Rust setters also clamp
  (`mod.rs:455`, `:461`). `quant_rescale_table.rs:293` guards `b_src == 0`, which C#
  `QuantRescaleTable.cs:361-371` would divide by zero on. `distance.rs:337` uses
  `tick_table.get(..).unwrap_or(0)`.
- **A torn uplink delta is dropped rather than half-applied.** `inbound.rs:146-150` checks the
  result of `deserialize_additional_data` and returns the frame to the pool; C# `Inbound.cs:177-180`
  ignores it and queues whatever was parsed.
- **Config setters clamp.** `set_bsrs_millisecond_default_interval` (`mod.rs:320`) and both
  distance-interval setters take `.max(1)`; the C# equivalents are bare public static fields
  (`Configuration.cs:21`, `LoadControl.cs:387`, `Distance.cs:50`).
- **`parallel_for` degrades instead of failing.** `parallelism.rs:121-126` runs the range inline if
  the pool cannot be built, and logs once (`:105`).

## Verdict

This is a faithful port of a large, heavily tuned module: I read every line of the hot path —
`SendLoop.cs`/`send_loop.rs`, `Distance.cs`/`distance.rs`, `Bundling.cs`/`bundling.rs`,
`Tick.cs`/`tick.rs`, `Parallelism.cs`/`parallelism.rs`, `LoadControl.cs`/`load_control.rs`,
`Serialization.cs` and `MessageProcessing.cs` and their counterparts — function by function, and
found no correctness bug in what a receiver is sent. Distance tiers, interval maths, shed
thresholds and hysteresis, slicing, keyframe/delta triggers, bundle chunking and the flush order
are all bit-for-bit the same policy, and every constant matches. The cold files
(`AvatarQualityRepacker`, `QuantRescaleTable`, `Profiling`, the small data types) I read in full
but compared more quickly, since their tests already pin them.

The one finding that changes production behaviour is D1: nothing in the Rust ever calls
`BasisCpuBudget::set_send_socket_count`, so the send-pool ceiling is stuck at `min(cores, 8)`
where the C# raised it to `8 × bound sockets` on every rebalance. The estimator, the widen trial
and the learned ceiling are all ported correctly and will simply saturate against a ceiling several
times lower than the C#'s on any host with more than 8 cores. Second in importance is the set of
hot-path allocations and atomic-refcount traffic the port introduced (D5, D6, D7) and the weaker
distance vector path (D4) — none of them wrong, all of them spending against budgets the C#
comments were written to defend.
