# io — port diffs

C#: `BasisNetworkCore/Io/` · Rust: `basis_network_core/src/io/`

Both sides are vendored copies of LiteNetLib's `NetDataReader` / `NetDataWriter` (MIT, Ruslan
Pyrch), already modified in the C# before the port. Everything below compares the *vendored* C#
in `BasisNetworkCore/Io/`, not upstream LiteNetLib.

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `NetDataReader.cs` | `net_data_reader.rs` | 833→706 | deviates |
| `NetDataWriter.cs` | `net_data_writer.rs` | 478→335 | deviates |
| — | `mod.rs` | —→5 | extended (module wiring; also re-exports `NetDataError`/`NetResult`, which have no C# counterpart because the C# threw) |

Test coverage on both sides: C# `BasisServerTests/Networking/NetDataReaderWriterTests.cs`
(38 `[Fact]`/`[Theory]`); Rust `basis_server_tests/tests/networking/net_data_reader_writer_tests.rs`
(36 tests, a near line-for-line port of the C# suite) plus `basis_network_core/tests/io_errors.rs`
(16 tests, new — negative paths the C# suite did not have).

## Deviations

**1. Over-64K length prefixes are refused instead of wrapping to zero.**
C# `PutArray(Array, int)` casts the element count with an unchecked `(ushort)`
(`NetDataWriter.cs:302`), as does `PutArray(string[])` (`NetDataWriter.cs:359`), so a 65536-element
array is written as an empty record. Rust `put_ushort_count` returns `NetDataError::TooLong`
(`net_data_writer.rs:200-204`) and the caller writes nothing. Both sides pin their own behaviour,
in direct contradiction: the C# test `PutBytesWithLength_Above64K_LengthPrefixWrapsToZero`
(`NetDataReaderWriterTests.cs:365-376`, comment "Known design cap … Pinned so any future widening
is a deliberate wire change") asserts the wrap; the Rust
`put_bytes_with_length_above_64k_is_refused_and_writes_nothing`
(`net_data_reader_writer_tests.rs:268-277`) and `oversized_length_prefixed_values_are_refused_without_writing`
(`io_errors.rs:193-219`) assert the refusal. Reason: no exceptions in Rust, and the port chose an
error over a silent truncation. This is wire-visible — a message a C# server would have emitted as
an empty record is simply not emitted by the Rust server — so it is the deviation to know about.

**2. Invalid UTF-8 on the wire is decoded lossily instead of rejected.**
The C# encoder is `new UTF8Encoding(false, true)` (`NetDataWriter.cs:43`) — `throwOnInvalidBytes:
true` — so `GetString` (`NetDataReader.cs:444`), `GetString(int)` (`:428`, `:430`), `GetLargeString`
(`:456`) and `PeekString` (`:618`, `:620`) throw `DecoderFallbackException` on malformed UTF-8.
Rust uses `String::from_utf8_lossy` at `net_data_reader.rs:454`, `:478` and `:606`, substituting
U+FFFD and returning `Ok`. A peer that sends a mangled display name or chat line gets a disconnect-
counted protocol fault on the C# server and a mangled-but-accepted string on the Rust one. Reason:
`String` must be valid UTF-8, so the port had to choose; `String::from_utf8` would have been the
strict equivalent. **Not pinned by a test on either side** — neither suite feeds invalid UTF-8.

**3. `char` is a Unicode scalar value, not a UTF-16 code unit.**
C# `GetChar` is `(char)GetUShort()` (`NetDataReader.cs:354-357`), so a lone surrogate 0xD800–0xDFFF
round-trips; Rust `get_char` maps it to U+FFFD (`net_data_reader.rs:333-336`), and `put_char` maps
any non-BMP `char` to U+FFFD rather than truncating to its low half (`net_data_writer.rs:159-162`
vs C# `Put(char)` at `NetDataWriter.cs:199-202`). Reason: Rust `char` cannot hold a surrogate.
Pinned by `string_truncation_and_char_handling_do_not_split_utf8` (`io_errors.rs:249-259`).

**4. `try_get_char` uses U+0000 where `get_char` uses U+FFFD.**
`net_data_reader.rs:637` substitutes `'\0'` for a surrogate; `net_data_reader.rs:335` substitutes
`'\u{FFFD}'` for the same input. In the C#, `'\0'` was the *failure* sentinel written to the `out`
parameter when the read did not happen (`NetDataReader.cs:680`), never a successful value. The Rust
returns `Some('\0')`, conflating "read a surrogate" with the C# no-read sentinel. Internal
inconsistency rather than a divergence from the C#; not pinned. `try_get_char` has no production
call site in the Rust tree (only `net_data_reader_writer_tests.rs:285` and `:368`), so it is inert.

**5. String length limits count scalar values, not UTF-16 code units.**
C# `GetString(int maxLength)` compares `GetCharCount` (`NetDataReader.cs:428`) and `Put(string,
int)` truncates at `value.Length` (`NetDataWriter.cs:425`), both UTF-16 units. Rust compares and
truncates on `chars().count()` (`net_data_reader.rs:457`, `net_data_writer.rs:317-322`). For a
string with astral characters near the limit, a C# writer and a Rust reader disagree about whether
it is over the cap. Reason: documented in-line at `net_data_reader.rs:455-456`. Rust↔Rust is
self-consistent, so this only bites in mixed-version traffic. Partly pinned:
`string_writer_max_length_truncates_by_char_count` (`net_data_reader_writer_tests.rs:129-135`)
uses ASCII only, so the astral case is unpinned.

**6. Writer `set_position` grows the buffer; C# does not.**
`NetDataWriter.cs:144-149` just assigns `_position`, so `SetPosition(n)` past capacity followed by
`AsReadOnlySpan()` (`:38-41`) or `CopyData()` (`:132-137`) reads out of range and throws. Rust
`set_position` calls `resize_if_need` first (`net_data_writer.rs:107-112`), so the written prefix is
zero-filled and valid. Reason: preserving the `position <= data.len()` invariant instead of
throwing. Pinned by `the_writer_grows_instead_of_overflowing` (`io_errors.rs:241-246`).

**7. `from_bytes(bytes, copy: false)` moves rather than aliases.**
C# `FromBytes(byte[], copy: false)` keeps the caller's array (`NetDataWriter.cs:72`), and the C#
test asserts identity with `Assert.Same(source, borrowed.Data)`
(`NetDataReaderWriterTests.cs:791-793`). Rust takes the `Vec<u8>` by value
(`net_data_writer.rs:41-47`), so the caller cannot observe later writes. Reason: Rust ownership;
shared mutable aliasing is not expressible. The Rust test asserts contents instead
(`net_data_reader_writer_tests.rs:620-627`).

**8. `is_null()` is true for an empty buffer.**
C# `IsNull` is `_data == null` (`NetDataReader.cs:40-44`), false for a zero-length array; Rust is
`data.is_empty() && data_size == 0` (`net_data_reader.rs:214-216`), true. Reason: no null. Verified
inert — `NetDataReader.IsNull` has no production call site in the C# tree (only `Clear`'s test
assertion). Pinned by `set_source_reuse_resets_state_and_clear_nulls_reader`
(`net_data_reader_writer_tests.rs:518`).

**9. `get_bool_array` normalises non-0/1 bytes.**
C# `GetBoolArray` goes through `GetArray<bool>(1)`, which `Buffer.BlockCopy`s raw bytes into a
`bool[]` (`NetDataReader.cs:249`, `:278-281`), so a wire byte of 2 becomes a `bool` with bit pattern
2 — truthy under `if`, unequal to `true` under `==`. Rust maps `*b == 1` (`net_data_reader.rs:386`),
so 2 becomes `false`. Only reachable with malformed input, and the scalar path already agreed (C#
`GetBool` is `GetByte() == 1`, `NetDataReader.cs:351`). Not pinned.

## Corners cut

**1. The writer's non-auto-resizing mode is gone.** C# `NetDataWriter(bool autoResize)` and
`(bool, int)` (`NetDataWriter.cs:49-57`) skip every `ResizeIfNeed` guard, so a `Put` past capacity
hits `FastBitConverter.ThrowIndexOutOfRangeException` (`:449-450`, `:475`). Rust always grows
(`net_data_writer.rs:81-86`, documented at `:11-13`). I verified the rationale: `grep -rn
"NetDataWriter(false"` across the C# tree hits only `NetDataReaderWriterTests.cs:764` and `:769`
— no production call site. The C# test `Writer_WithoutAutoResize_ThrowsWhenCapacityExceeded`
(`:762-773`) has no Rust counterpart, which is the one test in the C# suite the Rust suite does not
carry.

**2. `GetRemainingBytesMemory` is not ported.** `NetDataReader.cs:506-510` returns a
`ReadOnlyMemory<byte>`. Verified: the only caller is `NetDataReaderWriterTests.cs:864`.
`get_remaining_bytes_segment` returning `Bytes` (`net_data_reader.rs:500-505`) serves the same
purpose and is stronger (owned, refcounted). No real loss.

**3. `FromBytes(byte[], int offset, int length)` is not ported.** `NetDataWriter.cs:81-86`. Verified:
only caller is `NetDataReaderWriterTests.cs:799`. `NetDataWriter::from_slice(&bytes[o..o+n])` is the
idiom and the Rust test uses exactly that (`net_data_reader_writer_tests.rs:631`). No real loss.

**4. The untyped `PutArray(Array arr, int sz)` overload is not exposed.** `NetDataWriter.cs:300-310`
is `public` and takes any `Array`; the Rust equivalent `put_array_le` is private
(`net_data_writer.rs:225-237`) with typed `put_array_*` wrappers only. A deliberate narrowing — the
untyped form is what allowed the C# to `BlockCopy` an arbitrary array with a caller-supplied element
size.

**5. `recycle()` / `recycle_with()` are no-ops** (`net_data_reader.rs:258-260`). Checked what was
lost: the C# `NetPacketReader.Recycle` body (`BasisNetworkShell.cs:204-215`) is a pool return
through the `RecycleInternal` delegate plus a leftover-bytes warning guarded by `#if UNITY_EDITOR ||
DEVELOPMENT_BUILD` — never compiled into the server. `Bytes` is refcounted, so there is no pool to
return to. The doc comment at `net_data_reader.rs:256-257` states this accurately.

**6. The LiteNetLib bridging constructor is not ported.** `NetDataReader.cs:95-98` and `:120-122`
adapt a `LiteNetLib.Utils.NetDataReader`. Transport difference, not a cut: the Rust transport hands
out `Bytes` directly.

**7. The `Get(out T)` overload family is not ported** (`NetDataReader.cs:143-216`, 14 overloads).
C# `out`-parameter idiom; the Rust `get_*() -> NetResult<T>` plus `NetResultExt::field`
(`net_data_reader.rs:149-158`) covers it with more information, not less.

**8. One defensive path I checked and found unreachable, recorded so it is not rediscovered as a
bug:** `window()` sets `self.position = end` *before* the copy (`net_data_writer.rs:116-123`), and
`write_raw` skips the copy if the window comes back short (`:126-131`), which would leave zero-filled
bytes rather than an error. `resize_if_need` guarantees `end <= data.len()`, so the short window can
only arise from a `usize` overflow in `start.saturating_add(n)`, i.e. a length near `usize::MAX`.
Not reachable with any real payload.

Nothing else: every C# public method on both types has a Rust counterpart or is listed above.

## Improvements

**1. Multi-byte reads are bounds-checked. This is the headline.** In the C#, only `GetByte`
validates against `_dataSize` (`NetDataReader.cs:229-230`, with a comment explaining exactly the
hazard). `GetUShort`/`GetShort`/`GetInt`/`GetUInt`/`GetLong`/`GetULong`/`GetFloat`/`GetDouble`
(`NetDataReader.cs:359-413`), `GetArray<T>`'s length prefix (`:243`), `GetGuid`'s span (`:465`) and
every `Peek*` (`:548-606`) go through `_data.AsSpan(_position)`, which bounds against
`_data.Length` — the *backing array* — not `_dataSize`. Those differ in production: the vendored
reader is built over a pooled LiteNetLib packet buffer via `BasisNetworkShell.cs:181` →
`LNLNetworkImpl.cs:88` → `NetDataReader.cs:95-98` → `LiteNetLib/NetManager.cs:41`
(`SetSource(packet.RawData, headerSize, packet.Size)`), and the pool resets `packet.Size` while
keeping the previous, possibly larger, `RawData` (`LiteNetLib/NetManager.PacketPool.cs:297-300`).
So a truncated packet reads **stale bytes from a previously received packet**. Concretely and
reachably: `BasisServerEventsRouter.HandleAvatarRateChange` does `reader.GetUShort()`
(`BasisServerEventsRouter.cs:84`) with no length check and broadcasts the result to every client
(`:94`), so a 1-byte events packet leaks two pooled bytes to the room. The Rust `take` / `peek_slice`
check `position + n <= data_size` (`net_data_reader.rs:269-292`) and return `ShortRead`; the Rust
handler bails (`basis_network_server/src/core/basis_server_events_router.rs:52-55`). Pinned by
`every_scalar_getter_fails_cleanly_on_one_byte` (`io_errors.rs:23-41`) and
`peek_never_moves_the_cursor_and_fails_on_short_data` (`:43-50`). (Traced through the code, not
executed.)

**2. Allocations are bounded by the remaining data before anything is read.** `check_length`
(`net_data_reader.rs:308-316`) gates `get_array_raw` (`:373-378`), `get_string_array` (`:432`),
`get_string_array_max` (`:440`), `get_string_max` (`:452`), `get_large_string` (`:476`) and
`get_bytes_segment` (`:488`). C# `GetStringArray` (`NetDataReader.cs:323-332`) allocates
`new string[65535]` off a 2-byte prefix before reading a single element. Pinned by
`length_prefix_beyond_available_data_is_refused_before_allocating` (`io_errors.rs:52-83`).
`try_get_string_array` additionally bounds its `with_capacity` by `available_bytes() / 2`
(`net_data_reader.rs:684`).

**3. The remaining-bytes helpers no longer corrupt the cursor.** C# `GetRemainingBytesSegment`
(`NetDataReader.cs:482`) and `GetRemainingBytes` (`:515`) both set `_position = _data.Length` — the
backing array, not `_dataSize`. On a pooled buffer that leaves `Position > _dataSize`, so
`EndOfData` (`_position == _dataSize`, `:53`) is permanently false and `AvailableBytes` (`:58`) goes
negative. Rust sets `position = data_size` (`net_data_reader.rs:503`, `:513`). Live call sites exist
(`RemoteAvatarDataMessage.cs:31`, `AvatarDataMessage.cs:60`, `SceneDataMessage.cs:52`,
`ServerStatisticMessage.cs:20`) but all read last, so the consequence in the C# is latent rather
than a live bug. Pinned by `cursor_movement_is_clamped_to_the_data` (`io_errors.rs:129-139`).

**4. Cursor arithmetic cannot go negative or run off the end.** `available_bytes` and
`user_data_size` saturate (`net_data_reader.rs:211`, `:227`) where the C# subtracts `int`s
(`NetDataReader.cs:38`, `:58`); `skip_bytes` and `set_position` clamp to `data_size`
(`net_data_reader.rs:231-238`) where the C# assigns blind (`NetDataReader.cs:61-69`) and the callers
had to clamp by hand (`ChatMessage.cs:47`, `:59`, `VoiceReceiversMessage.cs:154`);
`set_source_with_offset` clamps both bounds to the buffer (`net_data_reader.rs:247-254`) where the
C# takes `maxSize` on trust (`NetDataReader.cs:87-93`).

**5. `clear()` resets `offset`.** `net_data_reader.rs:700-705` vs `NetDataReader.cs:826-831`, which
leaves `_offset` stale so `UserDataSize` reads negative on a cleared offset reader.

**6. A refused write leaves nothing partial behind.** `put_string_max` validates the `u16` prefix
*before* writing anything (`net_data_writer.rs:328-331`), where the C# writes the UTF-8 bytes at
`_position + 2` first (`NetDataWriter.cs:429`) and only then evaluates `checked((ushort)(size + 1))`
(`:435`), so an over-64K string throws with stale bytes sitting past `_position`.
`put_array_string_max` rolls the whole record back — count prefix included — if any element is
refused (`net_data_writer.rs:281-291`), where the C# `PutArray(string[], int)`
(`NetDataWriter.cs:365-371`) would throw mid-array under an already-written full count. Pinned at
`net_data_reader_writer_tests.rs:276` ("a refused write must leave nothing partial behind").

**7. Truncation cannot split a surrogate pair.** C# `Put(string, maxLength)` cuts at `maxLength`
UTF-16 units (`NetDataWriter.cs:425`) and the encoder has `throwOnInvalidChars: true`
(`NetDataWriter.cs:43`), so a cut landing mid-pair throws `EncoderFallbackException`. Rust cuts on a
`char_indices` boundary (`net_data_writer.rs:317-322`). Pinned by `io_errors.rs:250-259`.

**8. The writer is unconditionally little-endian.** C# `FastBitConverter.GetBytes` stores in *native*
byte order (`Unsafe.As<byte, T>` at `NetDataWriter.cs:452`, `*(T*)ptr` at `:469`) while the reader
decodes with explicit `BinaryPrimitives.Read*LittleEndian` — the pair is only correct on a
little-endian host. Rust uses `to_le_bytes` / `from_le_bytes` throughout
(`net_data_writer.rs:133-182`, `net_data_reader.rs:338-368`). Theoretical on x86-64/ARM-LE, but real.

**9. Errors carry structure instead of a formatted string.** `NetDataError`
(`net_data_reader.rs:36-105`) records a discriminated kind, an optional field name, and a
`#[track_caller]` `Location` — no allocation on the hot path — and converts into a
`Permanent`/`Protocol` `BasisError` with a frame trace (`:137-145`). The C# threw bare
`InvalidOperationException` / `ArgumentException` with an interpolated message. `NetResultExt::field`
(`:149-158`) lets a handler name the field being parsed. Pinned by
`field_names_travel_with_the_error` and `a_wire_error_becomes_a_permanent_protocol_fault_with_a_trace`
(`io_errors.rs:158-189`).

**10. Zero-copy reads that outlive the packet.** The reader holds a `Bytes`
(`net_data_reader.rs:169`), so `get_bytes_segment` (`:487-497`) and `get_remaining_bytes_segment`
(`:500-505`) hand out refcounted views that keep the buffer alive. The C# `ArraySegment<byte>`
(`NetDataReader.cs:470-484`) aliases the pooled array, whose lifetime ends at `Recycle` — retaining
one past recycling would read a reused buffer. I did not find a C# call site that does so; the point
is that the Rust makes it structurally impossible.

**11. Panics are impossible on the malformed-input paths.** Every read goes through a checked
`take`/`peek_slice`; every slice in the writer is produced by `window()` after a `resize_if_need`;
the `Bytes::slice` in `get_bytes_segment` re-checks against `data.len()` (`net_data_reader.rs:491-493`)
even though the type invariant already guarantees it. `io_errors.rs` exists specifically to assert
"error, never a panic" (`io_errors.rs:1-3`).

## Verdict

Yes — this is a faithful port, and in a few places a better one than the original. The wire format
is byte-identical, the C# test suite was carried across almost test-for-test (36 of 38; the one
omission is the non-auto-resize writer mode, which has no production caller), and a second suite of
16 negative tests was added for paths the C# never covered. The two things to know before trusting
it: the Rust refuses over-64K length-prefixed writes where the C# silently wrapped them to an empty
record, which is wire-visible and deliberately contradicts a C# test that pinned the old behaviour;
and invalid UTF-8 is decoded lossily rather than rejected, which is unpinned on both sides and is
the one deviation I would want an explicit decision on. The bounds-checking on multi-byte reads
alone makes the Rust the safer of the two to run in production — the C# leaks stale pooled-buffer
bytes on any truncated packet, and I traced a reachable path where those bytes are broadcast to
every client in the room.
