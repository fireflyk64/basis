# BasisBenchAgent — port diffs

C#: `Basis Server/BasisBenchAgent/` · Rust: `basis_server/basis_bench_agent/src/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BenchAgentProtocol.cs` | `bench_agent_protocol.rs` | 76 → 108 | ported; same port, same version, same field names |
| `Program.cs` | `main.rs` | 405 → 501 | ported; XML patching is textual, CPU sampling is Linux-only |
| `LaunchTarget.cs` | `launch_target.rs` | 126 → 97 | ported; `TryResolve` folded into the caller |
| `BasisBenchAgent.csproj` | `Cargo.toml` | — | — |
| tests | in-file `#[cfg(test)]` | 0 → 5 | the C# project has no tests |

The only consumer of this protocol in either tree is the C# `BasisServerBenchmark`
(`Basis Server/BasisServerBenchmark/Harness/LoadClientDriver.cs:461-547`); there is no Rust
benchmark, so interop means "the C# benchmark drives either agent".

## Protocol, field by field

`BenchAgentProtocol.cs:28` / `bench_agent_protocol.rs:14`: `DefaultPort` 4297 on both.
`BenchAgentProtocol.cs:31` / `bench_agent_protocol.rs:16`: `Version` 1 on both. Both check the
version before dispatching any command, so even `hello` is refused on a mismatch
(`Program.cs:131-137` / `main.rs:159-165`).

**AgentRequest** (`BenchAgentProtocol.cs:34-48` / `bench_agent_protocol.rs:27-50`)

| JSON name | C# type / default when absent | Rust type / default when absent | match |
| --- | --- | --- | --- |
| `cmd` | string, `""` | String, `""` | yes |
| `version` | int, `1` | i32, `1` (`default_version`) | yes |
| `clients` | int, `0` | i32, `0` | yes |
| `host` | string, `""` | String, `""` | yes |
| `port` | int, `0` | i32, `0` | yes |
| `connectIntervalMs` | int, `1` | i32, `1` (`default_connect_interval`) | yes |

Both sides ignore unknown properties and are case-sensitive on names, so requests interoperate in
both directions.

**AgentResponse** (`BenchAgentProtocol.cs:50-76` / `bench_agent_protocol.rs:52-84`)

| JSON name | C# type / default | Rust type / default | match |
| --- | --- | --- | --- |
| `ok` | bool, `false` | bool, `false` | yes |
| `error` | string?, `null`, **always written** | `Option<String>`, **omitted when none** | see Deviation 1 |
| `version` | int, `1` | i32, `1` | yes |
| `agent` | string?, `null`, always written | `Option<String>`, omitted when none | see Deviation 1 |
| `cores` | int, `0` | i32, `0` | yes |
| `os` | string?, `null`, always written | `Option<String>`, omitted when none | see Deviation 1 |
| `running` | bool, `false` | bool, `false` | yes |
| `clientCores` | double, `0` | f64, `0.0` | value encoding differs — Deviation 2 |
| `voiceDelivered` | double, **`-1`** | f64, **`0.0` when the key is absent** | see Deviation 3 |

Commands are the same four (`hello`, `start`, `status`, `stop`), lower-cased before dispatch on both
sides, with the same unknown-command error text (`Program.cs:139-172` / `main.rs:166-182`). Framing
is the same: one JSON object per line, blank lines skipped, the response written and flushed
immediately. The C# writes `Environment.NewLine` and the Rust writes `\n`; `BufRead::lines` strips a
trailing `\r` and `StreamReader.ReadLine` accepts a bare `\n`, so both directions parse.

## Deviations

**1. Null-valued response fields are omitted rather than written.**
`error`, `agent` and `os` are `[JsonPropertyName]`-mapped nullable strings in C#, and
System.Text.Json's default is to write `"error":null`. `bench_agent_protocol.rs:55`, `:61` and `:65`
carry `skip_serializing_if = "Option::is_none"`, so those keys are absent instead. Both parsers treat
an absent key and a null one the same way for these fields, so interop holds in both directions;
what changes is what a human sees with netcat, which the protocol's own doc comment lists as a
design goal. Pinned by `bench_agent_protocol.rs:106` (`assert!(!json.contains("\"error\""))`).

**2. A non-finite `clientCores` is encoded differently, and neither encoding is good.**
`ProcessCpuSampler` returns `NaN` on both sides when the CPU read fails — deliberately, so a failed
read cannot read as "the load generator is free" (`Program.cs:356-361` / `main.rs:406-409`).
`Program.cs:161` puts that straight into the response, and System.Text.Json rejects non-finite
doubles by default, so serialization at `Program.cs:119` throws; that line sits *outside* the
try/catch that starts at `:107`, so the exception unwinds out of `Serve`, skips the
connection-close `StopClient()` at `:126`, and is caught by the accept loop at `:80` as
"connection failed" — the control connection dies with the load client still running.
`main.rs:175` puts the same `NaN` into the response, and `serde_json` writes non-finite floats as
`null` (verified in `serde_json-1.0.151/src/ser.rs:169-180`), so the reply is
`"clientCores":null`. The C# benchmark then fails to deserialize it into a non-nullable `double`,
and `LoadClientDriver.Poll` (`LoadClientDriver.cs:491-505`) catches that, sets `_cores = NaN` and —
importantly — leaves `_voice` un-refreshed for that poll. So both agents mishandle a NaN sample; the
Rust one keeps the connection and loses one status reading, the C# one drops the connection and
orphans the load client. Neither is pinned by a test. On Linux with a live child, `try_read`
(`main.rs:444-462`) succeeds and the value is finite, so this is reachable mainly after the child
exits and on non-Linux hosts (Deviation 4).

**3. `voiceDelivered` defaults differently when the key is absent.**
`BenchAgentProtocol.cs:75` initialises the property to `-1`, so a response object missing that key
deserializes to `-1` — the documented "unknown". `bench_agent_protocol.rs:76` uses plain
`#[serde(default)]`, which for `f64` is `0.0`, not the `-1.0` in the struct's own `Default` impl at
`:82` (a field-level `default` uses `Default::default()` for the field's type, not the container's).
Both agents always serialize the key, so the two current implementations do not hit it; a
hand-written or older peer that omits it would be read as "0% of voice delivered" instead of
"unknown". Not pinned.

**4. CPU sampling is Linux-only.**
`Program.cs:393-404` reads `Process.TotalProcessorTime`, which works on every platform .NET runs on.
`main.rs:444-462` parses `/proc/<pid>/stat` under `#[cfg(target_os = "linux")]` and returns `None`
otherwise, so on macOS or Windows `sample_cores` always yields `NaN` and every `status` reply carries
`"clientCores":null` — which, per Deviation 2, the C# benchmark cannot parse. An agent built for a
non-Linux load box is therefore not usable with the existing benchmark. Not pinned.

**5. `ClientSimConfig.xml` is patched textually rather than through an XML parser.**
`Program.cs:238-256` loads the file with `XDocument`, matches child elements by `LocalName` (so a
namespaced document still works), assigns `element.Value` (escaping handled by the writer), appends a
real `XElement` when the setting is missing, and saves.
`main.rs:258-276` searches the raw text for the first `<Name>` and the first `</Name>` and replaces
between them, then inserts before the last `</Configuration>` when that fails. Consequences: a
comment containing the literal `<Ip>` or `<Port>` ahead of the real element would be patched instead
of it; a self-closing `<SimulateVoice/>` would be appended a second time rather than replaced; a
namespaced document is not handled. None of these occur in the document the load client actually
writes — its comments mention `<Password>`, `<SetPort>` and `<AvatarUrl>`, and none of the five
patched names (`ClientCount`, `Ip`, `Port`, `ClientConnectIntervalMs`, `SimulateVoice`) appear inside
a comment — so it works today and is brittle by construction. The escaping set (`&`, `<`, `>` at
`main.rs:262`) matches what the C# writer escapes in text content, and the temp-then-rename write is
the same. Partly pinned: `main.rs:478-486` covers replace, append and the missing-root error.

**6. The Rust agent will launch either load client; the C# agent will only launch the C# one.**
`Program.cs:56`, `:184` and `:325` hardcode `"BasisNetworkClientConsole"`. `main.rs:30` searches
`["basis_network_client_console", "BasisNetworkClientConsole"]`, Rust name first, so a directory
holding both starts the Rust client. Combined with the load clients defaulting to different
transports (see `clientconsoleportdiffs.md`), which agent starts which binary decides which protocol
the crowd speaks. Not pinned.

**7. The child's stderr is discarded at the OS level rather than drained.**
`Program.cs:198`, `:209-211` redirects both pipes and attaches an empty handler to stderr, which
matters because an un-drained redirected pipe blocks the child once its buffer fills.
`main.rs:209` uses `Stdio::null()` for stderr, so the writes go nowhere and nothing can block. Same
visible result (nothing is printed either way), different mechanism.

**8. `Kill` is not tree-wide.** `Program.cs:274` uses `process.Kill(entireProcessTree: true)`;
`main.rs:282` uses `Child::kill`, which signals only the direct child. The load client spawns no
children today, so nothing is orphaned in practice.

**9. Signal handling.** `Program.cs:70-71` hooks `CancelKeyPress` (with `e.Cancel = false`, letting
the default termination proceed after `StopClient`) and `ProcessExit`. `main.rs:343-377` installs
plain SIGINT/SIGTERM handlers that set a flag, with a watcher thread polling it every 100 ms before
running the stop and calling `exit(0)` — so up to 100 ms of latency, the handler runs once and then
the watcher returns, and under `cfg(not(unix))` no handler is installed at all. A Ctrl-C on a
Windows agent therefore stops the agent without stopping its load client.

**10. Small message and discovery differences.**
(a) The C# reports `RuntimeInformation.OSDescription` in the `hello` reply (`Program.cs:330`); the
Rust reads `/proc/sys/kernel/osrelease` and falls back to `"<os> <arch>"` (`main.rs:333-341`) — same
field, different string. (b) `LaunchTarget.TryResolve` (`LaunchTarget.cs:57-76`) reported
`no directory '<dir>'` distinctly from a resolve failure; `main.rs:197-203` inlines the lookup and
reports `no load client under '<dir>'` for both. (c) `cores` comes from
`Environment.ProcessorCount` against `std::thread::available_parallelism` (`main.rs:329-331`).
(d) The unparseable-request error carries the serde message in Rust (`main.rs:144`) where the C#
said only "unparseable request" for a literal `null` line (`Program.cs:111`).

## Corners cut

* `/proc`-only CPU sampling (Deviation 4) — the one that actually restricts where the Rust agent can
  be deployed.
* No Windows signal handling (Deviation 9).
* Textual rather than parsed XML patching (Deviation 5).
* `LaunchTarget::resolve` and the `TryResolve` split survive in `launch_target.rs` but are unused by
  the agent (hence the crate-level `allow(dead_code)` at `main.rs:14`).

## Improvements

* **A bad `--port` no longer silently binds an ephemeral port.** `Program.cs:47` is
  `int.TryParse(Next(), out port)`, and `int.TryParse` writes `0` to its out parameter on failure —
  so `--port abc` makes the C# agent listen on port 0 and print "listening on 0.0.0.0:0".
  `main.rs:71-75` only assigns on a successful parse, and parses into `u16`, so both a
  non-numeric and an out-of-range port keep the 4297 default.
* **A bad `--bind` is reported instead of crashing.** `Program.cs:62` calls `IPAddress.Parse(bind)`,
  which throws on a hostname or a typo and takes the process down with an unhandled exception before
  anything is listening. `main.rs:93-99` binds through `TcpListener::bind((host, port))`, which
  accepts a hostname and reports the failure with the address and the OS error.
* **A poisoned mutex cannot wedge the agent.** Every lock is taken as
  `.unwrap_or_else(|p| p.into_inner())` (`main.rs:170`, `:227`, `:279`).
* **The response is always writable.** `main.rs:146` falls back to a fixed error object if
  serialization fails, and a failed write breaks the loop rather than escaping the connection
  handler — so the connection-close `stop_client` at `:155` always runs. The C# path escapes on a
  serialization failure and skips its equivalent (Deviation 2).
* **`ensure_executable` mirrors the read bits the same way** (`launch_target.rs:45-57` matches
  `LaunchTarget.cs:106-110`, including the `wanted == mode` fallback to user-execute) and the test at
  `launch_target.rs:78-96` pins 0640 → 0750.
* **Tests exist.** Five: wire names and request defaults (`bench_agent_protocol.rs:96-107`), the
  `[VOICE]` line parser including the reject cases (`main.rs:469-475`), config patch replace/append
  and the missing-root error (`:477-486`), version refusal plus the `hello`/`start`/unknown paths
  (`:488-500`), and the execute-bit repair. The C# project has none.

## Verdict

The protocol itself is a faithful port: same port number, same version, same four commands, same
JSON names, same request defaults, same line framing, same version-before-dispatch refusal. A C#
benchmark and a Rust agent interoperate on every normal exchange, and so do a Rust benchmark (if one
is written) and a C# agent.

One case does not interoperate, and it is worth fixing before the agent is relied on: a non-finite
`clientCores`. Rust sends `"clientCores":null`, which the C# benchmark's non-nullable `double`
cannot parse, and the C# agent's own handling of the same value is worse (it drops the connection and
orphans the load client). Sending `-1` — the value the C# already uses for "no sampler" at
`Program.cs:161` — in place of `NaN` on both sides would close it. Second, `/proc`-only CPU sampling
means the Rust agent reports nothing usable off Linux, which turns that same case from an edge into
the normal path on a macOS or Windows load box. Everything else is cosmetic or a deliberate
hardening of an input the C# handled badly.
