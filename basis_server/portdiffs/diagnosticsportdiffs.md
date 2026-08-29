# Diagnostics — port diffs

C#: `Basis Server/BasisNetworkCore/Diagnostics/` · Rust: `basis_server/basis_network_core/src/diagnostics/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BNL.cs` | `bnl.rs` | 59 → 86 | deviates |
| — | `mod.rs` | — → 2 | extended (Rust module wiring, no C# analogue) |

`BNL` has no log levels beyond the three entry points, no verbosity filter and no format string:
`Log`/`LogWarning`/`LogError` either hand the raw message to an installed sink or write it to the
console in one colour. Both sides are the same in that respect — `BNL.cs:13`'s
`string formattedMessage = message;` is a no-op the Rust simply drops. Everything level-, format-
and configuration-gated lives in the consumer, `BasisNetworkServer/Diagnostics/BasisServerSideLogging.cs`,
which is outside this file map (one adjacent finding from it is recorded at the end of Deviations).

## Deviations

1. **Multicast delegate collapsed to a single sink.** `BNL.cs:8-10` declares three
   `Action<string>` fields; a .NET `Action` is a multicast delegate, so several subscribers can be
   attached and all of them receive every message. The C# uses that: `BasisServerSideLogging.cs:35-37`
   attaches with `+=`, and the Unity client at
   `Basis/Packages/com.basis.framework/Networking/BasisNetworkConnection.cs:45-50` attaches with the
   `-=` then `+=` idiom. The Rust holds one `Option<LogSink>` per level (`bnl.rs:8-10`) and
   `set_log_output`/`set_log_warning_output`/`set_log_error_output` (`bnl.rs:46-56`) *replace* it.
   Consequences: a second component that installs a sink silently displaces the first instead of
   being added alongside it, and `BasisServerSideLogging::shutdown`
   (`basis_server/basis_network_server/src/diagnostics/basis_server_side_logging.rs:182-184`)
   clears every sink where the C# `-=` would only have detached its own. Why: a single slot is the
   shape the server actually needs (one file/console sink), and it makes double-installation
   harmless — see Improvements. Not pinned by a test; there is no Rust test for `BNL` at all. The
   C# tests only assign the field directly
   (`Basis Server/BasisServerTests/Infrastructure/OwnershipStateAndIdDatabaseTests.cs:55-57`,
   `Networking/ControlAndResourceMessageRoundTripTests.cs:30`), which the Rust API models exactly.

2. **Console colours are one intensity step darker.** `BNL.cs:21,33,45` use
   `ConsoleColor.White`, `.Yellow` and `.Red` — these are entries 15, 14 and 12 of `ConsoleColor`,
   the *bright* half of the 16-colour palette (`Gray`, `DarkYellow` and `DarkRed` are the normal
   half). `bnl.rs:24,33,42` emit SGR `37`, `33` and `31`, which are the normal-intensity codes; the
   bright equivalents are `97`, `93` and `91`. So Rust info lines render as grey rather than white
   and warnings/errors as dark yellow/red. The sibling port applies the mapping correctly for the
   colours it was given — `basis_server_side_logging.rs:57-64` maps DarkMagenta/DarkYellow/DarkRed
   to `35`/`33`/`31` — which is what makes `bnl.rs`'s choice look like a translation slip rather
   than a decision. Cosmetic; nothing pins it.

3. **Colour reset is wider than the C# restore.** `BNL.cs:50,53` saves
   `Console.ForegroundColor` and restores exactly that value afterwards. `bnl.rs:75` closes with
   `\x1b[0m`, which resets *all* SGR attributes — a background colour, bold or inverse the host
   terminal had set is cleared as a side effect of one log line. Cosmetic; nothing pins it.

4. **Line terminator.** `Console.WriteLine` (`BNL.cs:52`) emits `Environment.NewLine`, i.e. CRLF
   on Windows; `writeln!` (`bnl.rs:75,77`) always emits `\n`. Only visible on Windows hosts or
   when stdout is captured byte-for-byte. Nothing pins it.

5. **`ClearConsole` writes escapes even when stdout is not a terminal.** `BNL.cs:55-58` calls
   `Console.Clear()`, which goes through the runtime's console abstraction. `bnl.rs:81-85` writes
   `\x1b[2J\x1b[H` unconditionally — and, unlike `write_with_color` two functions above it
   (`bnl.rs:73`), with no `is_terminal()` guard. Redirect the server's stdout to a file and a
   `clear_console()` call deposits literal escape bytes in it. The inconsistency inside the Rust
   file is the concrete part of this finding; the failure is also swallowed (`let _ =`,
   `bnl.rs:83-84`) where `Console.Clear()` can surface an `IOException`. Nothing pins it.
   `clear_console` has no caller in the Rust tree.

6. **A lock is held across the sink call.** `bnl.rs:21,30,39` are
   `if let Some(sink) = LOG_OUTPUT.read().clone() { sink(message) }`. The `RwLockReadGuard` is a
   temporary of the `if let` scrutinee, so it lives for the whole consequent block (edition 2024,
   `basis_server/Cargo.toml:26`) — the sink runs with the read lock still held. A sink that logged,
   or that called `set_log_output`, would deadlock against `parking_lot`'s writer-preferring
   `RwLock`. The C# delegate invocation takes no lock at all, so this hazard is new. It is latent,
   not live: the only sink installed anywhere (`basis_server_side_logging.rs:104-106`) never
   re-enters `BNL` — its file-write failure path deliberately uses `eprintln!` with the comment
   "do not recurse into BNL" — and `shutdown` releases the `WRITER` mutex before touching `BNL`,
   so there is no lock-order inversion today. Not pinned by a test.

**Adjacent finding, outside this file map** (recorded here because it is the log-configuration
gating this module feeds): `BasisServerSideLogging.cs:203-210` early-returns only when *both*
`WriteToScreen` and `UseLogging` are false, then calls `WriteScreenLine` unconditionally — so with
`WriteToScreen = false` and `UseLogging = true` the C# still writes to the screen. The Rust guards
the screen write properly (`basis_server_side_logging.rs:220-228`, `if to_screen`). The Rust
behaviour is the intended one; the C# is arguably the bug. It belongs in that module's diff, not
this one.

## Corners cut

* **No tests.** The Rust has no test touching `BNL` — not the sink-installed path, not the
  console-fallback path, not `clear_console`. The C# had none either, so this is a preserved gap
  rather than a regression, but the port standard's "negative tests for every failure path" is not
  met here: `set_log_output(None)` restoring console output, and the sink-replaces-sink behaviour
  from deviation 1, are both untested.
* **Three getters that nothing uses.** `bnl.rs:58-68` exposes `log_output()`,
  `log_warning_output()` and `log_error_output()` to model the C# fields being publicly readable.
  No caller in the tree reads them. Harmless, but they are API surface carried for symmetry only.
* Nothing else was simplified: the three-level shape, the null-sink fallback to console, and the
  colour-per-level choice are all present.

## Improvements

* **The check-then-invoke race is gone.** `BNL.cs:15-17` reads the `LogOutput` field twice — once
  for the null check, once for `Invoke`. If another thread nulls the field between the two reads
  the C# throws `NullReferenceException`; the tests do exactly this kind of reassignment
  (`OwnershipStateAndIdDatabaseTests.cs:55-57`). `bnl.rs:21` clones the `Arc` out of the lock once
  and then calls the clone, so it either calls a live sink or falls back to the console. Same for
  `LogWarning` (`BNL.cs:27-29`) and `LogError` (`BNL.cs:39-41`).
* **Console writes are atomic per line.** `BNL.cs:48-54` mutates the process-global
  `Console.ForegroundColor` with no lock, so two threads logging concurrently interleave both the
  colour changes and the text. `bnl.rs:71-78` takes the stdout lock for the whole line and emits a
  self-contained escape-text-reset sequence, so colours cannot be torn across threads.
* **Colour is suppressed when redirected, explicitly.** `bnl.rs:73` checks `is_terminal()` and
  writes plain text otherwise, so piping the server's output never produces escape bytes in the
  captured stream (except via `clear_console`, deviation 5). The C# leaves this to the runtime's
  console layer.
* **Installing a sink twice is idempotent.** Because the Rust replaces rather than appends,
  calling `BasisServerSideLogging::initialize` twice cannot produce duplicated log lines the way
  a repeated `BNL.LogOutput += Log` (`BasisServerSideLogging.cs:35-37`) would.

## Verdict

The structure is a faithful port — three levels, an installable sink per level, console fallback
with a colour each — and the Rust fixes a real check-then-invoke race and a real colour-tearing
race that the C# has. The substantive behavioural change is the collapse of a multicast delegate
to a single sink slot, which is defensible for this server but means a second subscriber is
silently displaced and `shutdown` detaches everyone. The rest are cosmetic (colour intensity, the
wider reset, CRLF) plus one internal inconsistency worth fixing: `clear_console` writes escapes
without the `is_terminal` guard its sibling function uses. No test covers any of it on either side.
