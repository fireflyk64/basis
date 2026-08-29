# configuration — port diffs

C#: `BasisNetworkCore/Configuration/` · Rust: `basis_network_core/src/configuration/`

Every file on both sides was read, and the comparison was checked against a live experiment: each
server was started from a clean directory so it wrote its own default `config/config.xml` and
transport sidecars, and the two trees were then diffed and cross-loaded (the C# server booted on a
Rust-written config, and the Rust server on a C#-written one).

## File map

| C# | Rust | lines (C#→Rust) | status |
|---|---|---|---|
| `BasisServerConfiguration.cs` | `basis_server_configuration.rs` | 621 → 287 | ported; 93 settings identical, 1 added |
| `LNLTransportConfig.cs` | `lnl_transport_config.rs` | 212 → 80 | ported; 30 settings identical, 5 added |
| `BasisTransportConfigStore.cs` | `basis_transport_config_store.rs` | 200 → 320 | ported |
| `BasisConfigXmlDocs.cs` | `basis_config_xml_docs.rs` | 271 → 375 | ported; doc text byte-identical bar one comment |
| `BasisTuningProfile.cs` | `basis_tuning_profile.rs` | 323 → 457 | ported |
| `BasisPopulationScale.cs` | `basis_population_scale.rs` | 267 → 175 | ported; reliable-byte budget added |
| — | `iroh_transport_config.rs` | — → 52 | Rust only; the iroh stack has no C# sidecar |
| — | `mod.rs` | — → 231 | Rust only; the `basis_xml_config!` field table that stands in for C# reflection |

The settings' prose lives in the doc-comment tables in both ports, so the config structs shrink
(`BasisServerConfiguration.cs` is 93 fields plus 400 lines of `<summary>`) while the docs file and
the store grow.

## Setting map

The C# column is the value the C# server wrote into a fresh `config.xml`; the Rust column is what
the Rust server wrote from an equally fresh directory — measured, not read off the source.

### `Configuration` (config.xml)

| Setting | C# default | Rust default | Match |
|---|---|---|---|
| ConfigVersion | `13` | `14` | **differs** |
| PeerLimit | `65535` | `65535` | same |
| SetPort | `4296` | `4296` | same |
| ServerName | `Basis Server` | `Basis Server` | same |
| ServerMotd | (empty) | (empty) | same |
| EnableStatistics | `true` | `true` | same |
| HasFileSupport | `true` | `true` | same |
| HealthCheckHost | `localhost` | `localhost` | same |
| HealthCheckPort | `10666` | `10666` | same |
| HealthPath | `/health` | `/health` | same |
| HealthIncludeBSRProfiling | `false` | `false` | same |
| IdleMemoryReclaimEnabled | `true` | `true` | same |
| IdleMemoryReclaimSettleSeconds | `30` | `30` | same |
| IdleMemoryReclaimMinimumPeak | `8` | `8` | same |
| BSRSMillisecondDefaultInterval | `50` | `50` | same |
| BSRBaseMultiplier | `1` | `1` | same |
| BSRSIncreaseRate | `0.005` | `0.005` | same |
| BSRSlowestSendRate | `2.55` | `2.55` | same |
| DistanceUpdateIntervalTicks | `125` | `125` | same |
| EnableComputeOffload | `true` | `true` | same |
| ComputeDevice | (empty) | (empty) | same |
| ComputeDistanceUpdateIntervalTicks | `32` | `32` | same |
| HighQualityDistance | `10` | `10` | same |
| MediumQualityDistance | `20` | `20` | same |
| LowQualityDistance | `40` | `40` | same |
| OverrideAutoDiscoveryOfIpv | `false` | `false` | same |
| IPv4Address | `0.0.0.0` | `0.0.0.0` | same |
| IPv6Address | `::` | `::` | same |
| Password | `default_password` | `default_password` | same |
| UseAuth | `true` | `true` | same |
| UseAuthIdentity | `true` | `true` | same |
| NetworkStackId | (empty) | (empty) | same |
| BasisUserRestrictionMode | `Normal` | `Normal` | same |
| HowManyDuplicateAuthCanExist | `2` | `2` | same |
| AuthValidationTimeOutMiliseconds | `9000` | `9000` | same |
| EnableConsole | `true` | `true` | same |
| EnableAvatarBundleCompression | `true` | `true` | same |
| AvatarBundleMinMessages | `2` | `2` | same |
| AvatarBundleMinBytes | `128` | `128` | same |
| EnableAvatarBundleZstd | `true` | `true` | same |
| AvatarBundleZstdDeltaBundles | `false` | `false` | same |
| AvatarBundleZstdLevel | `-2` | `-2` | same |
| AvatarBundleZstdMaxShedTier | `1` | `1` | same |
| EnableAvatarDeltaCompression | `true` | `true` | same |
| AvatarDeltaKeyframeIntervalMs | `500` | `500` | same |
| AvatarDeltaKeyframeMaxIntervalMs | `2000` | `2000` | same |
| StripAdditionalDataAtLowQuality | `true` | `true` | same |
| EnableUplinkAvatarDelta | `true` | `true` | same |
| ImageCacheEnabled | `true` | `true` | same |
| ImageCacheMaxMegabytes | `512` | `512` | same |
| ImageCacheMinimumPerOwnerMegabytes | `32` | `32` | same |
| ImageShareEgressMegabitsPerSecond | `200` | `200` | same |
| ImageShareDownloadMegabitsPerSecond | `200` | `200` | same |
| ImageShareEgressEnforcementPercent | `150` | `150` | same |
| ImagePickupRangeMeters | `64` | `64` | same |
| EnableBSRProfiling | `false` | `false` | same |
| LogConnectionHandshake | `false` | `false` | same |
| BSRMaxDegreeOfParallelism | `0` | `0` | same |
| BSRSendPhaseBudgetPercent | `0` | `0` | same |
| BSRMaxSliceCount | `0` | `0` | same |
| VoiceFrameDurationMs | `20` | `20` | same |
| DisallowHeadless | `false` | `false` | same |
| AvatarsLocked | `false` | `false` | same |
| PropsLocked | `false` | `false` | same |
| WorldsLocked | `true` | `true` | same |
| ServersLocked | `false` | `false` | same |
| ThirdPersonDisabled | `false` | `false` | same |
| AdditionalAvatarDataLock | `false` | `false` | same |
| CameraMetadataDisallowMask | `0` | `0` | same |
| CrashReportingEnabled | `true` | `true` | same |
| MaxMicrophoneRangeMeters | `25` | `25` | same |
| MaxHearingRangeMeters | `25` | `25` | same |
| MinAvatarEyeHeightMeters | `0.1` | `0.1` | same |
| MaxAvatarEyeHeightMeters | `100` | `100` | same |
| MaxContentSpheresPerPlayer | `32` | `32` | same |
| MaxNetworkIdsPerPlayer | `32768` | `32768` | same |
| MaxOwnedObjectsPerPlayer | — | `262144` | Rust only |
| MaxLoadedResourcesPerPlayer | `16384` | `16384` | same |
| MaxSceneRelayMegabitsPerSecondPerPlayer | `0` | `0` | same |
| PlayspaceMoverLocked | `false` | `false` | same |
| DirectConnectLocked | `false` | `false` | same |
| CilboxLocked | `false` | `false` | same |
| ImagesLocked | `false` | `false` | same |
| EndEffectorIKDisabled | `false` | `false` | same |
| TextChatLocked | `false` | `false` | same |
| VoiceChatLocked | `false` | `false` | same |
| MediaPlayerLocked | `false` | `false` | same |
| CameraCaptureLocked | `false` | `false` | same |
| PropGrabbingLocked | `false` | `false` | same |
| SafeDisplayNamesForced | `false` | `false` | same |
| ApiEnabled | `false` | `false` | same |
| ApiHost | `localhost` | `localhost` | same |
| ApiPort | `10667` | `10667` | same |
| ApiKey | (empty) | (empty) | same |


### `LNLTransportConfig` (config/transports/litenetlib.xml)

| Setting | C# default | Rust default | Match |
|---|---|---|---|
| ConfigVersion | `10` | `11` | **differs** |
| UseNativeSockets | `true` | `true` | same |
| NatPunchEnabled | `true` | `true` | same |
| NatPortPredictionRange | `32` | `32` | same |
| PingInterval | `1500` | `1500` | same |
| DisconnectTimeout | `30000` | `30000` | same |
| SimulatePacketLoss | `false` | `false` | same |
| SimulateLatency | `false` | `false` | same |
| SimulationPacketLossChance | `10` | `10` | same |
| SimulationMinLatency | `50` | `50` | same |
| SimulationMaxLatency | `150` | `150` | same |
| ReconnectDelay | `500` | `500` | same |
| MaxConnectAttempts | `10` | `10` | same |
| ReuseAddresss | `false` | `false` | same |
| DontRoute | `false` | `false` | same |
| IPv6Enabled | `true` | `true` | same |
| MtuOverride | `0` | `0` | same |
| MtuDiscovery | `true` | `true` | same |
| DisconnectOnUnreachable | `false` | `false` | same |
| AllowPeerAddressChange | `true` | `true` | same |
| MultiSocketCount | `1` | `1` | same |
| MaxSendSockets | `0` | `0` | same |
| PacketPoolSizePerPeer | `48` | `48` | same |
| PacketPoolSizeMax | `0` | `0` | same |
| MergeHoldMs | `3` | `3` | same |
| CompactMerged | `true` | `true` | same |
| PeerUpdateParallelism | `0` | `0` | same |
| PeerUpdatePeersPerWorker | `0` | `0` | same |
| MaxUnreliableQueuePerPeer | `0` | `0` | same |
| MaxPriorityUnreliableQueuePerPeer | `0` | `0` | same |
| MaxReliableQueueBytesPerPeer | — | `0` | Rust only |
| ReliableQueueGraceMs | — | `5000` | Rust only |
| MaxFragmentBytesPerPeer | — | `0` | Rust only |
| MaxPendingRequests | — | `0` | Rust only |
| MaxRejectPeers | — | `0` | Rust only |


### `IrohTransportConfig` (config/transports/iroh.xml) — no C# counterpart

| Setting | C# default | Rust default | Match |
|---|---|---|---|
| ConfigVersion | — | `3` | Rust only |
| Port | — | `0` | Rust only |
| RelayMode | — | `default` | Rust only |
| RelayUrls | — | (empty) | Rust only |
| SecretKeyFile | — | `iroh-secret.key` | Rust only |
| PublishAddress | — | `false` | Rust only |
| IdleTimeoutMs | — | `30000` | Rust only |
| KeepAliveIntervalMs | — | `0` | Rust only |
| MaxDatagramQueuePerPeer | — | `0` | Rust only |
| MaxPriorityDatagramQueuePerPeer | — | `0` | Rust only |
| MaxReliableQueueBytesPerPeer | — | `0` | Rust only |
| ReliableQueueGraceMs | — | `5000` | Rust only |
| SendWindowBytes | — | `0` | Rust only |
| ReceiveWindowBytes | — | `0` | Rust only |
| MaxPendingHandshakes | — | `0` | Rust only |
| TokioWorkerThreads | — | `0` | Rust only |

### `BasisTuningProfile` (config/tuning-profile.xml)

| Setting | C# default | Rust default | Match |
|---|---|---|---|
| `CurrentVersion` (constant) | `1` | `1` | same |
| `FileName` (constant) | `tuning-profile.xml` | `tuning-profile.xml` | same |
| ProfileVersion | `1` (`CurrentVersion`) | `1` (`CURRENT_VERSION`) | same |
| GeneratedUtc | `""` | `""` | same |
| GeneratedBy | `""` | `""` | same |
| Machine | `""` | `""` | same |
| MachineDetail | `""` | `""` | same |
| DesignPlayers | `0` | `0` | same |
| ApplyToAnyMachine | `false` | `false` | same |
| AppliedUtc | `""` | `""` | same |
| Settings | empty list | empty list | same |
| `Setting` attributes | Name, Value, Stack, Evidence + `<Rationale>` element | identical | same |

### `BasisPopulationScale` (constants, not file settings)

| Constant | C# | Rust | Match |
|---|---|---|---|
| UnreliableQueueMemoryShare | `0.10` | `0.10` | same |
| PriorityQueueMemoryShare | `0.10` | `0.10` | same |
| PacketPoolMemoryShare | `0.22` (sum + 0.02) | `0.22` (sum + 0.02) | same |
| MinUnreliableQueuePerPeer | `512` | `512` | same |
| MaxUnreliableQueuePerPeer | `8192` | `8192` | same |
| MinPriorityQueuePerPeer | `1024` | `1024` | same |
| MaxPriorityQueuePerPeer | `8192` | `8192` | same |
| ApproxPacketBytes | `1432` | `1432` | same |
| memory fallback when the runtime will not answer | `4 GiB` | `4 GiB` | same |
| packet-pool floor / slice cap band / receivers per slice | `65536` / `32..256` / `64` | same | same |
| ReliableQueueMemoryShare | — | `0.10` | Rust only |
| MinReliableQueueBytesPerPeer | — | `256 KiB` | Rust only |
| MaxReliableQueueBytesPerPeer | — | `8 MiB` | Rust only |

### Totals

123 settings exist on both sides and 121 have identical defaults, in identical XML order, under
identical element names. The two that differ are both the schema-version stamp (`ConfigVersion`
13→14 in config.xml, 10→11 in litenetlib.xml). 22 settings exist only in Rust: 1 on
`Configuration`, 5 on `LNLTransportConfig`, and the whole 16-setting `IrohTransportConfig`. No
setting exists only in C#.

## Deviations

**1. The schema version stamp is one ahead on both files.** C#
`BasisServerConfiguration.cs:36` stamps 13, Rust `basis_server_configuration.rs:125` stamps 14;
C# `LNLTransportConfig.cs:9` stamps 10, Rust `lnl_transport_config.rs:58` stamps 11. The bumps pay
for the settings the Rust added, and the consequences are asymmetric and worth knowing:

* Rust reading a C#-written config always rewrites it once ("is from an older version; adding
  missing settings"), preserving every value already there. Verified: a config with `SetPort`
  7777, a custom `ServerName` and a `ServerMotd` came back untouched with the new settings
  appended.
* C# reading a Rust-written config leaves both files completely untouched — the version is not
  behind and nothing it knows about is missing — and it ignores the unknown elements without
  complaint. Verified by booting the C# server on the Rust tree; `config.xml` was byte-identical
  afterwards and `iroh.xml` was left alone.
* But if that C# server ever *saves* (the admin panel), `WriteXml` at
  `BasisServerConfiguration.cs:488-502` serializes only its own fields: the operator's
  `MaxOwnedObjectsPerPlayer` and the five LNL bounds are dropped and the stamp goes back to 13.
  The next Rust boot re-adds them at their defaults, not at the values the operator chose.

Pinned by `config_and_registry_tests.rs:172` (`CURRENT_CONFIG_VERSION == 14`) and `:312`
(`LNLTransportConfig::CURRENT_CONFIG_VERSION == 11`); nothing pins the cross-server behaviour.

**2. Rust trims whitespace from string settings; C# preserves it.** Rust sets
`trim_text(true)` at `basis_config_xml_docs.rs:110` and trims again at `:143`; the C# hands the
element to `XmlSerializer` (`BasisConfigXmlDocs.cs:47-57`), which keeps the text verbatim.
Measured on both servers with `<Password>  spaced pass  </Password>`: C# kept the spaces, Rust
loaded `spaced pass` and rewrote the file that way. So a password, server name, MOTD, API key or
compute-device selector with deliberate surrounding whitespace means a different thing on the two
servers, and the Rust silently normalises the file. Not pinned: the round-trip test's mutator
(`config_and_registry_tests.rs:53-68`) appends `_x` to strings and never uses whitespace.

**3. Empty elements are written in the long form.** Rust writes `<ServerMotd></ServerMotd>`
(`basis_config_xml_docs.rs:96-99`), C# writes `<ServerMotd />`. Both readers accept either — Rust
handles the self-closing form at `basis_config_xml_docs.rs:146-151` — so this is cosmetic, but it
does mean a file is never byte-identical after a round trip through the other server. It is one of
only four differences in the whole default `config.xml`.

**4. An empty `NetworkStackId` selects a different transport.** C#
`BasisNetworkStackRegistry.cs:37` makes the default `litenetlib`; Rust
`basis_network_stack_registry.rs:89` makes the server default `mixed` (iroh and LiteNetLib side by
side), used at `network_server.rs:467-471`, and `:86` makes the store/client default `iroh`. This
is deliberate and the doc comment says so (`basis_config_xml_docs.rs:260` versus
`BasisConfigXmlDocs.cs:178`) — it is the one doc comment whose text differs between the ports —
but it is exactly the case an operator moving `config.xml` between servers will hit: the same file
means "LiteNetLib only" on one server and "both stacks" on the other.

**5. That default leaks into the transport store, latently.** `BasisTransportConfigStore.Get<T>("")`
routed an empty id to `litenetlib` (`BasisTransportConfigStore.cs:52`); the Rust routes it to
`DEFAULT_ID`, which is now `iroh` (`basis_transport_config_store.rs:132`). Because the `iroh` slot
holds an `IrohTransportConfig`, `get::<LNLTransportConfig>("")` fails its downcast and returns —
and stores — a fresh default, discarding what `litenetlib.xml` said. No production call site passes
an empty id (every caller names the stack: `network_server.rs:307`, `iroh_network_impl.rs:629`,
`net_manager.rs:1002`, `basis_iroh_ffi/src/lib.rs:268`), so nothing is broken today. The C# test
asserted reference identity with the loaded litenetlib config
(`ConfigAndRegistryTests.cs:452-460`); its Rust counterpart
(`config_and_registry_tests.rs:446-453`) now compares two fresh defaults and would not catch this.

**6. The store hands out clones, not the live object.** `BasisTransportConfigStore.cs:50-63`
returned the stored instance, so a caller could mutate it in place; `basis_transport_config_store.rs:131-145`
returns a clone and mutation goes through `with_mut` / `with_object_mut` (`:156-184`). The header
comment at `:103-108` states the contract. Every caller was checked and uses the right one; the
re-registration test (`config_and_registry_tests.rs:470-484`) pins that a mutation survives.

**7. The tuning profile is more forgiving in two fields.** `basis_tuning_profile.rs:232` reads
`DesignPlayers` with `unwrap_or(0)` and `:233` reads `ApplyToAnyMachine` as
`eq_ignore_ascii_case("true")`. The C# fields (`BasisTuningProfile.cs:80,86`) go through
`XmlSerializer`, which rejects the whole file on a non-numeric `DesignPlayers` and accepts `1`/`0`
as booleans. So a hand-edited `<ApplyToAnyMachine>1</ApplyToAnyMachine>` applies the profile on
the C# server and refuses it on the Rust one. `ProfileVersion` is strict on both and is the only
one pinned (`configuration_errors.rs:133-140`).

**8. `BasisUserRestrictionMode` also accepts its numeric form.** `basis_user_restriction_mode.rs:43`
falls back to parsing the value as a byte, where the C# `XmlSerializer` accepts only the member
names (`BasisUserRestrictionMode.cs:6-12`). The written form is identical (`Normal`), so this only
widens what a hand-edited file may say.

**9. The machine fingerprint and the memory figure come from different APIs.**
`BasisTuningProfile.cs:96-103` uses `RuntimeInformation.OSArchitecture` and
`Environment.ProcessorCount`; `basis_tuning_profile.rs:88-104` uses `std::env::consts` plus
`available_parallelism`, mapping `x86_64→x64` and `aarch64→arm64` so the string shape matches.
`BasisPopulationScale.cs:101-132` reads `GC.GetGCMemoryInfo().TotalAvailableMemoryBytes`;
`basis_population_scale.rs:39-69` reads the cgroup v2/v1 limit file and falls back to `sysinfo`.
Same intent and the same 4 GiB fallback, but a container where the two disagree would resolve
different queue ceilings, or refuse a tuning profile the other accepts. Not pinned (the Rust tests
pin memory through `override_available_memory_for_tests`).

## Corners cut

* The version-history comment block (`BasisServerConfiguration.cs:22-35`, one line per bump from 5
  to 13) is reduced to the single "13:" line in `basis_server_configuration.rs:122-125`, and the
  reason for 14 lives in a test comment (`config_and_registry_tests.rs:174-176`) rather than beside
  the constant. No behaviour change; the audit trail is thinner where it matters most.
* The XML layer is a hand-rolled reader/writer for a flat list of scalar elements
  (`basis_config_xml_docs.rs:68-157`), not a general serializer. That covers every config type that
  exists, but `BasisTuningProfile` has attributes and a nested list, so it needs its own
  reader/writer (`basis_tuning_profile.rs:135-277`) where the C# reused one `XmlSerializer`.
* Environment overrides do not recurse into nested config objects; the C# did
  (`BasisServerConfiguration.cs:579-583`), and `basis_server_configuration.rs:241-243` says why it
  does not need to — no config type has a nested object. True today, silent if one is ever added.
* `Configuration::get_default_path` falls back to the working directory when the executable path
  cannot be read (`basis_server_configuration.rs:187-192`), where the C# always had
  `AppDomain.BaseDirectory` (`BasisServerConfiguration.cs:508-511`).
* Nothing else is stubbed or omitted. Every public C# member has a counterpart, including
  `RequiresRestart` (the same 15 field names, in the same order),
  `AppliesToNewJoinsOnly`, `IsSecretFieldName`, `StampVersion`, `ReadVersion`, `NeedsUpgrade`,
  `IsMissingAnyField` and the `IBasisTransportConfigMigration` hook (routed through
  `mod.rs:216-231`, with the LiteNetLib version-8 migration reproduced value for value at
  `lnl_transport_config.rs:60-79`).

## Improvements

* **Failures are typed and name the field.** `ConfigXmlError` (`basis_config_xml_docs.rs:33-47`)
  and `ConfigFieldError` (`mod.rs:43-49`) distinguish malformed XML, a missing root, the wrong
  root, an unwritable path and a bad value — and a bad value carries the field, the text and the
  reason. The C# lets an `XmlSerializer` `InvalidOperationException` escape `LoadFromXml`
  (`BasisServerConfiguration.cs:448`). The whole of `basis_network_core/tests/configuration_errors.rs`
  (188 lines) pins this, including that a corrupt file is reported and left for the operator to fix
  rather than replaced (`:122-129`).
* **Bounds the C# never had, and they are wired.** `MaxReliableQueueBytesPerPeer`,
  `ReliableQueueGraceMs`, `MaxFragmentBytesPerPeer`, `MaxPendingRequests` and `MaxRejectPeers`
  (`lnl_transport_config.rs:44-53`) are read at `net_manager.rs:108-110` and `net_peer.rs:1267`;
  the iroh equivalents plus `SendWindowBytes`, `ReceiveWindowBytes` and `MaxPendingHandshakes`
  (`iroh_transport_config.rs:105-113`) at `iroh_network_impl.rs:899-903`. Each defaults to 0
  meaning "resolve it", and the reliable one resolves through a new memory-share function
  (`basis_population_scale.rs:102-109`) with its own test (`:152-174`).
* **`MaxOwnedObjectsPerPlayer`** (`basis_server_configuration.rs:90-96`) bounds the ownership
  table, whose keys are client-chosen strings that are only released on disconnect
  (`basis_network_ownership.rs:152`). The C# has no ceiling on it at all.
* **Registration is type-checked.** `register_type::<T>` (`basis_transport_config_store.rs:116`)
  cannot be handed something that is not a config; the C# took a `System.Type` and an
  `Activator.CreateInstance` (`BasisTransportConfigStore.cs:36-48`).
* **The static store can be emptied for tests** (`basis_transport_config_store.rs:316-319`); the C#
  store had no such seam, so its tests worked around it with per-test random stack ids.
* The test suite is a superset: every C# config test has a Rust counterpart, plus a
  serialize/deserialize round-trip through the doc comments and a malformed-input case
  (`config_and_registry_tests.rs:950-1050`).

## Verdict

This is a faithful port at the level that matters for an operator: 121 of the 123 shared settings
have byte-identical defaults, in identical order, with identical element names and — bar one
comment that had to change because the Rust ships iroh — byte-identical doc comments, which a
whole-file diff of two freshly generated `config.xml` files confirms. The two differences are the
deliberate version-stamp bumps that pay for the 22 settings the C# does not have: the iroh
sidecar, and a set of per-peer bounds. Both servers read the other's file without error. Two
things deserve attention before a config file is moved between servers: an empty `NetworkStackId` selects LiteNetLib on the C# server and the
mixed stack on the Rust one, and the Rust trims surrounding whitespace out of string settings —
including `Password` — where the C# keeps it. The empty-id path in the transport store
(deviation 5) is worth closing before something calls it.
