# networking — port diffs

C#: `BasisNetworkServer/Networking/` · Rust: `basis_network_server/src/networking/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BasisAvatarRequestMessages.cs` | `basis_avatar_request_messages.rs` | 17 → 17 | faithful (both are payload-consuming stubs; neither is registered anywhere) |
| `BasisImageBandwidthGovernor.cs` | `basis_image_bandwidth_governor.rs` | 371 → 296 | faithful |
| `BasisNetworkChat.cs` | `basis_network_chat.rs` | 216 → 148 | deviates (blocked-word dedup is ASCII-cased; see 5) |
| `BasisNetworkContentShare.cs` | `basis_network_content_share.rs` | 253 → 192 | deviates (unknown content type is no longer rejected; see 3) |
| `BasisNetworkImageCache.cs` | `basis_network_image_cache.rs` | 1116 → 771 | deviates (chunk-slot byte accounting; see 6) |
| `BasisNetworkingGeneric.cs` | `basis_networking_generic.rs` | 276 → 218 | faithful |
| `BasisNetworkOwnership.cs` | `basis_network_ownership.rs` | 284 → 296 | deviates (new per-player cap, and a success report on the refusal path; see 1, 2) |
| `BasisSavedState.cs` | `basis_saved_state.rs` | 188 → 127 | faithful |
| `BasisWordFilter.cs` | `basis_word_filter.rs` | 560 → 554 | deviates (mask width and blocklist stepping on non-BMP input; see 4a, 4b) |
| `InitialData/BasisDefaultLibraryConfiguration.cs` | `initial_data/basis_default_library_configuration.rs` | 37 → 42 | deviates (hand-rolled XML vs `XmlSerializer`; see 7) |
| `InitialData/BasisDefaultLibraryLoader.cs` | `initial_data/basis_default_library_loader.rs` | 186 → 157 | faithful |
| `InitialData/BasisLoadableConfiguration.cs` | `initial_data/basis_loadable_configuration.rs` | 52 → 113 | deviates (see 7) |
| `InitialData/BasisLoadableLoader.cs` | `initial_data/basis_loadable_loader.rs` | 115 → 107 | deviates (folder resolution fixed; see Improvements 5) |
| — | `initial_data/mod.rs` | — → 115 | Rust-only: the shared flat-XML reader/writer that stands in for `XmlSerializer` |
| — | `mod.rs` | — → 22 | Rust-only module wiring |
| 6 `*.meta` files | — | — | not ported (Unity asset metadata, nothing to port) |

Totals: 3671 C# lines → 3175 Rust lines.

**Relay routing: exact match, no privacy regression.** `HandleScene`/`HandleAvatar`
(`BasisNetworkingGeneric.cs:184-215`, `:241-272`) and the shared Rust `relay`
(`basis_networking_generic.rs:100-128`) both take the same two branches: with `recipientsSize != 0`
they resolve each id through `AuthenticatedPeers`, deduplicate through a thread-local seen-set,
count unresolvable ids into the throttled missing-peer report, and send **only** to the resolved
list; with `recipientsSize == 0` they broadcast to the peer snapshot minus the sender. The C#
targeted send uses the `ref List<NetPeer>` overload (`NetworkServer.cs:356`) and the Rust uses
`broadcast_message_to_clients` (`network_server.rs:566`) — neither excludes the sender, so a peer
that lists itself gets its own echo in both. Nothing broadcasts where the other unicasts.

The one shape that could have gone wrong is guarded on both sides: the Rust targeted branch is
`recipients_size != 0 && let Some(recipients)` (`basis_networking_generic.rs:101-103`), and falling
out of that `if` broadcasts. That combination is unreachable — `SceneDataMessage::deserialize`
(`basis_network_core/src/serializable/scene.rs:53-75`) only leaves `recipients` at `None` on the
truncated-header path, which also leaves `recipients_size` at 0, and it fills the vector with
exactly `recipients_size` entries otherwise. The C# equivalent
(`BasisNetworkCore/Serializable/Scene/SceneDataMessage.cs:24-40`) allocates the array to the exact
claimed size the same way. So the two agree, but the Rust guard is load-bearing: any future
deserializer that can produce a non-zero size with no array converts a unicast into a room-wide
broadcast.

**Saved-state lifecycle: exact match.** `BasisSavedState.RemovePlayer`
(`BasisSavedState.cs:22-48`) and `remove_player` (`basis_saved_state.rs:25-36`) clear all four maps
— avatar change, player metadata, resolved voice peers, shout mode — and then purge the departing
id from every other player's cached voice-peer list. Both are called from the same position in the
same disconnect sequence (`BasisServerHandleEvents.cs:384-397` vs
`basis_server_handle_events.rs:426-439`), which runs the identical 16 cleanup calls in the identical
order, including `RemovePlayerOwnership`, `RemovePlayerSpheres`, `RemovePlayerImages`,
`BasisImageBandwidthGovernor.RemovePeer` and `RemovePeerSceneEgress`. Nothing is left behind on
either side.

**Per-player limits: one addition, everything else identical.** Content spheres are the same 32
default / 4096 absolute ceiling with the same sanitiser (`BasisResourceLimitManager.cs:15-33` vs
`basis_resource_limit_manager.rs:17-31`) and the same `>=` comparison at the drop site
(`BasisNetworkContentShare.cs:76` vs `basis_network_content_share.rs:69-73`). Scene egress is the
same opt-in token bucket, disabled at 0, 125 000 bytes per megabit, 2 s burst, gate-on-credit and
allow-negative (`BasisNetworkingGeneric.cs:64-103` vs `basis_networking_generic.rs:43-89`). The
image governor matches on every constant: 200 Mbit/s advertised, 150 % enforcement floored at 100,
200 Mbit/s replay, 2 s burst, 25 ms pump (`BasisImageBandwidthGovernor.cs:39-52, 74-89` vs
`basis_image_bandwidth_governor.rs:75-82, 102-113`). The image cache caps match too: 512 MB total,
32 MB per-owner floor, per-owner fair share with per-owner eviction
(`BasisNetworkImageCache.cs:497-547` vs `basis_network_image_cache.rs:368-399`). The only new limit
is ownership, discussed below.

**Word filter tables: byte-identical.** All 4948 trigrams and all 26 homoglyph rows were compared
mechanically; the sets are equal and every homoglyph string matches character for character.

## Deviations

1. **`switch_ownership` reports success on the path where the new per-player cap refused the
   claim.** In Rust, `switch_ownership` on an object the table does not hold calls `add_ownership`
   and returns `true` without looking at the result (`basis_network_ownership.rs:234-237`), while
   `add_ownership` now refuses and returns `false` once the owner is at
   `MaxOwnedObjectsPerPlayer` (`basis_network_ownership.rs:164-167`). `ownership_transfer` treats
   that `true` as a successful switch and broadcasts `CHANGE_CURRENT_OWNER` naming the capped
   player as the owner (`basis_network_ownership.rs:109-119`) even though the table holds no entry
   — the room and the server then disagree, and the next `ownership_response` for that id hands it
   to whoever asks next.

   The C# has the same unconditional `AddOwnership(objectId, newOwnerId); return true;` shape
   (`BasisNetworkOwnership.cs:196-200`) and the same broadcast (`BasisNetworkOwnership.cs:101-108`),
   but there `AddOwnership` can only fail on a lost `TryAdd` race
   (`BasisNetworkOwnership.cs:150`), so the divergent state is a narrow race window rather than a
   reachable outcome. Adding the cap turned an unreachable branch into a reachable one.

   Not pinned. `ownership_state_and_id_database_tests.rs:715-753` covers claims up to the cap,
   refusals past it, release, transfer accounting and disconnect, but never a `switch_ownership`
   whose inner `add_ownership` is refused, and never asserts what `ownership_transfer` broadcasts
   in that case.

2. **The per-player ownership cap is enforced on claim but not on transfer.** `switch_ownership`
   re-points an existing object with no cap check and moves the counters
   (`basis_network_ownership.rs:220-239`), so `owned_count` can be driven arbitrarily above
   `owned_cap()` by transferring objects that already exist. The table size is still bounded — a
   transfer moves an entry rather than creating one — so the denial-of-service goal the cap exists
   for still holds; what does not hold is the literal reading of "per-player cap". The C# has no
   cap at all, so this is Rust-only behaviour. The permissive direction is pinned
   (`ownership_state_and_id_database_tests.rs:741-743` asserts a transfer succeeds and moves the
   counts); the capped direction is not tested.

3. **An unknown content-share type byte is accepted as an Avatar share instead of being rejected.**
   The C# casts the wire byte straight into the enum
   (`BasisNetworkCore/Serializable/Resources/ContentShareMessage.cs:60`), so `msg.ContentType` can
   hold 4–255, and `HandleContentShareDrop` has a `default:` arm that logs
   `Unknown content share type {byte} from peer {id}` and drops the message
   (`BasisNetworkContentShare.cs:65-67`). The Rust clamps at deserialize —
   `ContentShareType::from_byte` maps everything outside 1–3 to `Avatar`
   (`basis_network_core/src/serializable/resources.rs:18-25`) — so the `match` in
   `handle_content_share_drop` (`basis_network_content_share.rs:43-62`) is exhaustive over four
   variants and has no reject arm. A modified client sending type 7 gets a sphere created in Rust
   where the C# refused it. It is still gated behind `ContentShareCreate`, the avatars lock, the
   avatar lockbypass permission and the 32-sphere cap, so the blast radius is small, but a
   validation the C# performed is gone. Not pinned on either side.

4. **Word filter, two non-BMP divergences. Detection is identical for every ASCII and BMP input;
   only text outside the BMP differs.**

   a. *Mask width.* The C# writes asterisks over a UTF-16 `char[]`, using the grapheme's UTF-16
   offset and length (`BasisWordFilter.cs:544-548`), so an astral homoglyph such as `𝐚` (U+1D41A,
   present in the `'a'` row at `BasisWordFilter.cs:273`) becomes **two** asterisks. The Rust writes
   over a `Vec<char>` using code-point offset and count (`basis_word_filter.rs:544-547`), so the
   same grapheme becomes **one**. `"𝐚ss"` against `["ass"]` yields `"****"` in C# and `"***"` in
   Rust. Both detect the match; only the mask length differs. Reachable, since the homoglyph tables
   are full of astral characters. Not pinned — both suites use only BMP homoglyphs
   (`sanitizer_and_word_filter_tests.rs:290-295`, `SanitizerAndWordFilterTests.cs:395-400`).

   b. *Blocklist stepping.* The C# steps the banned word by UTF-16 code unit
   (`BasisWordFilter.cs:451`, `:468`), the Rust by Unicode scalar
   (`basis_word_filter.rs:487`, `:494`, `:508`). For an entry containing a non-BMP character the C#
   compares lone surrogates and can never match it, while the Rust compares the whole scalar and
   can. Every shipped default word is ASCII (`BasisNetworkChat.cs:41-99` /
   `basis_network_chat.rs:23-34` are identical, 51 entries in the same order), so this only bites an
   operator who puts an emoji or an astral character in `chat_word_filter.txt`. Not pinned.

   Everything else about the two matchers lines up: the same three trigram windows and the same
   "skip trigrams wholly inside the banned word" rule (`BasisWordFilter.cs:329-381` vs
   `basis_word_filter.rs:402-439`), the same both-boundary embedded-word check that deliberately
   does *not* apply that skip (`BasisWordFilter.cs:390-426` vs `basis_word_filter.rs:450-484`), the
   same advance/retreat state machine, the same reset-and-continue on an embedded match, and the
   same restart-from-the-top replacement loop.

5. **Blocked-word deduplication is ASCII-case-insensitive in Rust, ordinal-case-insensitive in
   C#.** The C# stores the list in a `HashSet<string>(StringComparer.OrdinalIgnoreCase)`
   (`BasisNetworkChat.cs:19`, filled at `:106-117`); the Rust dedups with `eq_ignore_ascii_case`
   (`basis_network_chat.rs:67`). A list containing two non-ASCII case variants of the same word
   keeps one entry in C# and two in Rust. Filtering is idempotent, so the filtered output is
   unchanged; only the `Loaded N words into chat filter` count differs. The same ASCII-only
   comparison appears in `BasisDefaultLibraryLoader::remove_item`
   (`basis_default_library_loader.rs:116`, `:130`) where the C# used
   `StringComparison.OrdinalIgnoreCase` (`BasisDefaultLibraryLoader.cs:127`, `:143`), so a
   non-ASCII URL differing only in case would not be matched for removal. Not pinned.

6. **Image-cache chunk-slot accounting charges twice as much per slot.** Both sides charge the
   client-declared chunk count against the buffer so an implausible `totalChunks` trips
   `cost > cap` before anything is allocated — the C# at `IntPtr.Size` = 8 bytes per slot
   (`BasisNetworkImageCache.cs:332`, and `:382` for the animation backbone), the Rust at
   `size_of::<Option<Arc<[u8]>>>()` = 16, because an `Arc<[u8]>` is a fat pointer
   (`basis_network_image_cache.rs:122`, used at `:226` and `:262`). At the same configured
   `ImageCacheMaxMegabytes` the Rust therefore refuses a declared chunk count roughly half as large
   at `basis_network_image_cache.rs:370` (`BasisNetworkImageCache.cs:500`), and per-owner shares
   fill sooner. It errs in the safe direction and is arguably the more honest figure for the Rust
   representation, but the admission threshold is not the same number. Not pinned — every byte
   assertion on both sides is relative (`network_image_cache_tests.rs:250, 281, 298, 371` vs
   `BasisNetworkImageCacheTests.cs:318, 348, 365, 443`).

7. **`InitialData` XML: a bad field value is defaulted rather than aborting the folder.** The C#
   deserialises through `XmlSerializer` (`BasisLoadableConfiguration.cs:41-47`), which throws on a
   malformed number; `BasisLoadableLoader.LoadXML` catches at the top
   (`BasisLoadableLoader.cs:47-50`), so one bad `<PositionX>abc</PositionX>` means **nothing** in
   that folder is loaded. The Rust parses field by field and falls back to the per-field default
   (`initial_data/mod.rs:94-96`), so the rest of the file and the rest of the folder still load.
   Relatedly, `field_bool` accepts only `true`/`false` case-insensitively
   (`initial_data/mod.rs:98-100`) whereas `XmlSerializer` also accepts the `xs:boolean` forms `1`
   and `0`, so `<Persist>1</Persist>` is `true` in C# and `false` in Rust. Neither loader has a test
   on either side.

8. **Malformed packets are logged and dropped instead of thrown.** The C# handlers call
   `Deserialize` bare and let it throw out of the handler
   (`BasisNetworkOwnership.cs:32`, `:54`, `:95`; `BasisNetworkContentShare.cs:29`, `:131`;
   `BasisNetworkChat.cs:182`; `BasisNetworkingGeneric.cs:117`, `:222`). The Rust returns a
   `Result` and drops the packet at each site (`basis_network_ownership.rs:40-49`,
   `basis_network_content_share.rs:32-35` and `:116-119`, `basis_networking_generic.rs:132`
   and `:188`), with chat deliberately silent rather than logged, for the amplification reason
   already documented in the C# comment (`basis_network_chat.rs:115-118`). Listed here for
   completeness; it reads as an improvement, not a regression.

**Checked and equal, for the record.** The chat handler's filter/sanitise/rewrap/broadcast-excluding-
sender sequence (`BasisNetworkChat.cs:169-214` vs `basis_network_chat.rs:105-147`); the chat
sanitiser itself, which lives outside this module (`BasisNetworkCore/Sanitization/BasisChatSanitizer.cs`
vs `basis_network_core/src/sanitization/basis_chat_sanitizer.rs`) and applies the same 256 UTF-16
unit clamp, the same 512-byte cap and the same never-split-a-scalar trimming; the image cache's
offer/deliver bookkeeping, one-offer-per-player rule, recipient seeding, owner-only despawn, evict
notification, pose overwrite and replay ordering (spawn, transform, chunks, animation); the governor's
append-to-live-job replay queue and retired-job race handling; and content-share create/cleanup
authorisation, including the sharer-or-`protection` check that stops one player deleting another's
orbs (`BasisNetworkContentShare.cs:145-150` vs `basis_network_content_share.rs:130-133`).

## Corners cut

One, and it is deviation 3: the `default:` arm that rejected an unrecognised `ContentShareType`
(`BasisNetworkContentShare.cs:65-67`) has no counterpart, because the Rust enum absorbs unknown
bytes into `Avatar` at deserialize time. A validation the original performed is not performed.

Nothing else is stubbed, simplified or left less capable. `BasisAvatarRequestMessages` is a pair of
handlers that read their payload and do nothing in *both* languages
(`BasisAvatarRequestMessages.cs:7-15`, `basis_avatar_request_messages.rs:9-16`), and is registered
in neither. The `XmlSerializer` replacement in `initial_data/mod.rs` is a real reimplementation
rather than a stub: it handles comments, empty elements, entity unescaping and unknown elements, and
adds a matching writer that the C# got from the serializer for free
(`basis_loadable_configuration.rs:193-215`, `basis_default_library_configuration.rs:253-255`). Test
coverage is at parity or better: 50 sanitiser/word-filter tests against 49, 41 ownership tests
against 38, 37 image-cache tests against 37, 10 governor tests against 10.

## Improvements

1. **A per-player ownership cap the C# lacks.** `add_ownership` refuses a claim once the owner
   holds `MaxOwnedObjectsPerPlayer` objects (`basis_network_ownership.rs:150-167`), configured at
   `basis_server_configuration.rs:96` with a deliberately high 262144 default. This matters because
   ownership ids are client-chosen strings and entries only leave the table when the owner
   disconnects, so before this the table was unbounded for as long as a client stayed connected —
   the C# `ownershipByObjectId` (`BasisNetworkOwnership.cs:12`) has no ceiling of any kind. Pinned
   at `ownership_state_and_id_database_tests.rs:713-753`, with the shipped default's adequacy pinned
   separately at `:757-761`. See deviations 1 and 2 for the two edges it leaves open.

2. **The cap is O(1), not a table scan.** `OWNED_COUNT` is maintained alongside the table
   (`basis_network_ownership.rs:15`, `:183-197`) with the counter entry dropped at zero so the map
   is bounded by the live population. A scan-per-claim at a 262144 ceiling would itself be the
   denial of service the cap exists to prevent.

3. **A latent deadlock avoided rather than inherited.** `RemoveOwnership` takes `LockObject`
   (`BasisNetworkOwnership.cs:56`) and then calls `RemoveObject`, which takes it again
   (`BasisNetworkOwnership.cs:166`) — correct only because C# `lock` is reentrant. `parking_lot`'s
   `Mutex` is not, so the Rust splits the entry point from the locked body
   (`basis_network_ownership.rs:200-217`), keeping the public API identical.

4. **The pump survives a failed thread spawn.** `EnsurePump` calls `_pump.Start()` with no guard
   (`BasisImageBandwidthGovernor.cs:258`), leaving `_pumpRunning` true and throwing into whichever
   caller happened to enqueue. The Rust checks the spawn result, resets the flag and logs
   (`basis_image_bandwidth_governor.rs:198-202`), so a later enqueue can retry.

5. **`BasisLoadableLoader` reads the folder it created.** The C# builds
   `exeDirectory/FolderName` and creates it there (`BasisLoadableLoader.cs:18-30`) but then passes
   the bare relative `FolderName` to `LoadAllFromFolder` (`BasisLoadableLoader.cs:32`), so the load
   resolves against the current working directory — the same folder only when cwd happens to equal
   the exe directory. The Rust resolves once through `Configuration::base_directory()` and passes
   the absolute path through (`basis_loadable_loader.rs:52-53`, used at `:67`). Note this is also a
   behavioural difference, not purely a cleanup: a server started from a different cwd loads a
   different (or no) folder under the C#.

6. **Deterministic XML load order.** `xml_files` sorts the directory listing
   (`initial_data/mod.rs:107-115`); `Directory.GetFiles` (`BasisLoadableConfiguration.cs:40`,
   `BasisDefaultLibraryConfiguration.cs:77`) makes no ordering guarantee, so identical folders could
   load resources in different orders on different machines.

7. **A few extra bounds guards on paths the C# would have thrown on.** `build_spawn` checks the
   retained transform is long enough before splicing the pose
   (`basis_network_image_cache.rs:576`), where the C# splices unconditionally
   (`BasisNetworkImageCache.cs:829`) and relies on the length check at admission
   (`BasisNetworkImageCache.cs:404`). The chat handler checks `payload.len() >= payload_size`
   before slicing (`basis_network_chat.rs:122`), where the C# `Encoding.UTF8.GetString(payload, 0,
   payloadSize)` (`BasisNetworkChat.cs:188`) would throw; today's deserialisers make both
   unreachable, but the Rust degrades instead of faulting.

## Verdict

This is a close, careful port: relay routing, recipient resolution, saved-state teardown, the
word-filter matcher and its 4948-entry trigram table, the image cache's whole offer/deliver
lifecycle, and every rate limit and per-player cap the C# had all agree, and the disconnect path
runs the same sixteen cleanups in the same order. The additions are genuine — a per-player
ownership cap over an unbounded client-keyed table, an O(1) counter to enforce it with, a
non-reentrant-lock fix, and a folder-resolution bug fixed on the way through. Two things deserve
follow-up: the new cap's refusal path returns `true` through `switch_ownership`, so a capped player
can have a transfer broadcast to the room that the server's own table does not record; and the
Rust's `ContentShareType` deserialiser silently coerces unknown type bytes to `Avatar`, dropping the
rejection the C# performed. Both are small and both are unpinned by tests.
