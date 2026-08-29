# Identity — port diffs

C#: `Basis Server/BasisNetworkCore/Identity/` · Rust: `basis_server/basis_network_core/src/identity/`

## File map

| C# | Rust | lines (C#→Rust) | status |
| --- | --- | --- | --- |
| `BasisUserRestrictionMode.cs` | `basis_user_restriction_mode.rs` | 13 → 52 | extended |
| `IPlayerIdentityProvider.cs` | `i_player_identity_provider.rs` | 69 → 85 | deviates |
| — | `mod.rs` | — → 5 | extended (Rust module wiring, no C# analogue) |

`IPlayerIdentityProvider.cs` holds three things: the `PlayerIdentity` data class, the provider
interface, and the static `BasisPlayerIdentityRegistry`. All three are present in Rust with the
same member set. Note that the registry is dormant in the server build on **both** sides: the only
implementation registers itself from `AutoRegister`, which is `#if UNITY_2017_1_OR_NEWER`-gated in
C# (`Basis Server/BasisNetworkClient/BasisDIDAuthIdentityProvider.cs:24-31`), and its Rust
counterpart `auto_register` (`basis_server/basis_network_client/src/basis_did_auth_identity_provider.rs:14-17`)
has no caller. The only C# consumer is the Unity client
(`Basis/Packages/com.basis.framework/Networking/BasisNetworkConnection.cs:52`).

## Deviations

1. **`PlayerIdentity.Properties` is case-insensitive in C# and case-sensitive in Rust — and the
   Rust comment says otherwise.** `IPlayerIdentityProvider.cs:10` constructs the dictionary with
   `StringComparer.OrdinalIgnoreCase`, so `properties["Uuid"]` and `properties["uuid"]` are the
   same entry. `i_player_identity_provider.rs:11` is a plain `HashMap<String, String>`, which is
   case-sensitive, under a doc comment at `:10` claiming "Case-insensitive keys, like the C#
   `StringComparer.OrdinalIgnoreCase` dictionary". The comment is wrong. Why: probably an
   intent-recorded-but-not-implemented slip — there is no newtype or wrapper doing the folding.
   Latent today, because the single constructor in the tree
   (`basis_did_auth_identity_provider.rs:26`) leaves `properties` empty and nothing reads it; it
   becomes a live bug the moment a provider writes a property under one casing and a consumer
   reads it under another. Not pinned by a test — there is no Rust test for `PlayerIdentity` or
   the registry at all.

2. **Registry key folding is Unicode-lowercase, not ordinal-ignore-case.**
   `IPlayerIdentityProvider.cs:23-24` uses an `OrdinalIgnoreCase` dictionary, which folds per
   UTF-16 code unit with the invariant simple mapping. The Rust lowercases the key instead
   (`i_player_identity_provider.rs:43` on insert, `:64`, `:74`, `:83` on lookup), and
   `str::to_lowercase` is *full* Unicode lowercasing, which is locale-independent but can change
   length (`'İ'` U+0130 becomes two code points). For any ASCII provider id the two agree exactly,
   and the only id in either tree is `"did"`. Not pinned by a test.

3. **`Register` panics where the C# threw.** `IPlayerIdentityProvider.cs:30-31` throws
   `ArgumentNullException` for a null provider and `ArgumentException` for an empty `ProviderId`.
   `i_player_identity_provider.rs:41` is `assert!(!provider.provider_id().is_empty(), ...)` — a
   panic, in a library crate whose own header denies `clippy::panic`
   (`basis_server/basis_network_core/src/lib.rs:6-16`). `assert!` is not covered by that lint, so
   it slipped through the net. This is the one finding here that contradicts the project's stated
   port standard ("panics are unacceptable; use `Result`/`Option`"); the C# behaviour translates
   to returning a `Result` (or ignoring the empty id), not to aborting the process. The null case
   is structurally impossible in Rust — `Arc<dyn IPlayerIdentityProvider>` cannot be null — so
   only the empty-id path is reachable. Not pinned by a test, and no caller passes an empty id
   today.

4. **`BasisUserRestrictionMode::parse` accepts numeric text; the C# config path rejects it.**
   There is no `Parse` in the C# enum file to deviate from — the C# reads this field through
   `XmlSerializer` (`Basis Server/BasisNetworkCore/Configuration/BasisServerConfiguration.cs:443-448`,
   field declared at `:73`), which maps the XML text to a *member name* and throws
   `InvalidOperationException` for anything else, numeric text included; `LoadFromXml` does not
   catch, so a bad value aborts the load. The Rust config path calls
   `BasisUserRestrictionMode::parse` (`basis_server/basis_network_core/src/configuration/mod.rs:130-136`),
   and `basis_user_restriction_mode.rs:43` falls back to `other.parse::<u8>().ok().map(Self::from_byte)`.
   So `<BasisUserRestrictionMode>2</BasisUserRestrictionMode>` silently becomes `AllowList` in
   Rust and is a hard error in C#, and — worse — `200` becomes `Normal`, the *least restrictive*
   mode, because `from_byte` (`basis_user_restriction_mode.rs:14-21`) folds every unrecognised
   byte to `Normal`. Casing behaves identically on both sides (both reject `"normal"`), and an
   unparseable string is a hard error on both (`basis_config_xml_docs.rs:143` propagates with
   `?`). Why the fallback exists: presumably to tolerate a hand-edited numeric config. Not pinned
   by a test.

Checked and found matching:

* `ResolveActive`'s fallback to the default provider when the active id is not registered
  (`IPlayerIdentityProvider.cs:46-47` vs `i_player_identity_provider.rs:64-65`), and `Resolve`'s
  deliberate *lack* of that fallback (`:57-58` vs `:73-74`).
* Empty/null `ActiveProviderId` resetting to `"did"` (`:38` vs `:51-58`), and `IsRegistered`
  returning false for an empty id (`:65` vs `:80-82`).
* Both sides resolve the provider under the lock and then call `GetOrCreate`/`get_or_create`
  *outside* it (`:49` vs `:68`, `:76`), so user code never runs while the registry is locked.
* `null` return vs `Option::None` for an unresolvable provider (`:49` vs `:61`) — equivalent.
* Enum discriminants 0..3 in declaration order, `byte`/`u8`-backed, default `Normal`
  (`BasisUserRestrictionMode.cs:6-12` vs `basis_user_restriction_mode.rs:3-11`). The wire byte the
  server writes (`Basis Server/BasisNetworkServer/Security/BasisGlobalLockManager.cs:247,288`)
  therefore matches, and the C# and Rust connection-lifecycle tests exercise the same four modes.
* The environment-override and tuning-profile paths refuse to set this field on both sides
  (`BasisServerConfiguration.cs:617` "type could not be processed" vs
  `basis_server_configuration.rs:276-278`; `BasisTuningProfile.cs:311` vs the
  `NotFromProfile` error at `basis_tuning_profile.rs:21`), so the numeric fallback in deviation 4
  is reachable only through the XML file.

## Corners cut

* **No newtype wrapping and no validation.** `PlayerIdentity.uuid` and `.provider` are bare
  `String`s (`i_player_identity_provider.rs:8-9`), exactly as in C# (`IPlayerIdentityProvider.cs:8-9`).
  The port had the opportunity to make these newtypes with a validating constructor — a DID has a
  shape — and did not; nothing checks that `uuid` is non-empty, well-formed, or that `provider`
  matches the provider that produced it. That is faithful to the C#, so it is a preserved gap
  rather than a regression, but it is the thing to fix before trusting these types with anything
  new. The only validation anywhere in the module is the empty-`ProviderId` check, which panics
  (deviation 3).
* **No tests.** Neither `BasisPlayerIdentityRegistry` nor `PlayerIdentity` has a test on either
  side. `BasisUserRestrictionMode` is exercised only indirectly, as a `Configuration` field, by
  `basis_server_tests/tests/networking/basis_connection_lifecycle_tests.rs` and
  `.../security/security_list_and_lock_manager_tests.rs`. The registry's own semantics — the
  default-provider fallback, the empty-id reset, replacement on re-register — are unpinned in both
  languages, which is why deviations 1-3 are invisible to CI.
* **Registration silently replaces.** `IPlayerIdentityProvider.cs:32` (`_providers[id] = provider`)
  and `i_player_identity_provider.rs:43` (`insert`) both overwrite an existing provider without a
  word. Faithful, and worth knowing.

## Improvements

* **The enum cannot hold an invalid discriminant.** `(BasisUserRestrictionMode)200` is a legal C#
  value that flows through the system and matches no `case`; the Rust type has exactly four
  inhabitants (`basis_user_restriction_mode.rs:5-11`), so every `match` on it is genuinely
  exhaustive. The cost is the permissive fold in `from_byte` — see deviation 4.
* **Useful surface the C# lacks**: `as_byte`, `as_str`, `parse`, `Display` and serde derives
  (`basis_user_restriction_mode.rs:23-52`, `:3`), which is what lets the Rust config layer render
  and parse the field by name instead of reflecting over it.
* **Lazy static state with no static-constructor ordering hazard.** `STATE` is
  `Mutex<Option<RegistryState>>` initialised on first touch (`i_player_identity_provider.rs:24-33`),
  so there is no equivalent of the C# static-field initialisation order the config loader has to
  force with `RuntimeHelpers.RunClassConstructor` elsewhere
  (`BasisServerConfiguration.cs:440`).
* **`Send + Sync` is required of every provider** (`i_player_identity_provider.rs:14`), so a
  provider that is not thread-safe cannot be registered into a registry that is shared across
  threads. The C# interface makes no such demand.

## Verdict

The registry is a close port — the fallback rules, the empty-id handling and the resolve-outside-
the-lock discipline all match — and the enum is strictly better typed than the C# original. The
two things to fix are small and concrete: the `properties` map is documented as case-insensitive
and is not, and `register` panics on an empty provider id in a crate whose own standard forbids
panics. The third, `parse`'s numeric fallback turning an out-of-range config value into `Normal`,
matters because `Normal` is the unrestricted mode; the C# rejects the document instead. None of
it is pinned by a test on either side, and the whole module is dormant in the server build.
