# security — port diffs

C#: `BasisNetworkServer/Security/` · Rust: `basis_network_server/src/security/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BasisAllowList.cs` | `basis_allow_list.rs` | 78 → 126 | ported, synchronous load, results reported |
| `BasisAudioRangeLimitManager.cs` | `basis_audio_range_limit_manager.rs` | 80 → 63 | ported, one extra NaN guard |
| `BasisAvatarScaleLimitManager.cs` | `basis_avatar_scale_limit_manager.rs` | 85 → 77 | ported, identical clamps |
| `BasisBanList.cs` | `basis_ban_list.rs` | 85 → 106 | ported, synchronous load |
| `BasisCrashReportStateManager.cs` | `basis_crash_report_state_manager.rs` | 64 → 42 | ported 1:1 |
| `BasisDIDAuthIdentity.cs` | `basis_did_auth_identity.rs` | 461 → 457 | ported, nonce size differs (see D1) |
| `BasisGlobalLockManager.cs` | `basis_global_lock_manager.rs` | 316 → 231 | ported 1:1, same wire order |
| `BasisHeadlessAudioStateManager.cs` | `basis_headless_audio_state_manager.rs` | 57 → 37 | ported 1:1 |
| `BasisHeadlessConnectionPolicyManager.cs` | `basis_headless_connection_policy_manager.rs` | 98 → 70 | ported, case-fold narrowed (D3) |
| `BasisOpusFrameDurationStateManager.cs` | `basis_opus_frame_duration_state_manager.rs` | 64 → 49 | ported 1:1 |
| `BasisOpusPacketLossStateManager.cs` | `basis_opus_packet_loss_state_manager.rs` | 62 → 43 | ported 1:1 |
| `BasisPlayerModeration.cs` | `basis_player_moderation.rs` | 1580 → 1506 | ported, all 57 permission gates identical |
| `BasisRejoinLockManager.cs` | `basis_rejoin_lock_manager.rs` | 45 → 47 | ported 1:1 |
| `BasisResourceLimitManager.cs` | `basis_resource_limit_manager.rs` | 79 → 53 | ported 1:1 |
| `BasisServerLogBundleService.cs` | `basis_server_log_bundle_service.rs` | 298 → 285 | ported, name sanitizer widened (I5) |
| `BasisUserOpusBitrateStateManager.cs` | `basis_user_opus_bitrate_state_manager.rs` | 135 → 91 | ported 1:1, same clamps |
| `PermissionManager.cs` | `permission_manager.rs` | 1194 → 1213 | ported, same resolution semantics |
| — | `mod.rs` | — → 67 | new: shared `send_admin_state_to_peer` / `broadcast_admin_state` helpers |
| `*.cs.meta` (6 files) | — | — | Unity asset metadata, correctly not ported |

Totals: 17 C# files (4781 lines) against 18 Rust files (4563 lines).

Cross-module files read to trace the auth path, not part of this module's file map:
`Contrib/Auth/Did/DidAuth.cs` ↔ `contrib/did/src/did_auth.rs`,
`Contrib/Crypto/Ed25519.cs` ↔ `contrib/crypto/src/ed25519.rs`,
`Core/BasisServerHandleEvents.cs` ↔ `core/basis_server_handle_events.rs`.

## Deviations

**No security-relevant weakening found in the DID handshake, in permission evaluation, in the
ban/allow-list matching, or in any of the 57 admin permission gates.** Every deviation below is
either neutral, a hardening, or an obscure Unicode corner; each is traced to the code path that
reaches it.

### D1 — Challenge nonce is 32 bytes in Rust, 256 bytes in C#

`Contrib/Auth/Did/DidAuth.cs:89` reads `const ushort NONCE_LEN = 256 / sizeof(byte);` — and
`sizeof(byte)` is 1, so the C# nonce is **256 bytes**, not the 256 bits its own comment on line 86
claims. `contrib/did/src/did_auth.rs:105` reads `const NONCE_LEN: usize = 256 / 8;` — **32 bytes**,
which is what the comment always meant.

This is not a weakening. 32 bytes is 256 bits of CSPRNG output; collision and guessing resistance
are far beyond any practical attack, and the C# was simply spending 8× the entropy the design
called for. Nor is it a wire-compatibility problem: the server sends the nonce
(`basis_did_auth_identity.rs:181`, `BasisDIDAuthIdentity.cs:167`) and the client signs the bytes it
received, so no length is baked into either peer.

Both RNGs are cryptographic. C# uses `RandomNumberGenerator.Create()`
(`BasisDIDAuthIdentity.cs:49`); Rust uses `StdRng` (ChaCha12) seeded from the OS
(`did_auth.rs:25-30`). A fresh nonce per challenge is preserved on both sides.

Pinned in Rust by `contrib/did/tests/did_auth_errors.rs:27`
(`assert_eq!(challenge.nonce.0.len(), 32)`). The C# has no test on nonce length, which is how the
`sizeof(byte)` slip survived.

### D2 — Permission node case-folding uses Unicode lowercase, not .NET ordinal-ignore-case

C# stores every permission key in `StringComparer.OrdinalIgnoreCase` collections
(`PermissionManager.cs:89-106`, `:196`, `:653`). Rust folds with `to_lowercase()`
(`permission_manager.rs:91-93`) and keys `CaseInsensitiveSet`/`CaseInsensitiveMap` on the result
(`:107-122`, `:171-193`).

The two agree on all ASCII, which is every node in `PermNodes` and every group name the defaults
create. They diverge only on characters where .NET's simple case folding and Rust's full Unicode
lowercase disagree. The one direction that matters is U+017F LATIN SMALL LETTER LONG S: .NET folds
`ſ` to `s`, Rust does not (`ſ` is already lowercase). So a deny entry hand-written as
`-baſis.moderation.ban` would deny the real node under C# and would not under Rust — a permission
that resolves to allowed in Rust where the C# said denied.

Reaching it requires an operator to type a long-s into `permissions.xml` or the admin panel; no
network input picks the key a decision is stored under. Recorded for completeness, not as a live
hole. No test on either side covers non-ASCII nodes; `moderation_and_permission_manager_tests.rs:363`
(`uuids_and_nodes_are_case_insensitive_and_trimmed`) and
`ModerationAndPermissionManagerTests.cs:502` both test ASCII only.

### D3 — Headless platform match folds ASCII only

`BasisHeadlessConnectionPolicyManager.cs:43-46` compares the four server platform ids with
`StringComparison.OrdinalIgnoreCase`; `basis_headless_connection_policy_manager.rs:42` uses
`eq_ignore_ascii_case`. Same divergence as D2 and the same character: .NET would accept
`Headleſſ` as headless, Rust would not, so with `DisallowHeadless` on a client sending that exact
platform string would be admitted by Rust and rejected by C#. The platform string is
client-supplied (`ClientMetaDataMessage.player_platform`), so this one is at least reachable from
the network — but the only thing it buys an attacker is *not* being classified as a server
platform, which a client can already achieve by sending any other string.

Both sides pin the same 14 cases and both are ASCII:
`opus_and_policy_state_manager_tests.rs:268-286` and
`OpusAndPolicyStateManagerTests.cs:390-408`.

### D4 — A corrupt `permissions.xml` is an error in Rust, a thrown exception in C#

`PermissionManager.cs:227-241` calls `PermissionXml.Load`, whose `XmlReader` (`:880`) throws on
malformed XML; the exception escapes `LoadFromXml` with the store already unchanged.
`permission_manager.rs:387-396` returns `Err` and leaves `inner.store` untouched, and
`PermissionIntegration::init` (`:1094-1106`) propagates it. Same end state — the operator's file is
never silently replaced with an empty (all-default) store — but Rust reports it as a value instead
of an exception. Pinned by
`moderation_and_permission_manager_tests.rs:572` (`load_from_xml_malformed_file_is_an_error_and_keeps_the_store`).

The same applies to `banned_players.xml`: `BasisPlayerModeration.cs:180-183` catches and logs,
leaving the in-memory ban list intact; `basis_player_moderation.rs:257-271` returns `Err` and the
sole caller at `core/network_server.rs:442-444` logs it with the same `"Load banned failed: {e}"`
text. Identical outcome. Pinned by
`moderation_and_permission_manager_tests.rs:923`.

### D5 — Self-closing XML elements clear the parse context in Rust

`PermissionManager.cs:950-973` only clears `currentGroupDef` / `currentUser` on an `EndElement`, and
an `XmlReader` raises no `EndElement` for `<Group name="x" />`. `permission_manager.rs:992` and
`:1003` clear the context when `Event::Empty` is seen. The two differ only for a hand-written file
where a `<Node>` follows an empty `<Group>`/`<User>` as a sibling: C# would attach that node to the
preceding element, Rust would drop it. Files written by either serializer never take that shape,
because a `<Node>` is only ever emitted inside a non-empty parent. Rust's reading is the correct
one. Not pinned.

### D6 — Truncated admin requests are refused instead of read past the end

`BasisPlayerModeration.OnAdminMessage` (`:246-578`) reads fields straight out of the
`NetPacketReader` with no length checks on most branches — a short packet takes whatever LiteNetLib
returns past the end. `basis_player_moderation.rs:336-591` threads `NetResult` through every read;
a short packet returns `Err` from `dispatch` and `on_admin_message` (`:322-325`) logs and replies
`"Malformed {mode:?} request."` without acting. The `AvailableBytes` guards C# does have
(`:1390`, `:1410`, `:1439`, `:1459`, `:1475`, `:1501`, `:1522`) are all preserved verbatim
(`basis_player_moderation.rs:1351`, `:1370`, `:1393`, `:1409`, `:1423`, `:1448`, `:1467`).
Pinned by `moderation_and_permission_manager_tests.rs:1005`
(`on_admin_message_truncated_request_is_dropped`).

### D7 — String truncation counts characters, not UTF-16 code units

`BasisPlayerModeration.cs:595-596` and `:605-606` truncate the server name and MOTD with
`Substring`, which cuts at a UTF-16 boundary and can split a surrogate pair.
`basis_player_moderation.rs:600-616` (`truncate_chars`) cuts at a `char` boundary. Both bound the
value (64 / 256, `BasisNetworkCommons.cs:1318`/`:1320` and
`basis_network_commons.rs:290-291`); the Rust value can be longer in bytes for astral text, and the
reader on the far side reads it with `get_string_max` against the same constant. Not pinned.

### D8 — `EffectivePermissions.Has` on a node beginning with `.`

`PermissionManager.cs:139` calls `node.LastIndexOf('.', idx - 1)` with `idx` already 0 after
matching a leading dot, and .NET throws `ArgumentOutOfRangeException` for a negative `startIndex` on
a non-empty string. `permission_manager.rs:272-278` walks with `rfind` and terminates cleanly. Both
`Has` overloads are only ever called with `PermNodes` constants
(`PermissionManager.cs:1116-1142`, `basis_player_moderation.rs` `require`), so the C# throw is not
reachable from any current call site. Recorded because the Rust is the robust one.

### D9 — `basis.moderation.whitelist` node name kept

`PermNodes.ModerationAllowlist` is the string `"basis.moderation.whitelist"`
(`PermissionManager.cs:75`), and `PermNodes::MODERATION_ALLOWLIST`
(`permission_manager.rs:80`) keeps it byte-for-byte. Deliberate: changing it would silently drop the
grant out of every existing `permissions.xml`. Pinned by
`permission_and_message_catalog_tests.rs:57` (`perm_nodes_string_values_are_pinned`) and its C#
twin.

### D10 — Iteration order of nodes, groups and rule listings is sorted

C# `HashSet<string>` / `Dictionary` iterate in bucket order; Rust `CaseInsensitiveSet::iter`
(`permission_manager.rs:133-137`) and `CaseInsensitiveMap::iter` (`:208-212`) sort. This changes the
order of `<Node>` elements in a saved `permissions.xml`, the order of rules in
`GetAllAllowedRules` / `GetAllDeniedRules`, and the order inside the `GetPermissions` admin
response.

It cannot change any decision. `ApplyRawNodes` (`PermissionManager.cs:714-731`,
`permission_manager.rs:799-809`) makes deny sticky — once a node is `false` no later rule can raise
it — so the final decision for a node is "denied if any applicable rule denies, else allowed if any
allows", which is order-independent. Group visitation order likewise cannot change which groups are
reached, only the sequence, because `visited` only suppresses repeats within one traversal
(`:675-693` / `:769-782`).

### D11 — Allow-list writes report their outcome instead of running detached

`BasisPlayerModeration.cs:636` and `:644` fire `AddToAllowlistAsync` / `RemoveFromAllowlistAsync` and
report success unconditionally; a disk failure is invisible to the admin.
`basis_player_moderation.rs:642-645` and `:656-659` run the write inline and reply
`"Failed to add {uuid} to allowlist — see server log."` on error. Same for the ban list
(`:83-86`, `:109-112`). Pinned by
`security_list_and_lock_manager_tests.rs:136` (`allow_list_add_into_an_unwritable_path_is_an_error`).

## Corners cut

### C1 — `register_for_tests` is compiled into the production crate

`basis_did_auth_identity.rs:96-100` is a `pub fn` — not `#[cfg(test)]`, not feature-gated — that
inserts an authenticated `OnAuth` entry for an arbitrary peer id and UUID, skipping the challenge
round trip entirely. The C# has no counterpart; the integration tests need it because they live in
a separate crate (`basis_server_tests/tests/security/auth_identity_recycled_id_tests.rs:14`, `:28`,
`:40`, `:54`, `:67`) and cannot reach a `#[cfg(test)]` item.

Nothing on the network path calls it, so it is not remotely reachable — but it is a capability in
the shipped library that the original does not expose, and it belongs behind a
`#[cfg(feature = "test-support")]` gate.

### C2 — Ban-list entries are still not newline-scrubbed (inherited)

`BasisAllowList.cs:50` strips CR and LF from an id before appending, because the store is
line-delimited; `BasisBanList.cs:61-69` does not. The Rust reproduces both exactly:
`basis_allow_list.rs:78` filters `\r` and `\n`, `basis_ban_list.rs:68-72` does not. A UUID
containing a newline, reaching `AddToBanList`, would still write two lines into
`BasisBanList.txt`.

This is a faithful port of a pre-existing gap, not something the port introduced. It is also not
currently reachable: the ban path an admin actually drives is
`BasisPlayerModeration::ban`/`ip_ban`, which writes XML (`basis_player_moderation.rs:154-172`,
correctly escaped through `quick_xml::escape::escape`), not `BasisBanList`. Worth fixing on both
sides.

### C3 — `Config::rng` accepts a non-cryptographic RNG

`DidAuth.cs:17` types the field as `CryptoRng` (`RandomNumberGenerator`), so nothing but a CSPRNG
can be installed. `did_auth.rs:16` types it as `Box<dyn Rng + Send>` and `Config::with_rng`
(`:35-40`) accepts any `Rng`, cryptographic or not. The default is `StdRng` (`:25-30`), production
never overrides it, and `with_rng` exists for the deterministic tests — but the type no longer
carries the guarantee the C# type did.

### C4 — `build_and_send` holds a second copy of the log container

`basis_server_log_bundle_service.rs:143` passes `raw.clone()` into `compress`, so a bundle near the
256 MB ceiling is resident twice (three times briefly, while LZ4 output is being built). The C#
holds `raw` plus the compressed target array (`BasisServerLogBundleService.cs:136`, `:227`) and no
clone. Memory only; no behavioural or security difference.

Nothing else is stubbed, simplified or left less capable. In particular: the 56 `Require(peer, …)`
gates in `OnAdminMessage` (`BasisPlayerModeration.cs:246-578`, covering 62 `AdminRequestMode`
case labels) plus the separate `PermissionsView` gate at `:232-239` map to exactly 56
`Self::require(peer, …)` calls plus the same view gate in `dispatch`
(`basis_player_moderation.rs:336-591`, `:339-346`), node for node; the
`IsProtected` guard is on all six of the same operations (ban, ip-ban, kick, force-avatar,
force-avatar-all, locomotion-override and its -all variant —
`:69`, `:94`, `:120`, `:840-842`, `:885`, `:908`, `:950`); every `EnsureDefaults` group membership
and node matches (`PermissionManager.cs:759-833` ↔ `permission_manager.rs:816-879`); every
sanitizer bound matches (avatar scale 0.01/1000, spheres 1/4096, opus bitrate 6000/510000, packet
loss 0/100, frame duration 20/40, peer limit 1/65535, quality distance 0/1000, image enforcement
100/1000); and the global lock payload is written in the identical field order
(`BasisGlobalLockManager.cs:236-265` ↔ `basis_global_lock_manager.rs:200-218`).

## Improvements

**I1 — Signature verification is strict.** `contrib/crypto/src/ed25519.rs:33` uses
`verify_strict`, which rejects small-order and torsion-component public keys and non-canonical
`R`/`S` on top of the RFC 8032 equation. `Contrib/Crypto/Ed25519.cs:47-50` uses BouncyCastle's
`Ed25519Signer`, which does not add the small-order rejection. Wrong-length keys and signatures are
refused before any curve work on both sides (`ed25519.rs:23-31`), so the attacker-controlled
signature length from `on_auth_received` (`basis_did_auth_identity.rs:244`) cannot panic.

**I2 — An unresolvable DID method returns an error instead of throwing.**
`DidAuth.cs:218` indexes `Resolvers[method]` and throws `KeyNotFoundException` if the map lacks the
kind; `did_auth.rs:189-192` returns `DidResolveErr::UnsupportedMethod`.

**I3 — A stale authentication timeout is aborted when it is replaced.**
`BasisDIDAuthIdentity.cs:173` assigns `_timeouts[newPeer.Id] = cts`, dropping the previous
`CancellationTokenSource` without cancelling or disposing it, so an old timer can still fire against
a recycled id. `basis_did_auth_identity.rs:216-218` aborts whatever handle the insert displaced.
Pinned by `auth_identity_recycled_id_tests.rs:62`
(`a_stale_timeout_cannot_evict_the_connection_that_inherited_the_id`).

**I4 — NaN audio ranges are rejected.** `BasisAudioRangeLimitManager.cs:77` sanitizes with
`meters <= 0f ? Default : meters`, and `NaN <= 0f` is false, so a NaN range ceiling is stored and
broadcast. `basis_audio_range_limit_manager.rs:60-62` adds `meters.is_nan()`. (The avatar-scale
sanitizer already guarded NaN and infinity on both sides — `BasisAvatarScaleLimitManager.cs:78-79`,
`basis_avatar_scale_limit_manager.rs:60-65`.)

**I5 — The log-bundle file name is sanitized to the same set on every OS.**
`BasisServerLogBundleService.cs:292` uses `Path.GetInvalidFileNameChars()`, which on Unix is only
`\0` and `/` — so a server name containing `..`, `:`, `*` or a control character reaches the admin
client as the suggested download name unchanged. `basis_server_log_bundle_service.rs:280` applies
the full Windows-invalid set plus all control characters regardless of host OS.

**I6 — The allow list is loaded before the first query can be answered.**
`BasisAllowList.cs:18` fires `LoadAllowlistAsync()` from the constructor and does not await it, so
`IsAllowed` can answer `false` for a legitimately allow-listed player during the window right after
construction — with `BasisUserRestrictionMode.AllowList` on, that is a spurious rejection.
`basis_allow_list.rs:20-26` loads synchronously in `with_file`. Pinned by
`security_list_and_lock_manager_tests.rs:103`.

**I7 — Permission resolution against the store takes the lock.**
`PermissionManager.BuildEffective_NoLock` is `public` (`PermissionManager.cs:650`) and reads
`_store` with no lock held; any caller outside the class races a concurrent mutation.
`permission_manager.rs:739-741` exposes `build_effective`, which takes the read lock, and keeps the
lock-free form private.

**I8 — `set_xml_path` returns an error rather than throwing.**
`PermissionManager.cs:219-224` throws `ArgumentException`; `permission_manager.rs:368-375` returns
`BasisError::permanent(ErrorCode::InvalidArgument, …)` and `PermissionIntegration::init`
propagates it.

**I9 — Serialization failures do not send a half-written packet.** Every admin send in the Rust
checks the writer result before transmitting (`mod.rs:46-53`, `:55-66`;
`basis_player_moderation.rs:1245-1275`; `basis_server_log_bundle_service.rs:234-243`,
`:252-261`), where the C# calls `TrySend` unconditionally after building the writer.

## Verdict

The security module is a faithful port. Every check that decides who gets in and what they may do
— the DID challenge/response and its verification, the duplicate-DID cap, the population-scaled
auth timeout with its 12 ms/peer and 45 s constants, deny-wins permission resolution with wildcard
climbing and group inheritance, the implicit `default` group, all 57 admin permission gates, the
`basis.protection` exemptions, ordinal ban/allow-list matching, IP-ban matching on the bare address,
and every numeric clamp — resolves identically in both implementations, and the extensive test
suites on both sides pin the same cases.

I found no security-relevant weakening. The nonce shrank from 256 bytes to 32 (D1), which corrects
a `sizeof(byte)` slip in the C# and leaves 256 bits of entropy; the only place a permission could
resolve to allowed in Rust where the C# denied is a Unicode long-s in a hand-written deny entry
(D2), which no configuration path produces. The port is meaningfully harder in several places —
strict Ed25519 verification, refusal of truncated admin packets, no stale-timeout race on recycled
peer ids, no allow-list load race, and OS-independent file-name sanitizing.

The one item worth acting on is C1: `register_for_tests` bypasses the challenge round trip and
should be feature-gated out of release builds. C2 (unscrubbed newlines in `BasisBanList`) is an
inherited gap that should be fixed on both sides.
