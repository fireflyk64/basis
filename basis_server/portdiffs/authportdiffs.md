# auth — port diffs

C#: `Basis Server/BasisNetworkServer/Auth/` · Rust: `basis_server/basis_network_server/src/auth/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `Interface.cs` | `interface.rs` | 26 → 43 | ported; `IAuth` and `IAuthIdentity` become traits, the `HasFileSupport` static becomes `IAuthIdentitySupport` |
| `Password.cs` | `password.rs` | 75 → 68 | ported; the three newtypes and `CheckPassword` map 1:1, plus a local `fixed_time_equals` |
| — | `mod.rs` | 0 → 6 | Rust-only module wiring |

**On the split.** It is the same on both sides, so nothing needs reconciling against `security`.
The `Auth` folder in the C# holds only the two contract files; the sole `IAuthIdentity`
implementation, `BasisDIDAuthIdentity`, lives in
`Basis Server/BasisNetworkServer/Security/BasisDIDAuthIdentity.cs` (461 lines), and its port lives
in `basis_server/basis_network_server/src/security/basis_did_auth_identity.rs`, implementing the
trait at `:413-457`. The sole `IAuth` implementation is `PasswordAuth` in this module. Nothing
moved between `auth` and `security`; `basis_did_auth_identity.rs` is covered by the security
module's own diff, and only its trait-facing surface is compared below.

## Deviations

1. **`process_connection` takes the connect reader explicitly.** `Interface.cs:17` declares
   `ProcessConnection(Configuration, ConnectionRequest, NetPeer)`; `interface.rs:19` adds a
   `data: NetDataReader` parameter. In the C#, `ConnectionRequest.Data` is one reader that
   `HandleConnectionRequest` has already advanced past the version `ushort` and the auth
   `BytesMessage` (`Core/BasisServerHandleEvents.cs:535, 549, 564`), and
   `BasisDIDAuthIdentity.ProcessConnection` keeps reading from that same position
   (`Security/BasisDIDAuthIdentity.cs:110, 116`). The Rust makes that hand-off explicit: the
   advanced reader is threaded from `core/basis_server_handle_events.rs:555` through `:586` into
   the identity. Same bytes, same position, different signature. Documented in the doc comment at
   `interface.rs:17-18`. Not pinned by a test.

2. **The `RemoveConnection` overload pair is renamed, not merged.** C# has
   `RemoveConnection(int)` and `bool RemoveConnection(int, NetPeer)` (`Interface.cs:19-20`), where
   the one-arg form delegates to the two-arg form with `null`
   (`Security/BasisDIDAuthIdentity.cs:304-307`). Rust splits them into `remove_connection` and
   `remove_connection_expected` (`interface.rs:21-23`, implemented at
   `security/basis_did_auth_identity.rs:428, 438`). The disconnect path uses the value-matched form
   in both (`Core/BasisServerHandleEvents.cs:364` ↔ `core/basis_server_handle_events.rs:405-407`).

3. **`out` parameters become `Option`.** `NetIDToUUID(NetPeer, out string)` and
   `UUIDToNetID(string, out int)` (`Interface.cs:21-22`) become
   `net_id_to_uuid(&NetPeerRef) -> Option<String>` and `uuid_to_net_id(&str) -> Option<i32>`
   (`interface.rs:25-27`). One caller consequence worth noting: the C# `CleanupPeerSubsystems`
   had to check both the bool *and* `string.IsNullOrEmpty(uuid)` because a `false` return still
   assigns the `out` (`Core/BasisServerHandleEvents.cs:356`); the Rust collapses that to
   `unwrap_or_default()` plus one `is_empty()` (`core/basis_server_handle_events.rs:398-399`).
   Same behavior, one fewer way to get it wrong.

4. **`HasFileSupport` is now atomic.** `Interface.cs:24` declares it as a mutable `public static
   bool` field *on the interface* — written from `NetworkServer.InitializeAuth`
   (`Core/NetworkServer.cs:232`) and read from `BasisDIDAuthIdentity`
   (`Security/BasisDIDAuthIdentity.cs:367, 389`) with no synchronization, i.e. a plain unsynchronized
   cross-thread field. Rust holds it in a `static AtomicBool` with Acquire/Release accessors behind
   `IAuthIdentitySupport` (`interface.rs:30-42`), written at `core/network_server.rs:413` and read
   at `security/basis_did_auth_identity.rs:325, 392`. Improvement; the visible value is the same.

5. **Password decoding.** `Encoding.UTF8.GetString(Bytesmsg)` (`Password.cs:29`) vs
   `String::from_utf8_lossy(bytes_msg)` (`password.rs:19`). .NET's default `UTF8Encoding` uses a
   replacement fallback rather than throwing, so both map invalid byte sequences to U+FFFD and a
   password containing invalid UTF-8 compares identically on both sides. No behavioral deviation —
   recorded because `GetString` is easy to misread as strict.

6. **`FixedTimeEquals` is reimplemented rather than delegated.**
   `CryptographicOperations.FixedTimeEquals` (`Password.cs:58`) becomes a hand-written
   `fixed_time_equals` (`password.rs:59-68`). The contract matches: an early `false` on a length
   mismatch (so both leak the password *length*), then an OR-accumulate over equal-length inputs
   compared once at the end — the same shape .NET uses. It is not routed through a crate whose job
   is to defeat the optimizer (`subtle`, `constant_time_eq`), so nothing in the type system
   prevents LLVM from short-circuiting the loop; in practice the accumulator pattern survives.
   Note the tree already contains a second, independent copy at
   `rest_api/basis_rest_api_handler.rs:79-88` for API tokens.

7. **Everything else in `CheckPassword` is line-for-line.** The empty-server-password branch logs
   at *error* level and returns `true` — "the server is open to all users" — in both
   (`Password.cs:46-50` ↔ `password.rs:34-37`); the empty-user-password branch logs at *log* level
   and returns `false` in both (`Password.cs:51-55` ↔ `password.rs:38-41`); the mismatch branch
   logs at error level (`Password.cs:64` ↔ `password.rs:45`). The log levels and strings are
   identical, including the em dash.

8. **Construction.** `new PasswordAuth(Configuration.Password ?? string.Empty)`
   (`Core/NetworkServer.cs:209, 234`) vs `PasswordAuth::new(&configuration.password)` over a
   non-nullable `String` (`core/network_server.rs:388, 415`). The `?? string.Empty` is
   unrepresentable and unneeded.

9. **Call-site fail-closed behavior differs.** `NetworkServer.Auth` is a bare field in the C#
   (`Core/NetworkServer.cs:77`), so a null comparer throws at
   `Core/BasisServerHandleEvents.cs:554` and lands in that method's catch-all. The Rust
   `NetworkServer::auth()` returns an `Option` and
   `core/basis_server_handle_events.rs:570` treats `None` as "not authenticated", producing the
   ordinary "Authentication failed, Auth rejected" rejection. Both refuse the connection; the Rust
   does so through the normal path. (Also recorded in `coreportdiffs.md`, deviation 3.)

## Corners cut

- **The module has no tests.** `PasswordAuth` is referenced only from
  `core/network_server.rs:388` and `:415`; a grep across `basis_server_tests/`,
  `basis_network_server/tests/` and `basis_rest_api_tests/` finds nothing constructing it, calling
  `is_authenticated`, or exercising `fixed_time_equals`. That leaves all four
  behaviors unpinned: the empty-server-password "open to all" branch, the empty-user-password
  rejection, the correct-password accept, and the length-mismatch short-circuit. The connection
  lifecycle suite does cover the *call site* — `wrong_password_is_rejected_when_auth_enabled` and
  `malformed_auth_payload_is_rejected_when_auth_enabled`
  (`basis_server_tests/tests/networking/basis_connection_lifecycle_tests.rs:148-177`) — but it
  installs a `FakeAuth` stub (`:51-52`, defined at `basis_server_tests/src/support/lifecycle.rs:126`),
  so `PasswordAuth` itself is never executed by any test.
- **`IAuthIdentitySupport::set_has_file_support` is unguarded.** Any crate that can see the type
  can flip a flag that decides whether the DID identity persists to disc
  (`security/basis_did_auth_identity.rs:325, 392`). The C# static field
  (`Interface.cs:24`) was equally public, so this is inherited rather than introduced — but the
  port had an opportunity to narrow it and did not.
- **The two newtypes carry no invariants.** `ServerPassword` and `UserPassword`
  (`password.rs:8, 11`) are as thin as the C# structs (`Password.cs:11-22`): no `Zeroize` on drop,
  no `Debug` suppression, so a password sits in a plain `String` for the life of the
  `PasswordAuth`. Same exposure as the original.
- **No rate limiting or attempt accounting.** Neither tree tracks failed password attempts; the
  only backstop is the transport's connect timeout. Faithfully ported.

## Improvements

- The atomic `HasFileSupport` (deviation 4) removes an unsynchronized static-field read/write that
  the C# performed across the boot thread and the transport threads.
- The traits carry `Send + Sync` bounds (`interface.rs:11, 16`), making the "these are shared
  across transport threads" requirement explicit; the C# interfaces stated nothing.
- `Option`-returning lookups (deviation 3) remove the C# pattern where a `false` return still
  writes an `out` parameter, which the disconnect path had to defend against by hand.
- Missing-comparer handling at the call site is a normal rejection rather than an exception caught
  by a catch-all (deviation 9).

## Verdict

The smallest and most faithful of the three modules. The password comparison, its three branches,
its log levels and its log strings are line-for-line identical, and the timing-safe compare keeps
the same leak profile (length yes, content no) as `CryptographicOperations.FixedTimeEquals`. The
interface changes are all mechanical translation — traits, `Option` instead of `out`, an explicit
reader parameter where the C# relied on a shared mutable `ConnectionRequest.Data` cursor — and one
of them, the atomic `HasFileSupport`, fixes a real data race in the original.

The single thing worth acting on is coverage: this module gates entry to the whole server and has
zero tests, and the lifecycle suite that looks like it covers it substitutes a stub. Four small
unit tests over `PasswordAuth::is_authenticated` would pin every branch.
