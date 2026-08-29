# BasisNetworkClientConsole — port diffs

C#: `Basis Server/BasisNetworkClientConsole/BasisNetworkClientConsole/` · Rust: `basis_server/basis_network_client_console/src/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `Program.cs` | `main.rs` | 477 → 456 | ported; identical driver tick, `BASIS_PACKET_LOSS` not applied |
| `Client/ConfigManager.cs` | `client/config_manager.rs` | 320 → 427 | ported; all 33 settings, identical names and defaults |
| `Client/ClientManager.cs` | `client/client_manager.rs` | 347 → 313 | ported; different transport default, different body-fit RNG |
| `Client/MovementSender.cs` | `client/movement_sender.rs` | 772 → 690 | ported except the Opus encoder, which is absent |
| `Client/MessageHandler.cs` | `client/message_handler.rs` | 522 → 512 | ported; same counters and same wire parsing |
| `Client/ErrorHandlers.cs` | `util/error_handlers.rs` | 38 → 15 | ported as a panic hook |
| `Avatar/FakePoseGenerator.cs` | `avatar/fake_pose_generator.rs` | 444 → 326 | ported; same base pose, same idle animation, same encoding |
| `Avatar/AvatarKeyStoreLoader.cs` | `avatar/avatar_key_store_loader.rs` | 114 → 98 | ported |
| `Avatar/AvatarNetworkLoadInformation.cs` | `avatar/basis_avatar_network_load.rs` | 115 → 87 | ported; same wire format |
| `Audio/MicrophoneCapture.cs` | `audio/microphone_capture.rs` | 385 → 58 | **stub** — every method is a no-op, `start` returns false |
| `Audio/VoiceDeliveryStats.cs` | `audio/voice_delivery_stats.rs` | 117 → 123 | ported |
| `Util/Randomizer.cs` | `util/randomizer.rs` | 50 → 45 | ported |
| `Util/NameGenerator.cs` | `util/name_generator.rs` | 46 → 37 | ported; identical word lists |
| `Diagnostics/BundleCaptureSink.cs` | `diagnostics/bundle_capture_sink.rs` | 126 → 109 | ported; decimation now takes a lock |
| `Diagnostics/BasisClientLogger.cs` | `diagnostics/basis_client_logger.rs` | 16 → 16 | ported |
| — | `*/mod.rs` (5 files) | — | module wiring |
| tests | in-file `#[cfg(test)]` | 0 → 9 | the C# project has no tests |

## ClientSimConfig.xml

All 33 settings exist on both sides under the same element names, and every default matches.
C# `ConfigManager.cs:7-78` against Rust `config_manager.rs:60-98`:

| setting | C# default | Rust default | match |
| --- | --- | --- | --- |
| Password | `default_password` | `default_password` | yes |
| Ip | `localhost` | `localhost` | yes |
| Port | 4296 | 4296 | yes |
| ClientCount | 250 | 250 | yes |
| ClientConnectIntervalMs | 1 | 1 | yes |
| AvatarPassword | `default_avatar_password` | `default_avatar_password` | yes |
| AvatarUrl | `http://localhost/avatar` | `http://localhost/avatar` | yes |
| AvatarLoadMode | 1 | 1 | yes |
| UseRandomAvatarFromKeyStore | true | true | yes |
| AvatarKeyStorePath | (empty) | (empty) | yes |
| SimulateRealisticPlatforms | false | false | yes |
| SimulateBodyFit | true | true | yes |
| SpawnRadiusMeters | 40 | 40 | yes |
| SimulateVoice | true | true | yes |
| VoiceRangeMeters | 20 | 20 | yes |
| VoiceParticipantPercent | 60 | 60 | yes |
| VoiceTalkBurstMinMs / MaxMs | 500 / 4000 | 500 / 4000 | yes |
| VoiceSilenceMinMs / MaxMs | 4000 / 40000 | 4000 / 40000 | yes |
| VoiceChorusEnabled | true | true | yes |
| VoiceChorusPercent | 85 | 85 | yes |
| VoiceChorusDurationMinMs / MaxMs | 8000 / 25000 | 8000 / 25000 | yes |
| VoiceChorusIntervalMinMs / MaxMs | 45000 / 180000 | 45000 / 180000 | yes |
| VoiceRecipientRefreshMs | 5000 | 5000 | yes |
| VoiceAudibleTimeoutMs | 6000 | 6000 | yes |
| VoiceFrameMs | 20 | 20 | yes |
| VoiceBitrate | 32000 | 32000 | yes |
| VoiceBytesPerFrame | 60 | 60 | yes |
| VoiceUseSystemMicrophone | false | false | yes |
| VoiceMicrophoneDevice | `CABLE Output` | `CABLE Output` | yes |

The generated default document matches element for element and comment for comment
(`ConfigManager.cs:173-239` / `config_manager.rs:168-224`), including the four settings that share
a comment with their `Min` partner and so are emitted bare. Read order, fallback logging and the
atomic temp-file-then-rename write are the same. Neither side has a `NetworkStackId` element — see
Deviation 1, which is why that matters.

## Deviations

**1. The two load clients speak different transports by default.**
`ClientManager.CreateConfig` (`ClientManager.cs:297-305`) returns a `Configuration` with an empty
`NetworkStackId`, which `BasisNetworkStackRegistry.cs:37` resolves to `LiteNetLibId` — UDP.
`ClientManager::create_config` (`client_manager.rs:274-276`) likewise leaves it empty, and
`basis_server/basis_network_core/src/transport/basis_network_stack_registry.rs:85` resolves an empty
id to `IROH_ID`. `ClientSimConfig.xml` carries no setting for it on either side and no environment
variable overrides it, so this is not configurable from the load client: the C# crowd is LiteNetLib
and the Rust crowd is iroh, always. The Rust server's own default is `mixed` (both stacks,
`basis_network_stack_registry.rs:88`), so the C# client can drive either server, but the Rust client
can only drive the Rust one. Not pinned by a test.

**2. Voice payloads are the wrong size: there is no Opus encoder in the Rust build.**
`MovementSender.cs:279-342` encodes a one-second 180-260 Hz sine sweep into 50 real Opus frames at
`VoiceBitrate` (32000 by default) with complexity 5 and inband FEC, logs the measured average, and
`SendFrame` (`:591`) walks all 50 so consecutive frames differ in size the way speech does.
`movement_sender.rs:428-435` has no encoder at all: `build_opus_frames` unconditionally takes the
C#'s libopus-missing fallback path, logging "Opus encoder unavailable (no native Opus binding in
this build)", and returns a single random buffer of `VoiceBytesPerFrame` (60) bytes.
`send_frame` (`:653`) still computes `(seq + index) % len`, but `len` is 1, so every simulated
client sends the same 60 bytes on every voice frame forever. Frame *rate* is unchanged (both send
`due_frames` per `VoiceFrameMs`), so what differs is payload size and variance: a nominal 80 bytes
per 20 ms frame at the default 32 kbps, VBR, against a flat 60. The C# only reaches this same
fallback when libopus cannot be loaded, and it ships `opus.dll` for win-x64
(`BasisNetworkClientConsole.csproj:29`), so on a Linux benchmark host without a system libopus the
two would in fact agree — but that is an accident of the host, not a property of the port. Not
pinned by a test.

**3. `BASIS_PACKET_LOSS` is accepted and ignored.**
`Program.cs:108-115` sets `SimulatePacketLoss` and `SimulationPacketLossChance` on the shared
LiteNetLib transport config before the clients build their managers, so uplink and downlink frames
both drop and the keyframe-NACK/re-key recovery paths get exercised. `main.rs:117-119` parses the
variable and logs that the iroh transport carries no loss simulation, then does nothing. A run
started with that variable set produces a clean-network measurement in Rust and a lossy one in C#.
Documented in the code. Not pinned.

**4. `MicrophoneCapture` is a stub.**
`MicrophoneCapture.cs` is 385 lines of winmm/waveIn P/Invoke: device enumeration, an 8-buffer ring,
RMS speech gating at 0.0007, Opus encoding of the captured PCM.
`microphone_capture.rs:17-57` returns `false` from `active()` and `start()`, `None` from `try_read`,
`0.0` from `take_peak()`, and an empty device list. Setting `VoiceUseSystemMicrophone` true therefore
logs one error (`:44`) and falls through to synthetic voice, and the `[Mic]` reporter in
`main.rs:138-157` never runs. The C# path is Windows-only anyway, so on a Linux benchmark host both
end up on synthetic voice — but the C# would have worked on a Windows one. Documented in the module
header. Not pinned.

**5. `client.Poll()` / `client.Update(dt)` are no-ops.**
The C# constructs each client with `manualMode: true` (`ClientManager.cs:169`, `:237`) so the driver
thread pumps the socket itself: `DriveSlice` calls `client.Poll(); client.Update(dt)` every 15 ms
(`Program.cs:338-339`), and `MessageHandler.OnReceive` therefore runs on the driver thread.
`basis_server/basis_network_client/src/network_client.rs:100-104` makes both methods empty — the iroh
transport delivers events from its own runtime — so `drive_slice` (`main.rs:321-324`) calls two empty
functions and receive handling runs on transport threads instead. The traffic on the wire is the
same; the CPU accounting inside the load generator is not, which matters because the `[Driver]`
overrun report exists precisely to say whether the harness is the bottleneck. Documented at the
`poll` definition. Not pinned.

**6. A reconnected client advertises a different avatar quality.**
`ClientManager.ReconnectClientAsync` builds its `LocalAvatarSyncMessage` without
`DataQualityLevel` or `AdditionalAvatarDatas` (`ClientManager.cs:228-233`), so the reconnect
`ReadyMessage` claims quality 0 (`VeryLow`) while carrying a High-sized array; the initial connect
sets `High` explicitly (`:158-166`). The Rust `connect` is shared by both paths and always sets
`BitQuality::High as u8` (`client_manager.rs:206`). This changes what the server records for a peer
after one of the random reconnects. Arguably the C# is the one that is wrong, but the port did not
reproduce it. Not pinned.

**7. Body-fit scales come from a different random stream.**
`ClientManager.cs:84-88` seeds `new Random(unchecked(clientIndex * 8663) ^ 0x5eed)`;
`client_manager.rs:132-133` seeds `StdRng::seed_from_u64(seed as u32 as u64)`. Both are deterministic
per index and both stay inside the same band (arm `1 ± 0.15`, leg/torso `1 ∓ 0.12`), but client `i`
gets different numbers on the two sides. Payload size and shape are identical, so the load is the
same; a byte-for-byte comparison of two runs is not.

**8. Config XML lookup is namespace-sensitive.**
`ConfigManager.cs:81-82` matches children by `e.Name.LocalName`, so a `ClientSimConfig.xml` written
with a default or prefixed namespace still reads. `config_manager.rs:241` and `:279` compare the raw
qualified name, so a namespaced document would silently fall back to every default. Related: the
startup line `Root element: {root} | Namespace: ''` (`config_manager.rs:150`) hardcodes the empty
namespace where `ConfigManager.cs:274` printed the real one. Not pinned.

**9. `BundleCaptureSink` decimation now takes the lock.**
`BundleCaptureSink.cs:85-86` decimates with one `Interlocked.Increment` *before* the lock,
deliberately: "so the common case is one interlocked increment". `bundle_capture_sink.rs:67-69`
carries that comment but locks `GATE` first, to read `every_nth` out of the capture struct, then
does the atomic. Every driver thread therefore serialises on one mutex per received bundle whenever
`BASIS_BUNDLE_CAPTURE` is set. Only affects capture runs. Not pinned.

**10. Two small observer differences.**
(a) `MessageHandler.cs:70` drops any packet whose `peer.Id != 0`; `message_handler.rs:120` has no
such guard, on the stated grounds that a client has exactly one connection. (b) The face-counter
monotonicity check differs: `MessageHandler.cs:393` treats `counter <= prev` as a violation, so a
*duplicate* counter is reported; `message_handler.rs:410` adds `prev != counter`, so a duplicate is
not. Only reachable under `BASIS_EMIT_FACE`. (c) `reader.Recycle()` (`MessageHandler.cs:145`, `:422`)
returns the reader to LiteNetLib's pool; the Rust reader is owned and has no equivalent.

**11. Floating-point evaluation order in the pose animation.**
`FakePoseGenerator.cs` computes each animation angle as `MathF.Sin((float)(t * f * TwoPi + p))` —
the phase is added in `double` and the sum narrowed once. `fake_pose_generator.rs:261` narrows the
time term first and adds the phase in `f32`. Identical to well under a quantization step at any
plausible run length; noted only because the two will not produce bit-identical packets.

## Simulated behaviour, side by side

Everything below matched on inspection and is listed so the "checked" set is explicit.

* **Driver loop.** 15 ms tick, one worker per CPU core over a contiguous slice, per-worker phase
  offset of `MovementIntervalMs * w / workerCount`, movement every 90 ms, voice catch-up capped at 5
  frames, the amortized recipient sweep carrying fractional debt and clamped to the slice size, and
  the overrun/peak accounting. `Program.cs:286-459` / `main.rs:280-433`, line for line.
* **Connect ramp.** One client at a time, `ClientConnectIntervalMs` (default 1 ms) between them, a
  fresh DID identity and generated name each, `LocalAvatarIndex` 0 on first connect and 1 on
  reconnect. `ClientManager.cs:131-189` / `client_manager.rs:169-224`.
* **Movement.** Spawn uniform by area over `SpawnRadiusMeters` with `y = 1 ± 0.1`, per-tick random
  walk of ±0.25 m per axis, per-player phase offset, `BASIS_FACE_SPACING` pinning. `Randomizer.cs`
  / `randomizer.rs`, `MovementSender.cs:89-101` / `movement_sender.rs:121-129`.
* **Pose payload.** Same 21 wire bone slots with the same base standing pose in degrees, the same
  ten finger curl/splay channels at the same amplitudes and rates, the same hips sway/tilt, the same
  identity-quaternion write for the hips local-rotation slot, the same smallest-three and restricted
  encodings. `FakePoseGenerator.cs` / `fake_pose_generator.rs`.
* **Uplink protocol.** Keyframe every 500 ms on the quality channel, v42 dirty-mask deltas on
  `DeltaAvatarChannel` in between, fall back to a keyframe when the delta is not smaller, re-key on a
  server NACK, sequence and baseline-sequence bookkeeping, and the odd-channel selection when
  additional data rides along. `MovementSender.cs:628-735` / `movement_sender.rs:222-330`.
* **Voice model.** `VoiceParticipantPercent` of the crowd ever talks; burst/silence alternation with
  randomised durations; the global chorus with the same double-checked scheduling and the same
  "don't open the run mid-song" first pass; recipient lists built from live positions inside
  `VoiceRangeMeters` plus anyone heard at a near quality tier, aged out at `VoiceAudibleTimeoutMs`,
  sorted, published only on change, and switched to the large channel past 255 entries; silence-run
  accounting capped at 255. `MovementSender.cs:203-626` / `movement_sender.rs:333-690`.
* **Avatar payloads.** Same `BasisAvatarNetworkLoad` wire format, same keystore filtering
  (`Mode == 0`, non-blank `Url`, `LoadMode` forced to 0 for http/https), same random pick per client,
  same platform table and the same `Headless` default.
* **Reported statistics.** `[VOICE]`, `[FaceObserver]`, `[Mic]`, `[Driver]` and `[Fairness]` lines
  are byte-identical in format and are emitted on the same 5 s / 5 s / 5 s / 10 s cadences.
  `Program.cs:119-194` / `main.rs:123-174`, `MessageHandler.cs:489-519` /
  `message_handler.rs:488-511`, `VoiceDeliveryStats.cs:110-115` / `voice_delivery_stats.rs:96-98`.

## Corners cut

* Opus encoding (Deviation 2) — the single largest gap, because voice is a large share of the load
  the harness exists to generate.
* Microphone capture (Deviation 4) — the whole file.
* Packet-loss simulation (Deviation 3).
* `AppDomain.UnhandledException` becomes a panic hook and `TaskScheduler.UnobservedTaskException`
  has no counterpart (`ErrorHandlers.cs:27-36` / `error_handlers.rs:9-14`); the panic hook is set
  process-wide with `set_hook`, replacing anything installed earlier.
* `ProcessExit` as a shutdown backstop (`Program.cs:80`) is gone; Rust relies on the signal handlers
  and the stop watcher, and `main` parks forever (`main.rs:180-182`) rather than returning.

## Improvements

* **The connect ramp can be cancelled.** `ClientManager.cs:20` creates a `CancellationTokenSource`,
  passes its token to the ramp delay (`:186`) and never cancels it, so a stop during the ramp waits
  out every remaining client. `client_manager.rs:172-174` checks a `cancelled` flag that
  `stop_clients` sets (`:258`), so a stop during a 4000-client ramp takes effect at once.
* **Bounds are checked on every packet write.** `write_identity_quaternion` (`movement_sender.rs:198`),
  `write_scale_ushort` (`:211`), `write_bone_rotations` (`fake_pose_generator.rs:143-145`) and
  `write_compressed_quat` (`:208`) return instead of writing out of range; the C# equivalents index
  raw arrays and one of them uses `unsafe` pointer writes (`MovementSender.cs:743-754`).
* **Oversized values cannot silently truncate.** `basis_avatar_network_load.rs:45` clamps a string to
  65535 bytes where `AvatarNetworkLoadInformation.cs:77` casts the length to `ushort`;
  `bundle_capture_sink.rs:64` rejects a body over 65535 bytes where `BundleCaptureSink.cs:96-97`
  writes a truncated 16-bit length.
* **Reporters stop promptly.** `spawn_reporter` (`main.rs:259-272`) re-checks `is_running` after the
  sleep, so no report is emitted after shutdown begins; the C# `Task.Delay` loops log one more line.
* **The reconnect loop cannot outlive the run** (`main.rs:445-447`) and does nothing when
  `ClientCount` is 0 (`:437-439`); `Program.cs:461-475` would call `Random.Shared.Next(0, 0)` on an
  empty population.
* **Config values are read from an immutable snapshot.** `ConfigManager::current()` is one atomic
  load of an `Arc` (`config_manager.rs:109-111`) and the driver takes one per tick
  (`main.rs:338`), against 33 mutable statics read from every thread in the C#.
* **Tests exist.** Nine, covering the default-document round trip and fallback behaviour
  (`config_manager.rs:400-426`), the spawn disc (`randomizer.rs:36-44`), the avatar-load wire format
  including the pre-`VersionTag` record (`basis_avatar_network_load.rs:70-86`), that the pose region
  changes between sends and never writes past itself (`fake_pose_generator.rs:304-325`), the
  sequence-gap/wrap/reorder arithmetic (`voice_delivery_stats.rs:106-122`) and keystore filtering
  (`avatar_key_store_loader.rs:84-97`). The C# project has none.

## Verdict

Structurally a close port. The driver tick, the connect ramp, the movement model, the pose bit
layout, the uplink keyframe/delta protocol, the whole voice burst-and-chorus model, the recipient
publishing and every reported statistic line up function for function, and `ClientSimConfig.xml` is
identical in names, defaults, generated comments and read order. Nine tests now cover parts of it
that the C# never tested.

Three gaps decide whether the two generate comparable load. Two are conditional: `BASIS_PACKET_LOSS`
(ignored in Rust) and the microphone path (stubbed) only matter to runs that use them. The third is
not: the Rust build has no Opus encoder, so every simulated voice frame is a fixed 60 bytes instead
of a `VoiceBitrate`-sized VBR frame — with voice on by default and 20 ms frames, that is a systematic
understatement of voice bytes on the wire whenever the C# side has libopus available. And the two
clients default to different transports (LiteNetLib against iroh), which is a deliberate design
choice for the port but means a benchmark that runs both is comparing two protocols as well as two
servers. Both belong in the benchmark's own notes; the Opus gap is the one worth closing.
