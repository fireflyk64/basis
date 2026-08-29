# RestApi — port diffs

C#: `Basis Server/BasisNetworkServer/RestApi/` · Rust: `basis_server/basis_network_server/src/rest_api/`

## File map

| C# | Rust | lines (C#→Rust) | status |
|---|---|---|---|
| `BasisRestApiHandler.cs` | `basis_rest_api_handler.rs` | 141 → 166 | ported; identical auth, one body-buffering order change |
| `BasisRestApiRoutes.cs` | `basis_rest_api_routes.rs` | 294 → 343 | ported; same routes, statuses and response shapes |
| `BasisServerInfoQuery.cs` | `basis_server_info_query.rs` | 170 → 156 | ported; identical wire format and rate limits |
| — | `mod.rs` | — → 9 | Rust module wiring |

Route table, both sides (`BasisRestApiRoutes.cs:29-79` vs `basis_rest_api_routes.rs:51-87`):

| route | methods | C# | Rust |
|---|---|---|---|
| `POST /api/announce` | POST (else 405 `Allow: POST`) | `:39-43` | `:56-61` |
| `POST /api/announce/{uuid}` | POST | `:41-42` | `:60` |
| `GET /api/players` | GET (else 405 `Allow: GET`) | `:45-48` | `:62-67` |
| `GET /api/worlds` | GET | `:59` | `:73` |
| `POST /api/worlds` | POST | `:60` | `:74` |
| `DELETE /api/worlds` | DELETE | `:62` | `:76-77` |
| `DELETE /api/worlds/{netId}` | DELETE | `:63` | `:78-79` |
| `POST /api/worlds/switch` | POST (else 405 `Allow: POST`) | `:51-56` | `:69-71` |
| anything else under `/api/` | — | 404 `{"error":"not found"}` `:69-70` | 404 `{"error":"not found"}` `:85` |
| anything not `/api/<resource>` | — | 404 empty body `BasisRestApiHandler.cs:93-98` | 404 empty body `basis_rest_api_handler.rs:115-117` |

Method dispatch order matters and matches: on `/api/worlds` the `switch` id is checked before the
method table on both sides, so `GET /api/worlds/switch` is 405 with `Allow: POST` rather than a
world listing.

## Deviations

**R1 — the request body is buffered before the API key is checked.** C# authenticates first
(`BasisRestApiHandler.cs:82-88`) and only reads the body inside a route, from the request stream,
with a cap (`BasisRestApiRoutes.cs:249-268`). The Rust handler takes `body: Bytes` as a handler
argument (`basis_rest_api_handler.rs:105`), so axum runs the body extractor to completion before
the handler body — and therefore before `authenticate` at `:110`. An unauthenticated caller can
make the server buffer a whole request body. It is bounded: axum's default request-body limit
(2 MiB in axum 0.8; no `DefaultBodyLimit` layer is configured anywhere in the tree) times the
32-permit concurrency cap at `:106`, so roughly 64 MiB worst case. No test.

**R2 — where the 413 comes from for an oversized body.** C# returns
`{"error":"payload too large"}` with 413 both from the `Content-Length` pre-check
(`BasisRestApiRoutes.cs:251`) and from the streaming counter (`:259`). Rust checks
`body.len() > MAX_BODY_BYTES` (`basis_rest_api_routes.rs:317`) and returns the same JSON 413 —
but only for bodies axum accepted. Above axum's own limit the rejection is axum's, with a
plain-text body instead of the JSON envelope. Same status class, different body. The Rust test
accepts either outcome (`basis_rest_api_tests/tests/rest_api_tests.rs:420-426`).

**R3 — message length is counted in different units.** C# uses `String.Length`, UTF-16 code
units (`BasisRestApiRoutes.cs:193` for `message`, `:168` for `announceMessage`). Rust uses
`chars().count()`, Unicode scalar values (`basis_rest_api_routes.rs:248, 219`). A 512-character
message of astral-plane codepoints (emoji) is 1024 units in C# and rejected, 512 scalars in Rust
and accepted. The limit constant is 512 on both (`BasisRestApiRoutes.cs:16` /
`basis_rest_api_routes.rs:40`). Pinned only for the ASCII case
(`RestApiTests.cs:220` / `rest_api_tests.rs:210`).

**R4 — the https check is a different parser.** C# builds a `Uri` and compares the scheme
(`BasisRestApiRoutes.cs:239-245`). Rust does a lowercased `https://` prefix match and then
requires a non-empty, whitespace-free host (`basis_rest_api_routes.rs:305-312`). The common cases
agree. At the edges they do not: the C# accepts anything `Uri.TryCreate` parses as absolute with
scheme https, including an empty authority, while the Rust rejects that and rejects some strings
.NET's parser would normalise. Same 400 and same message either way
(`{"error":"url must use https://"}`).

**R5 — a base64 fragment password that decodes to non-UTF-8 bytes.** C#
`DecodeFragmentPassword` (`BasisRestApiRoutes.cs:232-237`) decodes the base64 and runs
`Encoding.UTF8.GetString`, which substitutes U+FFFD rather than throwing — so the caller gets the
lossily-decoded text as the password. Rust (`basis_rest_api_routes.rs:293-302`) decodes the
base64, fails `String::from_utf8`, and falls back to the *raw base64 text* as the password. Two
different passwords for the same input. Only reachable when the fragment is valid base64 of
invalid UTF-8. `Convert.FromBase64String` also tolerates embedded whitespace where the Rust
`STANDARD` engine does not, so a padded-with-newlines fragment decodes on one side and is passed
through verbatim on the other.

**R6 — list ordering.** C# `ListWorlds`/`ListPlayers` return dictionary enumeration order
(`Core/BasisServerControl.cs:124-152`). Rust sorts — worlds by `netId`, players by `netId`
(`core/basis_server_control.rs:200, 217`). Response-visible, though both are JSON arrays with no
documented order.

**R7 — float rendering inside `position`.** C# `JsonSerializer.Serialize(float[])`
(`BasisRestApiRoutes.cs:100`) renders `2f` as `2`; Rust `serde_json` on `f32`
(`basis_rest_api_routes.rs:139-141`) renders it as `2.0`. Identical values, different spelling;
any JSON parser reads the same number. `null` for an unknown position is identical on both, and
pinned (`RestApiTests.cs:252-281`, `rest_api_tests.rs:272-296`).

**R8 — bind host grammar**, shared with the health check: C# uses an `HttpListener` prefix
(`BasisRestApiHandler.cs:37`) and so accepts DNS hostnames and `+`/`*`; Rust reuses
`BasisNetworkHealthCheck::bind_address` (`basis_rest_api_handler.rs:46`), which accepts only the
wildcards, `localhost` and IP literals, and returns a permanent error otherwise. Pinned:
`rest_api_tests.rs:410-417`. The startup log line also differs cosmetically — C# brackets an IPv6
host (`BasisRestApiHandler.cs:40`), Rust logs `config.api_host` raw (`:70`).

**R9 — what a handler failure logs.** C# catches the exception and logs it with its message and
stack (`BasisRestApiRoutes.cs:74-78`); Rust catches the unwind and logs the constant string
`"REST API handler error"` with no payload (`basis_rest_api_handler.rs:118-121`,
`basis_rest_api_routes.rs:339-342`). Same 500 and same `{"error":"internal server error"}` body,
much less to debug from. Also worth noting: `catch_unwind` does nothing under `panic = "abort"`,
where the C# `catch` would still have contained the failure.

**R10 — the per-IP cooldown's zero sentinel.** C# stores `_clock.ElapsedMilliseconds` and treats
`0` as "never seen" (`BasisServerInfoQuery.cs:144-146`); a probe arriving in the first
millisecond after start records `0`, and the next probe from that IP skips its cooldown. Rust
stores `now_ms.max(1)` (`basis_server_info_query.rs:132`). A one-millisecond window, closed.

**R11 — an unsendable name or MOTD drops the reply instead of truncating it.** C#
`writer.Put(name, maxLength)` truncates and always sends (`BasisServerInfoQuery.cs:117-119`).
Rust checks the result and returns `None` — no reply at all — if either string fails
(`basis_server_info_query.rs:109-114`). Practically unreachable: `put_string_max` truncates to
`max_length` and only errors above 64 KiB
(`basis_network_core/src/io/net_data_writer.rs:312-331`).

**R12 — error surface on the probe path.** C# wraps the whole handler in a catch and logs
`"ServerInfoQuery failed: {message}"` for anything (`BasisServerInfoQuery.cs:126-129`). Rust has
no blanket catch; it warns only when the transport refuses the unconnected send
(`basis_server_info_query.rs:64-68`), a failure the C# never noticed because it ignored the send
result.

### Checked and found equivalent

**Auth is identical.** SHA-256 of the configured key at construction, SHA-256 of everything after
a case-insensitive `"Bearer "` prefix, constant-time comparison, empty key rejects every request
with a startup warning, and a failure answers 401 with
`WWW-Authenticate: Bearer realm="basis-server"` and an empty body
(`BasisRestApiHandler.cs:29-34, 82-88, 109-122` vs `basis_rest_api_handler.rs:42-45, 74-103,
110-111, 135-137`). The Rust byte-slices `auth[..7]` at `:98`, which would panic on a non-boundary
index — not reachable, because `HeaderValue::to_str` at `:109` returns `Some` only for
visible-ASCII values, so index 7 is always a char boundary.

**Rate limits are identical.** Per-IP cooldown 500 ms (`BasisServerInfoQuery.cs:37` /
`basis_server_info_query.rs:36`), tracking map capped at 4096 entries and wiped wholesale past it
(`:40, 138-141` / `:38, 124-126`), global bucket refilling at 100 tokens/s with a 200-token burst
capacity (`:46-47, 150-168` / `:40-41, 136-150`), and the same layering: minimum request size
first, then the extra `< 8` guard, then magic, then per-IP, then the bucket
(`:71-101` / `:77-95`). The response frame — magic, protocol version, echoed nonce, online, max,
length-capped name and MOTD — is byte-identical in order and type
(`:112-118` / `:104-110`).

**Response shapes are identical** for every route: `{"ok":true}`,
`{"ok":true,"netId":"…"}`, `{"ok":true,"unloaded":N}`,
`{"players":[{"netId":N,"uuid":"…","displayName":"…","platform":"…","position":[x,y,z]|null}]}`,
`{"worlds":[{"netId":"…","url":"…","persistent":bool,"adminLocked":bool,"strategy":N}]}`,
`{"error":"…"}` for 400/404/500, empty bodies for 401/404-outside-api/405/503. Every error string
matches literally, including `"delay must be an integer 0–300 (seconds)"` with its en dash
(`BasisRestApiRoutes.cs:177` / `basis_rest_api_routes.rs:229`) and
`"password required (provide password field or embed in url as #fragment)"` (`:214` / `:277`).

Also verified equivalent: the 32-request concurrency cap and its 503
(`BasisRestApiHandler.cs:15, 57-61` / `basis_rest_api_handler.rs:39, 106-108`); the
`Cache-Control: no-store, max-age=0` and `X-Content-Type-Options: nosniff` hardening on every
response; `Content-Type: application/json; charset=utf-8` present exactly when there is a body;
path trimming and segment splitting (`:90-91` / `:113-114`); `strategy` accepted as either the
three names or a defined byte, with an undefined value 400 and a non-string non-byte silently
leaving `Immediate` (`BasisRestApiRoutes.cs:116-139` / `basis_rest_api_routes.rs:171-187`);
`delay` accepting null as 0 and rejecting non-integers and out-of-range values
(`:171-178` / `:224-231`); an empty or whitespace body parsed as `{}`
(`:264` / `:321-323`); explicit `password` overriding an embedded fragment
(`:205-210` / `:263-269`); `#` and `%23` fragment splitting (`:220-230` / `:282-291`); and the
`ApiEnabled && ApiKey != ""` start gate living outside the module on both sides
(`BasisServerConsole/Program.cs:74` / `basis_server_console/src/main.rs:129`).

## Corners cut

* `internal_error()` throws away the panic payload (`basis_rest_api_routes.rs:339-342`), so a 500
  in production leaves one constant log line and nothing else. The C# had the exception text.
* `read_body` uses `String::from_utf8_lossy` (`basis_rest_api_routes.rs:320`), which turns invalid
  UTF-8 into U+FFFD before `serde_json` sees it; the substitution can make a malformed body parse
  as a valid string field rather than fail. The C# `Encoding.UTF8.GetString`
  (`BasisRestApiRoutes.cs:263`) is lossy in the same way, so this is a faithful port of a rough
  edge, not a new one.
* The 1 MiB `MAX_BODY_BYTES` check is now redundant with axum's own limit for anything above
  2 MiB and only actually fires in the 1–2 MiB band (R2). Setting an explicit
  `DefaultBodyLimit::max(MAX_BODY_BYTES)` on the router would make the C# contract exact and
  would also fix R1's buffering window.
* Nothing in the module rate-limits or logs failed authentications on either side; a scanner gets
  32 concurrent 401s as fast as it can ask.

## Improvements

* The routes are a pure function of `(method, segments, body) -> ApiResponse`
  (`basis_rest_api_routes.rs:51`, `12-32`), so every route can be exercised without a socket or a
  listener. The C# writes directly into `HttpListenerResponse` from inside each route
  (`BasisRestApiRoutes.cs:270-291`), which is why its tests all have to stand up a real server.
* Bind failures are typed and reported rather than thrown out of a constructor: a busy port is
  transient, an unparseable host permanent (`basis_rest_api_handler.rs:46-49`). Pinned by
  `rest_api_tests.rs:400-417` — two tests the C# suite has no equivalent of.
* `is_https_url` requires a non-empty host (`basis_rest_api_routes.rs:311`); the C# check looks
  only at the scheme (`BasisRestApiRoutes.cs:241`), so a URL with an empty authority passes there
  and is handed to the loader.
* The per-IP cooldown's first-millisecond hole is closed (R10) and the global bucket uses an
  `Option` for "not yet refilled" (`basis_server_info_query.rs:25, 139`) instead of the C#'s
  `_lastRefillTicks == 0` sentinel (`BasisServerInfoQuery.cs:155`), which has the same class of
  hole at start-up.
* A refused unconnected send is now visible (`basis_server_info_query.rs:64-68`); the C# ignored
  the return value of `SendUnconnectedMessage` entirely.
* `MIN_INTERVAL_MS` is public and a test actually waits on it rather than hardcoding 500
  (`basis_server_tests/tests/networking/mixed_world_hello_tests.rs:211`).
* Subscription is idempotent: `subscribe` unsubscribes first and stores the id
  (`basis_server_info_query.rs:43-58`), so a double subscribe cannot register the handler twice
  the way the C# `+=` would (`BasisServerInfoQuery.cs:56`).
* One host parser serves both HTTP listeners (`basis_rest_api_handler.rs:46` reuses the health
  check's), where the C# had two copies of `FormatHost`
  (`BasisRestApiHandler.cs:125-128`, `BasisNetworkHealthCheck.cs:362-365`).

## Verdict

The REST surface is **contract-compatible**. Routes, methods, path parsing, status codes, the
`Allow` headers, the auth scheme and its 401 challenge, every response body shape, every error
string, and both rate-limit layers of the UDP probe are the same. The Rust test file is a
one-for-one port of `RestApiTests.cs` — same 32 cases, same names — plus three tests the C# does
not have. Anything written against the C# management API works unmodified against the Rust one.

The deviations are all edge conditions: unit of measure for the message length cap (R3), two
different-but-close URL parsers (R4), an unusual base64 fragment (R5), array ordering (R6) and
float spelling (R7). None changes a route, a status code or a field name.

Two are worth acting on rather than only recording. R1 — the body is buffered before the API key
is checked, which the C# deliberately did the other way round — is the only place the port is
weaker than the original at the security boundary, and an explicit `DefaultBodyLimit` on the
router fixes both it and R2's inconsistent 413 body. R9 — a 500 logs a bare constant — costs
nothing to fix and will matter the first time one fires.
