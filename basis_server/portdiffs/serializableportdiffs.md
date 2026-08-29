# serializable — port diffs

C#: `BasisNetworkCore/Serializable/` · Rust: `basis_network_core/src/serializable/`

The C# spreads 45 files over nine folders; the Rust collapses each folder to one module file, matching
the way every struct was a member of the single `SerializableBasis` partial class. Both sides total
3858 (C#) and 3086 (Rust) lines.

Throughout, "faithful" means *wire-identical and behaviourally equivalent apart from the module-wide
contract change*: every C# `Deserialize` that returned `void`/`bool` and threw or read past the end of
the buffer is a Rust `deserialize` returning `NetResult`. That change is declared once in
`serializable/mod.rs:5-8` and applies to every message; it is called out per-message under
**Deviations** only where the C# had an explicit guard whose *behaviour* (not just its return type)
changed.

## File map

| C# | Rust | lines (C#→Rust) | status |
|---|---|---|---|
| `Audio/AudioSegmentDataMessage.cs` | `audio.rs:6-44` | 49→39 | faithful |
| `Audio/ServerAudioSegmentMessage.cs` | `audio.rs:46-74` | 29→29 | faithful |
| `Audio/VoiceReceiversMessage.cs` | `audio.rs:76-182` | 158→107 | deviates |
| `Avatar/AdditionalAvatarData.cs` | `avatar.rs:8-75` | 73→68 | faithful |
| `Avatar/AvatarDataMessage.cs` | `avatar.rs:77-136` | 104→60 | faithful |
| `Avatar/AvatarLoadDataMessage.cs` | `avatar.rs:138-174` | 67→37 | faithful |
| `Avatar/BasisAvatarCloneRequest.cs` | `avatar.rs:176-206` | 29→31 | faithful |
| `Avatar/ClientAvatarChangeMessage.cs` | `avatar.rs:208-334` | 166→127 | faithful |
| `Avatar/LocalAvatarSyncMessage.cs` | `avatar.rs:336-529` | 266→194 | faithful |
| `Avatar/RemoteAvatarDataMessage.cs` | `avatar.rs:531-563` | 53→33 | faithful |
| `Avatar/ServerAvatarChangeMessage.cs` | `avatar.rs:565-582` | 19→18 | faithful |
| `Avatar/ServerAvatarDataMessage.cs` | `avatar.rs:584-601` | 19→18 | faithful |
| `Avatar/ServerSideSyncPlayerMessage.cs` | `avatar.rs:603-643` | 45→41 | faithful |
| `Camera/CameraPIPMessages.cs` | `camera.rs:3-166` | 174→164 | faithful |
| `Camera/CameraShutterSoundMessages.cs` | `camera.rs:168-220` | 63→53 | faithful |
| `Chat/ChatMessage.cs` | `chat.rs:6-61` | 83→56 | faithful |
| `Chat/ConsoleMessage.cs` | `chat.rs:63-96` | 60→34 | deviates |
| `Chat/ServerChatMessage.cs` | `chat.rs:98-116` | 25→19 | faithful |
| `Identity/ClientMetaDataMessage.cs` | `identity.rs:4-302` | 307→299 | deviates |
| `Identity/NetIDMessage.cs` | `identity.rs:304-323` | 37→20 | deviates |
| `Identity/OwnershipTransferMessage.cs` | `identity.rs:325-343` | 24→19 | faithful |
| `Identity/PlayerIdMessage.cs` | `identity.rs:345-382` | 30→38 | deviates |
| `Identity/ServerMetaDataMessage.cs` | `identity.rs:384-487` | 139→104 | faithful |
| `Identity/ServerNetIDMessage.cs` | `identity.rs:491-508` | 24→18 | faithful |
| `Identity/ServerUniqueIDMessages.cs` | `identity.rs:510-545` | 53→36 | deviates |
| `Identity/UshortUniqueIDMessage.cs` | `identity.rs:547-562` | 30→16 | deviates |
| `Permissions/AdminRequest.cs` | `permissions.rs:9-160` | 252→152 | deviates |
| `Permissions/PermissionBitsetMap.cs` | `permissions.rs:162-266` | 149→105 | deviates |
| `Permissions/PermissionCompression.cs` | `permissions.rs:268-332` | 110→65 | deviates |
| `Protocol/BasisMessageCatalog.cs` | `protocol.rs:453-535` | 106→83 | faithful |
| `Protocol/BasisMessageManifest.cs` | `protocol.rs:302-451` | 157→150 | faithful |
| `Protocol/BasisP2PMessages.cs` | `protocol.rs:206-300` | 102→95 | faithful |
| `Protocol/BytesMessage.cs` | `protocol.rs:11-44` | 50→34 | deviates |
| `Protocol/ErrorMessage.cs` | `protocol.rs:46-61` | 30→16 | deviates |
| `Protocol/ReadyMessage.cs` | `protocol.rs:63-88` | 34→26 | faithful |
| `Protocol/ServerReadyMessage.cs` | `protocol.rs:90-187` | 109→98 | deviates |
| `Protocol/ServerStatisticMessage.cs` | `protocol.rs:189-204` | 24→16 | faithful |
| `Resources/ContentShareMessage.cs` | `resources.rs:5-127` | 144→123 | deviates |
| `Resources/ModifyResource.cs` | `resources.rs:129-156` | 36→28 | faithful |
| `Resources/ResourceManagementMessage.cs` | `resources.rs:158-281` | 174→124 | faithful |
| `Resources/ServerLibraryMessage.cs` | `resources.rs:283-332` | 57→50 | faithful |
| `Resources/UnLoadResource.cs` | `resources.rs:334-355` | 31→22 | faithful |
| `Scene/RemoteSceneDataMessage.cs` | `scene.rs:6-39` | 50→34 | faithful |
| `Scene/SceneDataMessage.cs` | `scene.rs:41-96` | 95→56 | faithful |
| `Scene/ServerSceneDataMessage.cs` | `scene.rs:98-114` | 22→17 | faithful |
| — | `mod.rs` | —→27 | module root, no C# counterpart |

Every struct, enum and codec in the C# directory is present in the Rust. Nothing is "not ported".

## Deviations

**No wire-format incompatibility was found.** Every message writes the same fields in the same order
at the same widths. I checked each `Serialize`/`Deserialize` pair field by field, including every
length prefix (`ushort` array counts in `AvatarDataMessage`/`SceneDataMessage`/`ServerLibraryMessage`/
`BasisMessageSupply`/`BasisMessageSubscribe`/`ServerUniqueIDMessages`, the `byte`-vs-`ushort` recipient
count in `VoiceReceiversMessage`, the `byte` payload size in `AdditionalAvatarData`, the `int` payload
length in `ServerReadyBatchMessage`), the `PlayerIdMessage` byte/ushort channel variants, the
`LocalAvatarSyncMessage` quality-derived fixed payload sizes, and the string framing
(`[ushort byteLen+1][UTF-8]`, `[int len][UTF-8]` for large strings). The 58-entry
`BasisMessageCatalog` was diffed programmatically: every (channel id, name) pair is identical and in
the same order.

The behavioural differences that remain are these.

1. **`ContentShareType` coerces an unknown byte to `Avatar` instead of rejecting the message.**
   C# `ContentShareMessage.cs:60` does `(ContentShareType)reader.GetByte()`, keeping the raw byte in an
   out-of-range enum, and the server's `BasisNetworkContentShare.cs:64-67` has a `default:` arm that
   logs `"Unknown content share type …"` and **returns**, dropping the share. Rust
   `resources.rs:18-25` maps anything outside 0-3 to `Self::Avatar` (`resources.rs:23`), so
   `basis_network_content_share.rs:43-62` sees a valid `Avatar` and accepts the share, gating it on
   the avatar lock and re-broadcasting it with the type byte rewritten to `0`. This is the one
   finding with a security flavour: a client can no longer be rejected for an unknown type, and the
   relayed byte is not what arrived. No test pins the unknown-byte case — the round-trip test at
   `control_and_resource_message_round_trip_tests.rs:1105` only covers the four valid values.

2. **`PlayerIdMessage` refuses a byte-width write of an id past 255 instead of truncating.**
   C# `PlayerIdMessage.cs:27` does `Writer.Put((byte)playerID)`, silently sending id 300 as 44 —
   the wrong player. Rust `identity.rs:377` returns `NetDataError::too_long`. The only server caller
   is `basis_server_handle_events.rs:909-914`, which picks the width from `sender.id() > 255` and
   already gates the send on `.is_ok()`, so the error path is unreachable there and the behaviour is
   identical for every id the server actually sends. Pinned by
   `control_and_resource_message_round_trip_tests.rs:676`.

3. **`PermissionBitsetMap::decode` lower-cases what it returns.** C# `PermissionBitsetMap.cs:124`
   builds a `HashSet<string>(StringComparer.OrdinalIgnoreCase)`, so entries keep their original
   casing and compare case-insensitively. Rust `permissions.rs:253-265` uses a plain `HashSet<String>`
   and lower-cases both the known nodes (`:258`) and the extras (`:262`), so a mixed-case extra node
   comes back changed. Lookup semantics are preserved (`index_of` at `permissions.rs:211-213`
   lower-cases too), but a caller that reads the set and re-sends the strings would send different
   text. Nothing in the Rust server calls `get_permissions()` — only tests do — so the blast radius is
   currently zero. Pinned (as the intended behaviour) by
   `permission_and_message_catalog_tests.rs:198`.

4. **Deflate level differs, so compressed bytes are not identical.** C# uses
   `CompressionLevel.Optimal` at `PermissionCompression.cs:35` and `ServerReadyMessage.cs:77`; Rust
   uses `flate2::Compression::default()` (level 6) at `permissions.rs:284` and `protocol.rs:156`.
   Both emit raw Deflate and both are self-describing (a flag byte on the permissions payload, a bool
   on the ready batch), so either side decodes the other; only the byte count and the raw-vs-compressed
   tie-break can differ. Not a wire break — but a byte-for-byte comparison of two servers' output on
   these two payloads will not match. `repetitive_batch_is_actually_compressed`
   (`avatar_scene_audio_message_round_trip_tests.rs:1026`) pins only that compression happens, not its
   exact output.

5. **`VoiceReceiversMessage::serialize` writes exactly the recipients it holds.** C#
   `VoiceReceiversMessage.cs:107` takes `Users?.Length`, which after a `Deserialize` is the
   **rented** array's length — `ArrayPool.Rent(count)` may hand back a larger buffer, so re-serializing
   a just-deserialized message would emit uninitialised pool entries under an inflated count. Rust
   `audio.rs:146` reads a `Vec` sized exactly to `count`. This is a latent C# bug, not an intentional
   contract: neither server ever re-serializes a deserialized `VoiceReceiversMessage` (the C# server
   only calls `Deserialize` — `BasisServerHandleEvents.cs:1039`, `BasisSavedState.cs:65`; the Rust the
   same at `basis_server_handle_events.rs:930-935`), so it never fired. The truncation path *is*
   pinned, at `avatar_scene_audio_message_round_trip_tests.rs:1625`.

6. **Guards that logged-and-continued now return an error.** These five had an explicit C# guard on
   `AvailableBytes` that logged and left the struct half-filled; the Rust returns `Err` instead. Each
   is pinned by a Rust test, and each server caller (where one exists) handles it.

   | message | C# guard | Rust | test |
   |---|---|---|---|
   | `NetIDMessage` | `NetIDMessage.cs:13-21` logs and leaves `playerID` stale | `identity.rs:310-313` errors | `control_…:611` |
   | `UshortUniqueIDMessage` | `UshortUniqueIDMessage.cs:13-21` checks `!= 0`, so 1 byte still reads past the end | `identity.rs:553-556` errors | `control_…:1096` |
   | `ServerUniqueIDMessages` | `ServerUniqueIDMessages.cs:13-31` nulls `Messages` on a short count, but a *truncated entry list* leaves a partly-filled array | `identity.rs:519-530` clears to `None` first and only publishes a complete list | `control_…:1030` |
   | `ErrorMessage` | `ErrorMessage.cs:13-21` logs and leaves `Message` stale | `protocol.rs:52-55` errors | `control_…:538` |
   | `ConsoleData` | `ConsoleMessage.cs:40-43` logs on an empty reader; `:23-27` sets `array = new byte[0]` and returns on an over-claimed payload | `chat.rs:73` and `chat.rs:75-78` error (still setting `array` to empty first, so the struct state matches) | `control_…:484`, `control_…:496` |

   `NetIDMessage` is the only one with a live server caller: `basis_server_handle_events.rs:1143-1147`
   returns early on the error, where C# `BasisServerHandleEvents.cs:1357-1361` went on to call
   `AddOrFindNetworkID` with whatever string was left in the struct. `ConsoleData` and `ErrorMessage`
   have no server caller on either side.

7. **`BytesMessage::serialize` refuses a payload over 65535 bytes.** C# `BytesMessage.cs:40` casts
   `(ushort)Data.Length`, so a 70000-byte array ships a 4464-byte count and desyncs the reader. Rust
   `protocol.rs:36` returns `too_long`. Pinned by `control_and_resource_message_round_trip_tests.rs:106`.
   Note this bound was applied *selectively*: the other `(ushort)` casts the C# makes on counts —
   `ServerMetaDataMessage.cs:126`, `AvatarDataMessage.cs:84`, `ServerLibraryMessage.cs:40` — are
   reproduced as-is with `as u16` at `identity.rs:475`, `avatar.rs:122`, `resources.rs:315`, so those
   still wrap silently on both sides.

8. **`AdminRequestMode` is an `Option`, not a raw cast.** C# `AdminRequest.cs:10-13` returns
   `(AdminRequestMode)messageIndex` for any byte; Rust `permissions.rs:15-22` returns `None` for a
   byte past the 83 known modes and exposes the raw byte separately via `message_index()`. The 83
   discriminants were checked one by one against the C# enum (including the two commented-out entries
   that shift `TeleportAll` to 7) and match. Pinned by
   `control_and_resource_message_round_trip_tests.rs:59`.

9. **`did:key` bodies are measured in scalars, not UTF-16 units.** C#
   `ClientMetaDataMessage.cs:131` gates on `body.Length > 255` (UTF-16 units); Rust
   `identity.rs:156-157` gates on `body.chars().count() > 255`. Both then hit the same
   `WriteShortString`/`write_short_string` re-check that writes a `0` length when the UTF-8 form
   exceeds 255 bytes (`ClientMetaDataMessage.cs:207-211`, `identity.rs:220-223`) — so both lose a
   multi-byte body of ≤255 chars identically. The divergence is a narrow window only: 128-255
   astral-plane characters, where C# counts >255 units and falls back to the lossless `TagRaw`
   while Rust takes the did:key path and writes an empty body. Base58 did:keys are ASCII, so this is
   unreachable in practice. Untested on either side. The same scalar-vs-unit distinction applies to
   the shared `max_length` truncation in `net_data_writer.rs:317` (vs `NetDataWriter.cs:424`) and the
   read-side check in `net_data_reader.rs:455-457` (vs `NetDataReader.cs:428`), where the Rust
   documents it in a comment.

## Corners cut

None that reduce capability. The port keeps the awkward parts rather than tidying them:

- The two-byte header written for *every* `AdditionalAvatarData` entry including suppressed ones
  (`avatar.rs:60-72`), the exact behaviour the C# comment at `AdditionalAvatarData.cs:11-14` warns
  must not be "optimised".
- `LocalAvatarSyncMessage::serialize` reproduces the C# stub-write for an invalid quality — quality
  byte then a `0` additional-count (`avatar.rs:455-460` vs `LocalAvatarSyncMessage.cs:172-178`) — and
  the `Option<BitQuality>` parameter exists precisely so an out-of-range level round-trips the way
  the C# enum cast did (`avatar.rs:454`).
- `NetIDMessage::serialize` still writes *nothing* for an empty id (`identity.rs:316-320`), the C#
  behaviour at `NetIDMessage.cs:26-33` that leaves the reader desynced. Not fixed, deliberately.
- `ClientMetaDataMessage` still substitutes the literal `"Failure"` for empty fields
  (`identity.rs:297-299`).
- The dead `count > MaxUsers` branch in `VoiceReceiversMessage` (`audio.rs:111-118`) is carried over
  even though a `u16` count can never exceed `u16::MAX`, matching `VoiceReceiversMessage.cs:54-62`.
- `ServerMetaDataMessage`'s trailing-field back-compat reads (`identity.rs:441-443`) match
  `ServerMetaDataMessage.cs:85-89` exactly, including the `> 0` vs `>= 4` asymmetry.

Two genuine simplifications, neither of them capability loss:

- **Pooling is gone.** `ArrayPool` renting in `VoiceReceiversMessage` and the in-place buffer reuse in
  `AvatarDataMessage`/`AvatarLoadDataMessage`/`ClientAvatarChangeMessage`/`SceneDataMessage` become
  plain `Vec` allocations (`audio.rs:129`, `avatar.rs:99`, `avatar.rs:158`, `avatar.rs:266`,
  `scene.rs:60`). `return_pool` survives as a clear (`audio.rs:171-174`). The reuse that mattered most
  — the per-frame face-tracking buffers and the audio segment buffer — *is* kept
  (`avatar.rs:42-45`, `avatar.rs:393-399`, `audio.rs:26-29`). This is an allocation-rate difference,
  not a behavioural one.
- **`BasisMessageCatalog`'s volatile double-checked cache** becomes a `LazyLock` returning a
  `&'static [..]` (`protocol.rs:461-534`), so the Rust cannot hand out two different arrays under a
  race the way `BasisMessageCatalog.cs:18-26` could.

## Improvements

- `ServerUniqueIDMessages::deserialize` (`identity.rs:519-530`) clears `messages` before reading and
  only publishes the vector once every promised entry parsed, so a truncated list can never be
  consumed as a short-but-valid one. C# `ServerUniqueIDMessages.cs:21-25` left a partly-filled array
  with default-constructed tail entries.
- `RemoteSceneDataMessage::serialize` (`scene.rs:27`) clamps `payload_length` to the buffer with
  `.min(payload.len())`. C# `RemoteSceneDataMessage.cs:34-37` passes a stale `payloadLength` straight
  into `Put(payload, 0, len)`, which throws out of a send path.
- `NetDataWriter` grows unconditionally, so scalar `put_*` cannot fail
  (`net_data_writer.rs:11-14`), and length-prefixed puts return `NetResult` instead of the C#'s
  truncating casts.
- `put_array_string_max` rolls the cursor back when an entry is refused
  (`net_data_writer.rs:279-291`), so a caller never ships a truncated array under a full count.
- `get_array_raw` (`net_data_reader.rs:373-378`) validates the count against the remaining bytes
  *before* allocating; C# `GetArray<T>` (`NetDataReader.cs:241-252`) allocates `new T[length]` first
  and only then checks — a 65535-entry claim allocated before it was rejected.
- `AdminRequestMode::from_byte` / `BitQuality::from_byte` / `ContentShareType::from_byte` make the
  unknown-value case explicit at the type level where C# produced an out-of-range enum. (For
  `ContentShareType` the chosen mapping is a regression — see Deviations #1 — but the other two are
  strict wins.)
- The stale comment `0=Low, 1=Medium, 2=High` at `LocalAvatarSyncMessage.cs:19` is corrected to
  `0=VeryLow, 1=Low, 2=Medium, 3=High` at `avatar.rs:342`, which is what
  `BasisAvatarBitPacking.cs:55-61` actually declares.
- Test coverage is far denser than the C#'s: 268 round-trip and hostile-input tests across
  `basis_server_tests/tests/networking/avatar_scene_audio_message_round_trip_tests.rs`,
  `…/control_and_resource_message_round_trip_tests.rs` and
  `…/security/permission_and_message_catalog_tests.rs`, including double-round-trip byte-identity
  assertions per message and exact encoded-size pins on `BasisCompactId`
  (`avatar_scene_audio_message_round_trip_tests.rs:883`).

## Verdict

The wire format is intact: every message writes the same fields in the same order at the same widths,
and the message catalog's 58 (channel, name) pairs match exactly. The port is unusually careful about
reproducing the C#'s quirks — the always-written 2-byte additional-data header, the invalid-quality
stub write, the empty-`NetIDMessage` desync, the dead `MaxUsers` branch — rather than silently
improving them, and the `NetResult` contract change is applied uniformly and handled at every server
call site. The one finding worth acting on is `ContentShareType::from_byte` coercing an unknown type
byte to `Avatar`: the C# server rejected such a message outright, while the Rust accepts it, gates it
on the wrong lock, and rebroadcasts the type byte rewritten to zero.
