# Sanitization — port diffs

C#: `Basis Server/BasisNetworkCore/Sanitization/` · Rust: `basis_server/basis_network_core/src/sanitization/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `Sanitization/BasisChatSanitizer.cs` | `sanitization/basis_chat_sanitizer.rs` | 60 → 43 | faithful |
| `Sanitization/BasisDisplayNameSanitizer.cs` | `sanitization/basis_display_name_sanitizer.rs` | 69 → 47 | deviates |
| — | `sanitization/mod.rs` | — → 4 | Rust-only module glue |

## The exact limits — both sanitizers

| limit | C# | Rust | same? |
| --- | --- | --- | --- |
| chat max characters | 256, UTF-16 code units (`BasisChatSanitizer.cs:10`) | 256, UTF-16 code units (`basis_chat_sanitizer.rs:8`) | yes |
| chat max bytes | 512, UTF-8 (`:11` → `ChatMessage.cs:15`) | 512, UTF-8 (`:9` → `chat.rs:19`) | yes |
| display name length cap | none | none | yes |
| display name stripped | Cc, Cf, 6 named glyphs | Cc, Cf, the same 6 glyphs | see deviation 1 |

Pinned by `ChatSanitizer_Constants_MatchChatWireContract`
(`SanitizerAndWordFilterTests.cs:137-142`) and
`chat_sanitizer_constants_match_chat_wire_contract`
(`sanitizer_and_word_filter_tests.rs:112-117`), which assert 256 and 512 literally and tie
the byte cap back to `ChatMessage::MAX_PAYLOAD_BYTES` on both sides.

## Deviations

**1. The display-name sanitizer strips more than the C# did: supplementary-plane format
characters.** This is the one substantive finding in the module, and it is security-relevant.

The C# iterates `foreach (char character in displayName)` (`BasisDisplayNameSanitizer.cs:32`).
A C# `char` is a **UTF-16 code unit**, not a scalar. For any character above the BMP the loop
sees two surrogate code units, and each of the three filters lets a surrogate through:

* `char.IsControl(surrogate)` is false — `IsControl` is category Cc only (`:34`).
* `CharUnicodeInfo.GetUnicodeCategory(surrogate)` returns `UnicodeCategory.Surrogate`, never
  `Format` (`:38`).
* `IsInvisibleGlyph` compares against six BMP chars (`:14-22`, `:57-67`) — no match.

So the surrogates are appended unchanged (`:46`) and survive `Trim()` (`:49`). The concrete
consequence: **the C# does not strip any Cf character above U+FFFF**, including U+E0020–U+E007F
(the TAG characters), U+E0001, U+1D173–U+1D17A, U+13430–U+1343F, U+110BD, U+110CD and
U+1BCA0–U+1BCA3. A display name consisting only of TAG characters renders as nothing, yet
`BasisDisplayNameSanitizer.IsValid` returns **true** in C#.

The Rust iterates `display_name.chars()` (`basis_display_name_sanitizer.rs:14`) — Unicode
scalars — and its `is_format_category` (`:40-46`) enumerates the full Cf set including all of
the supplementary-plane ranges above. `sanitize("\u{E0020}")` returns `""` and `is_valid`
returns **false**.

This reaches the join gate. Both servers reject a connection whose sanitized display name is
empty (C# `BasisServerHandleEvents.cs:636-642`, Rust
`basis_server_handle_events.rs:641-647`, same log line and same rejection message). So a name
made only of invisible supplementary-plane format characters is **accepted by the C# server
and rejected by the Rust one**. The Rust behaviour is what the module's own doc comment asks
for — "a name that renders blank cannot slip through" (`BasisDisplayNameSanitizer.cs:7-8`) —
so this is the port fixing a hole, but it is still a behaviour change that can reject a name
the C# admitted.

**Not pinned by a test.** `DisplayName_FormatCharacters_Removed`
(`SanitizerAndWordFilterTests.cs:197`) and `display_name_format_characters_removed`
(`sanitizer_and_word_filter_tests.rs:150-155`) both test only BMP format characters
(U+200B, U+200C, U+200D, U+200E, U+202A, U+202E, U+2066, U+FEFF, U+00AD) — the C# theory is
typed `char`, so it *cannot* express a supplementary-plane case, and the Rust twin copied the
same list. Nothing in either suite covers U+E0020.

**2. The Cf set is hardcoded in Rust, table-driven in C#.**
`basis_display_name_sanitizer.rs:40-46` enumerates Cf as a `matches!` pattern, with the
reasoning stated at `:37-39` ("a short, stable list"). The C# asks the runtime
(`CharUnicodeInfo.GetUnicodeCategory`, `:38`). The enumerated list is complete and correct
against Unicode 15.1 — I checked every range — but it will not pick up characters a future
Unicode assigns to Cf, whereas the C# tracks whatever table the .NET runtime ships. A
maintenance difference, not a current behaviour difference. Not pinned.

**3. UTF-16 vs UTF-8 — traced, and it does *not* change the chat result.** The C# clamps
`string.Length` (UTF-16 code units) then measures `Encoding.UTF8.GetByteCount`; the Rust
holds UTF-8 throughout. The port preserves the C# semantics exactly rather than switching to
UTF-8 counting:

* `clamp_utf16_length` (`basis_chat_sanitizer.rs:24-36`) walks `char_indices`, accumulates
  `ch.len_utf16()`, and breaks at the character boundary before the budget would be exceeded
  — the comment at `:22-23` says so. It therefore never splits a pair, which is what the C#
  `ClampUtf16Length` achieves by backing off one unit when `text[255]` is a high surrogate
  (`BasisChatSanitizer.cs:36-42`). For a pair straddling the 256th unit both stop at 255 units.
* The byte loop is `sanitized.len() > MAX_MESSAGE_BYTES` (`:16`) — `str::len()` *is* the UTF-8
  byte count, the same quantity `Encoding.UTF8.GetByteCount` returns
  (`BasisChatSanitizer.cs:21`).
* `trim_last_scalar` (`:38-42`) is `String::pop`, which removes one whole `char` — one scalar,
  i.e. 2 UTF-16 units for a non-BMP character. The C# version explicitly removes 2 units for
  a surrogate pair and 1 otherwise (`BasisChatSanitizer.cs:50-57`). Same scalar granularity.

Pinned in both directions by `ChatSanitizer_TruncationDoesNotSplitSurrogatePair` (`:81`) /
`chat_sanitizer_truncation_does_not_split_surrogate_pair` (`:70`),
`ChatSanitizer_EmojiMessage_ClampsToWholeEmojiAtExactByteCap` (`:90`) / (`:78`),
`ChatSanitizer_CjkOverByteCap_TrimsWholeCharacters` (`:100`) / (`:87`), and
`ChatSanitizer_ByteTrim_RemovesWholeEmojiScalars` (`:109`) / (`:95`).

Neither sanitizer works in grapheme clusters. The Rust does not pull in a segmentation crate,
and it should not: matching the C# means scalars.

**4. The input domain narrowed: no lone surrogates.** A C# `string` can hold an unpaired
surrogate; a Rust `&str` cannot. The C#'s `ClampUtf16Length` surrogate check
(`BasisChatSanitizer.cs:37-40`) exists partly to avoid *creating* one, and
`Encoding.UTF8.GetByteCount` would encode any pre-existing lone surrogate as U+FFFD (3 bytes)
under the default replacement fallback — a case the Rust cannot reach because the string is
already valid UTF-8 before `sanitize` is called. A real difference in what the two functions
can be handed, but it moves the handling upstream to decoding rather than dropping it.

**5. Null is not representable.** C# both sanitizers guard `string.IsNullOrEmpty`
(`BasisChatSanitizer.cs:15`, `BasisDisplayNameSanitizer.cs:26`); Rust checks `is_empty()`
only (`:12` / `:10`). Language difference, no behaviour lost.

## Corners cut

None. Every filter and every constant is present:

| behaviour | C# | Rust |
| --- | --- | --- |
| six named invisible glyphs (U+115F, U+1160, U+3164, U+FFA0, U+2800, U+180E) | `:14-22` | `:7` — same six, same order |
| control (Cc) stripped, before the whitespace fold | `:34-36` | `:15-17` |
| format (Cf) stripped | `:38-41` | `:18-20` |
| whitespace folded to U+0020, not removed | `:46` | `:24` |
| trim after folding | `:49` | `:26` |
| `IsValid` = non-empty sanitize | `:52-55` | `:29-31` |

`char.IsControl` is Cc; Rust's `char::is_control` is Cc — the same U+0000–U+001F and
U+007F–U+009F. `char.IsWhiteSpace` (Zs + Zl + Zp + U+0009–U+000D + U+0085) and Rust's
`char::is_whitespace` (the Unicode `White_Space` property) resolve to the same set, so the
fold and the trim agree; and because the control check runs first, `\t` and `\n` vanish
rather than becoming spaces in both — pinned by
`DisplayName_TabsAndNewlines_RemovedAsControls_NotFoldedToSpace` (`:179`) and its twin
(`:142`).

Test coverage is 1:1: 25 C# facts/theories across the two sanitizers (12 chat, 13 display
name) against 25 Rust tests,
same names, same inputs, including the idempotence pairs and the `blank after sanitize` list.

## Improvements

* `basis_display_name_sanitizer.rs:40-46` — strips supplementary-plane Cf characters,
  including the TAG block, which the C# structurally could not (deviation 1). This is the
  sanitizer doing the job its own doc comment describes.
* `basis_chat_sanitizer.rs:16` — the byte-cap loop reads `str::len()`, an O(1) field, where
  the C# re-runs `Encoding.UTF8.GetByteCount` over the whole string on every iteration
  (`BasisChatSanitizer.cs:21`). Both loops are still quadratic in the number of trimmed
  scalars because `trim_last_scalar` allocates a fresh `String` each pass (`:39`), but the
  per-iteration constant is much smaller.

## Verdict

The chat sanitizer is a faithful port: 256 UTF-16 code units then 512 UTF-8 bytes, clamped at
scalar boundaries, with the UTF-16 counting preserved deliberately rather than quietly
becoming a UTF-8 or grapheme count — and the surrogate, emoji and CJK boundary cases are
pinned on both sides. The display-name sanitizer diverges in one place that matters: the C#
loops over UTF-16 code units and so silently passes every format character above U+FFFF,
including the TAG block, while the Rust loops over scalars and strips the complete Cf set.
That makes the Rust stricter and correct, but it changes the join gate's answer for such
names, and no test on either side covers it. The hardcoded Cf table is the price, and it will
need revisiting when Unicode adds to Cf.
