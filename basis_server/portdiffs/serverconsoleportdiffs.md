# BasisServerConsole — port diffs

C#: `Basis Server/BasisServerConsole/` · Rust: `basis_server/basis_server_console/src/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `Program.cs` | `main.rs` | 152 → 244 | ported; boot order identical, shutdown moved onto the main thread, no unhandled-exception hooks |
| `BasisConsoleCommands.cs` | `basis_console_commands.rs` | 720 → 714 | ported; same command set, `/config` reads and writes through `NetworkServer` instead of a captured object |
| `BasisConsoleDriver.cs` | `basis_console_driver.rs` | 445 → 574 | ported for unix only; the Windows cursor-API branch is gone, raw-mode entry/restore added |
| `BasisSetupWizard.cs` | `basis_setup_wizard.rs` | 320 → 337 | ported; identical prompts and text, one panic on non-ASCII input |
| `BasisFirstBootTuning.cs` | `basis_first_boot_tuning.rs` | 284 → 248 | ported; also finds the Rust-named binaries |
| — | (tests inside the above) | — | 3 Rust unit tests; the C# project has no tests anywhere in the solution |

`BasisNetworkConsole.csproj` → `Cargo.toml` (the csproj's `CopyBenchmarkTooling` target that stages the
benchmark and load client beside the server has no cargo equivalent; nothing in the Rust tree does that
staging, so `basis_first_boot_tuning.rs` only finds the tools if the operator put them there).

## Boot sequence, step by step

Both sides run the documented order, and every step lands in the same place. `main.rs:3-5` states the
order; the code matches it.

| # | step | C# | Rust | same? |
| --- | --- | --- | --- | --- |
| 1 | predecessor wait | `Program.cs:20` | `main.rs:63` | yes |
| 2 | config dir created, first-boot flag captured before load | `Program.cs:22-31` | `main.rs:65-73` | yes |
| 3 | config load | `Program.cs:32` | `main.rs:74` | yes |
| 4 | tuning profile applied | `Program.cs:43` | `main.rs:89` | yes |
| 5 | environment overrides | `Program.cs:45` | `main.rs:90` | yes |
| 6 | logging initialised | `Program.cs:47-48` | `main.rs:92-96` | yes |
| 7 | first-boot wizard | `Program.cs:54` | `main.rs:101` | yes |
| 8 | first-boot tuning, then re-read + profile + overrides | `Program.cs:58-68` | `main.rs:105-118` | yes |
| 9 | "Server Booting" + health check | `Program.cs:70-71` | `main.rs:121-128` | yes |
| 10 | REST API when `ApiEnabled` and a key is set | `Program.cs:73-75` | `main.rs:129-139` | yes |
| 11 | network server start | `Program.cs:77` | `main.rs:141` | yes |
| 12 | legacy resource-directory migration | `Program.cs:81-108` | `main.rs:153`, `:194-214` | yes |
| 13 | loadable + default library XML | `Program.cs:109-110` | `main.rs:154-155` | yes |
| 14 | exit/signal handler registered | `Program.cs:112` | `main.rs:157` | yes |
| 15 | console commands registered, listener started | `Program.cs:128-139` | `main.rs:159-169` | yes |
| 16 | wait for shutdown | `Program.cs:141` | `main.rs:172` | yes |

The two order-critical relationships the C# comment at `Program.cs:36-42` calls out are both preserved:
the tuning profile is applied *before* `ProcessEnvironmentalOverrides` (`main.rs:89` before `:90`), and
the post-tuning path re-reads config.xml from disc rather than reusing the in-hand object
(`main.rs:110-114` mirrors `Program.cs:60-67`). An operator's environment override therefore stays a
per-run pin on both sides and is never persisted into config.xml.

## Console commands

Registration is the same list in the same order on both sides: `/players`, `/status`, `/shutdown`,
`/restart`, `/help`, `/clear` (`Program.cs:129-134` / `main.rs:160-165`), then
`RegisterPermissionCommands` / `register_permission_commands` (28 entries, identical names, identical
descriptions, identical help text — `BasisConsoleCommands.cs:143-184` / `basis_console_commands.rs:151-191`),
then the `/config` family.

No command exists on one side and not the other, with one exception that comes from the config type
rather than from this module: `/config` registers one subcommand per public `Configuration` field, and
the Rust `Configuration` has 94 fields against the C# 93 — the extra is `MaxOwnedObjectsPerPlayer`
(`basis_server/basis_network_core/src/configuration/basis_server_configuration.rs:96`, no counterpart
in `Basis Server/BasisNetworkCore/Configuration/BasisServerConfiguration.cs`). So `/config
maxownedobjectsperplayer` exists only in Rust, and the `/config` header line reads "94 settings"
(`basis_console_commands.rs:77`) where the C# reads "93 settings" (`BasisConsoleCommands.cs:38`).

## Deviations

**1. `looks_like_did` panics on non-ASCII input, aborting first boot.**
`basis_setup_wizard.rs:281` is `value.len() > "did:".len() && value[..4].eq_ignore_ascii_case("did:")`.
`value[..4]` is a byte slice, so it panics when byte 4 is not a char boundary. The C# equivalent,
`BasisSetupWizard.cs:283`, is `value.StartsWith("did:", StringComparison.OrdinalIgnoreCase)` and cannot
fail. The function is called on raw operator input at `basis_setup_wizard.rs:146`, inside the admin
prompt loop, which runs from `main.rs:101` on the main thread — outside the `catch_unwind` that guards
console commands (`basis_console_commands.rs:542`). Typing a UUID beginning with three ASCII characters
and a multi-byte one ends the boot. Verified by extracting the function and running it: `"abc€xyz"`
gives `end byte index 4 is not a char boundary; it is inside '€' (bytes 3..6 of string)`. Not pinned:
the test at `basis_setup_wizard.rs:328-336` uses ASCII inputs only.

**2. `/config <field> <value>` writes through a different object.**
C# captures the one live `Configuration` in the command closure (`BasisConsoleCommands.cs:21`, `:28`)
and mutates it in place (`:90`). Rust clones the server's current configuration, edits the clone, and
installs it (`basis_console_commands.rs:122`, `:134`). Deliberate — it makes `NetworkServer` the single
source of truth — but it leaves `main.rs`'s local `config` stale, and that local is what the shutdown
path reads at `main.rs:183` (`if config.enable_statistics`). In C# the same read at `Program.cs:125`
sees the operator's change. Effect is confined to whether `BasisStatistics::stop_worker_thread` is
called after a live `/config EnableStatistics` flip. Not pinned by a test.

**3. `/shutdown` and `/restart` no longer call `exit()`.**
`BasisConsoleCommands.cs:612-613` and `:688-689` set `isRunning = false` then `Environment.Exit(0)`,
which relies on the `ProcessExit` handler at `Program.cs:112-127` to clean up. Rust calls
`Program::request_shutdown()` (`basis_console_commands.rs:571`, `:641`), which wakes the main thread and
runs the shutdown block at `main.rs:174-189`. Deliberate; see Improvements. Not pinned.

**4. The predecessor wait does nothing off unix.**
`basis_console_commands.rs:599-611`: `process_exists` is `libc::kill(pid, 0)` on unix and a hard `false`
under `cfg(not(unix))`, so `wait_for_predecessor_exit` (`:577`) returns immediately on Windows.
`BasisConsoleCommands.cs:634-639` uses `Process.GetProcessById` + `WaitForExit(30000)` on every
platform. On unix the Rust version also polls every 100 ms (`:595`) rather than waiting on a handle,
so a restart can take up to 100 ms longer than it needs to. Partly pinned:
`basis_console_commands.rs:709-713` covers only the "already gone / malformed / absent" paths.

**5. The console driver is unix-only.**
C# runs on both, with an ANSI path and a Windows cursor-API path selected at `BasisConsoleDriver.cs:30`
and branched at `:263-271` and `:293-308`. The Rust driver is ANSI-only (`basis_console_driver.rs:332-350`),
and `stdin_is_terminal` / `stdout_is_terminal` (`:394-416`) return `false` under `cfg(not(unix))`. So on
Windows `interactive` is never set, `read_line` falls straight to `read_plain_line` (`:153`), and the
operator loses history, in-place editing and the log-line-above-the-prompt behaviour the class exists
for. `BasisSetupWizard::can_prompt` (`basis_setup_wizard.rs:299-309`) and
`BasisFirstBootTuning::stdin_is_terminal` (`basis_first_boot_tuning.rs:219-229`) are the same shape, so
on Windows the first-boot wizard silently takes the `warn_no_admin` path and tuning is never offered —
where `Console.IsInputRedirected` (`BasisSetupWizard.cs:312`, `BasisFirstBootTuning.cs:165`) works
everywhere. Not pinned.

**6. Console output interception is narrower.**
C# swaps `Console.Out` for an `InterceptingWriter` (`BasisConsoleDriver.cs:54`, class at `:425-443`), so
*anything* written to stdout erases and redraws the input line. Rust registers a sink on the logging
path only (`basis_console_driver.rs:96` → `BasisServerSideLogging::set_console_sink`). In practice
equivalent: `grep` finds no `println!`/`print!` on any post-boot path in `basis_network_server/src`
(the one direct write is an `eprintln!` in the log-file error handler,
`basis_server_side_logging.rs:168`). Any future direct stdout write would corrupt the prompt where the
C# would have absorbed it.

**7. No unhandled-exception or unobserved-task routing.**
`Program.cs:17-18` installs `AppDomain.CurrentDomain.UnhandledException` and
`TaskScheduler.UnobservedTaskException`, both logging through `BNL` (`Program.cs:141-150`) and therefore
into the server log file. `main.rs` installs neither a panic hook nor anything equivalent; a panic on a
worker thread goes to stderr through the default hook and never reaches the log file. Not pinned.

**8. Failure at boot is tolerated rather than fatal.**
`Program.cs:71` and `:75` construct the health check and REST handler directly; a throw escapes to the
unhandled-exception handler and the process dies. `main.rs:122-139` logs and continues without them,
and `main.rs:141-151` exits 1 after an orderly teardown when the network server itself will not start.
Deliberate, and stated in the surrounding comments. Not pinned.

**9. Cosmetic message differences.**
(a) Parse failure: `BasisConsoleCommands.cs:86` prints `Expected {DescribeType(...)}` with the helper at
`:136-141` rendering `true or false` / `one of [A, B]` / the CLR type name; `basis_console_commands.rs:124`
prints the `ConfigFieldError` text instead (`configuration/mod.rs:43-48`), e.g. `'X' cannot be set to
'v': 'v' is not a valid i32`. (b) Command failure: `BasisConsoleCommands.cs:573` includes `ex.Message`;
`basis_console_commands.rs:543` prints only the command name, because `catch_unwind` gives it nothing
else. (c) Prompt colour: C# saves and restores the previous foreground (`BasisConsoleDriver.cs:322-330`);
Rust emits a fixed `\x1b[0m` reset (`basis_console_driver.rs:22`, used at `:304`).

**10. Rust accepts Ctrl-D as end of input.** `basis_console_driver.rs:489` maps `0x04` to `Key::Eof`,
ending the reader loop. C# reaches `char.IsControl` at `BasisConsoleDriver.cs:197` and ignores it.

## Corners cut

* Windows. Four separate `cfg(not(unix))` dead ends: the console driver (`basis_console_driver.rs:394-416`,
  `:441-443`), the wizard prompt (`basis_setup_wizard.rs:305-308`), the tuning prompt
  (`basis_first_boot_tuning.rs:225-228`) and the predecessor wait (`basis_console_commands.rs:606-610`).
  Each degrades to a defensible fallback rather than misbehaving, but a Windows operator gets a plainly
  worse server console than the C# gave them, and gets neither the first-boot wizard nor first-boot
  tuning at all.
* The non-ANSI erase/move code (`BasisConsoleDriver.cs:263-271`, `:293-308`) and the `Console.BufferWidth`
  / `Console.SetCursorPosition` wrappers (`:332-361`) have no port.
* `catch_unwind` loses the failure text that C#'s `catch (Exception ex)` printed
  (`basis_console_commands.rs:542-544` vs `BasisConsoleCommands.cs:567-574`).
* The csproj's `CopyBenchmarkTooling` target has no build-system equivalent, so
  `BasisFirstBootTuning::run` will normally find nothing to run in a cargo-built tree.

## Improvements

* **Orderly shutdown on every path.** `main.rs:174-189` restores the terminal, stops the REST API, stops
  the health check listener, shuts the reduction system down, stops statistics, stops the network server
  and flushes the log — reached from `/shutdown`, `/restart`, SIGINT and SIGTERM alike. C# stops the API,
  the reduction system and statistics only (`Program.cs:113-126`): it never stops the health-check
  listener and never stops the network server, leaving both to process teardown. `/restart` in particular
  is safer for it, because the successor's socket bind is being waited on by a process that has actually
  closed its socket.
* **The terminal is handed back.** `BasisConsoleDriver::restore` (`basis_console_driver.rs:101-113`) leaves
  raw mode and un-hooks the sink. The C# never entered raw mode, but it also never restored `Console.Out`.
* **Explicit signal handling.** `main.rs:218-244` routes SIGINT and SIGTERM into the same shutdown, and
  says so when it cannot install them.
* **`/config` persists before it applies.** `basis_console_commands.rs:128-134` saves the edited clone and
  only then installs it, so a failed write leaves the running configuration untouched. C# sets the field,
  saves, and reverts on throw (`BasisConsoleCommands.cs:90-101`) — a window in which the live
  configuration holds a value that never reached disc.
* **`/perm` reports I/O failures.** Every handler matches on the result and logs (e.g.
  `basis_console_commands.rs:255-260`, `:275-280`, `:294-300`). The C# equivalents call straight into the
  manager (`BasisConsoleCommands.cs:247`, `:267`, `:286`) and rely on the dispatcher's catch-all.
* **Boot failures are diagnosable rather than fatal.** See Deviation 8.
* **First-boot tuning finds the Rust binaries too.** `basis_first_boot_tuning.rs:24-25` searches both
  `basis_server_benchmark`/`basis_network_client_console` and the C# names, against a single hardcoded
  name each in `BasisFirstBootTuning.cs:35-36`.
* **Tests exist at all.** `basis_console_commands.rs:667-713` pins longest-prefix dispatch, argument
  splitting, case-insensitive re-registration in place, and the predecessor-wait early exits;
  `basis_setup_wizard.rs:328-336` pins DID detection. Nothing in the C# solution tests any of these five
  files.

## Verdict

A faithful port. The boot sequence is step-for-step identical, including both of the ordering
relationships the C# comments flag as load-bearing, so an operator's config.xml, tuning profile and
environment overrides compose exactly as before. The command surface is identical bar one extra
`/config` subcommand that comes from a core-module config field, not from here.

Two things to fix. The `value[..4]` slice in `basis_setup_wizard.rs:281` is a real panic on operator
input during first boot and should be a `starts_with`. The Windows `cfg(not(unix))` fallbacks are a
deliberate scope cut, but the one in `wait_for_predecessor_exit` is the only one that fails silently
into a race rather than into a fallback, and it is worth either implementing or documenting at the
call site.
