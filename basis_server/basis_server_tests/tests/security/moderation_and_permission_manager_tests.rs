//! `PermissionManager` (every test builds its own instance) and `BasisPlayerModeration` (process
//! statics, GUID-suffixed uuids and unique IPs so results never depend on ordering or on ban-file
//! leftovers from prior runs).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use basis_network_core::BasisNetworkCommons;
use basis_network_core::SerializableBasis::{AdminRequest, AdminRequestMode, ClientMetaDataMessage, PermissionBitsetMap};
use basis_network_core::transport::basis_network_shell::NetPeer;
use basis_network_core::{NetDataReader, NetDataWriter};
use basis_network_server::NetworkServer;
use basis_network_server::security::permission_manager::PermissionXml;
use basis_network_server::security::{BasisPlayerModeration, PermNodes, PermissionIntegration, PermissionManager};
use basis_server_tests::support::{FakePeer, MapAuthIdentity, ServerStaticsScope};
use parking_lot::Mutex;
use serial_test::serial;

fn io_root() -> PathBuf {
    let root = std::env::temp_dir().join("perm-test-io");
    std::fs::create_dir_all(&root).expect("io root");
    root
}

fn unique_xml_path() -> PathBuf {
    io_root().join(format!("perms-{}.xml", uuid::Uuid::new_v4().simple()))
}

/// Fresh manager whose debounced saves never fire and whose xml path is unique.
fn create_manager() -> Arc<PermissionManager> {
    let manager = PermissionManager::new();
    manager.set_save_debounce_ms(u64::MAX);
    manager.set_xml_path(unique_xml_path()).expect("xml path");
    manager
}

fn new_uuid() -> String {
    format!("user-{}", uuid::Uuid::new_v4().simple())
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| s.to_string()).collect()
}

fn set_equals(a: &[String], b: &[&str]) -> bool {
    let a: HashSet<String> = a.iter().map(|s| s.to_lowercase()).collect();
    let b: HashSet<String> = b.iter().map(|s| s.to_lowercase()).collect();
    a == b
}

// ---- stability contract: permission node wire/persistence names ----

#[test]
fn perm_nodes_string_values_are_pinned() {
    assert_eq!(PermNodes::ALL, "*");
    assert_eq!(PermNodes::HELP, "basis.command.help");
    assert_eq!(PermNodes::SERVER_STATS, "basis.server.stats");
    assert_eq!(PermNodes::RESOURCE_LOAD_WORLD, "basis.resource.load.world");
    assert_eq!(PermNodes::RESOURCE_UNLOAD_WORLD, "basis.resource.unload.world");
    assert_eq!(PermNodes::RESOURCE_LOAD_PROP, "basis.resource.load.prop");
    assert_eq!(PermNodes::RESOURCE_UNLOAD_PROP, "basis.resource.unload.prop");
    assert_eq!(PermNodes::RESOURCE_LOAD_AVATAR, "basis.resource.load.avatar");
    assert_eq!(PermNodes::RESOURCE_UNLOAD_AVATAR, "basis.resource.unload.avatar");
    assert_eq!(PermNodes::RESOURCE_LOCK_BYPASS_AVATAR, "basis.resource.lockbypass.avatar");
    assert_eq!(PermNodes::RESOURCE_LOCK_BYPASS_PROP, "basis.resource.lockbypass.prop");
    assert_eq!(PermNodes::RESOURCE_LOCK_BYPASS_WORLD, "basis.resource.lockbypass.world");
    assert_eq!(PermNodes::RESOURCE_LOCK_BYPASS_SERVER, "basis.resource.lockbypass.server");
    assert_eq!(PermNodes::OWNERSHIP_TRANSFER, "basis.ownership.transfer");
    assert_eq!(PermNodes::OWNERSHIP_REMOVE, "basis.ownership.remove");
    assert_eq!(PermNodes::OWNERSHIP_GET, "basis.ownership.get");
    assert_eq!(PermNodes::CONTENT_SHARE_DELETE, "basis.contentshare.delete");
    assert_eq!(PermNodes::CONTENT_SHARE_CREATE, "basis.contentshare.create");
    assert_eq!(PermNodes::PROTECTION, "basis.protection");
    assert_eq!(PermNodes::CONFIGURATION_EDITOR, "basis.configuration");
    assert_eq!(PermNodes::PLAYER_MODERATION, "basis.moderation");
    assert_eq!(PermNodes::MODERATION_BAN, "basis.moderation.ban");
    assert_eq!(PermNodes::MODERATION_KICK, "basis.moderation.kick");
    assert_eq!(PermNodes::MODERATION_IP_BAN, "basis.moderation.ipban");
    assert_eq!(PermNodes::MODERATION_UNBAN, "basis.moderation.unban");
    assert_eq!(PermNodes::MODERATION_UNBAN_IP, "basis.moderation.unbanip");
    assert_eq!(PermNodes::MODERATION_MESSAGE, "basis.moderation.message");
    assert_eq!(PermNodes::MODERATION_MESSAGE_ALL, "basis.moderation.messageall");
    assert_eq!(PermNodes::MODERATION_TELEPORT, "basis.moderation.teleport");
    assert_eq!(PermNodes::MODERATION_SHOUT, "basis.moderation.shout");
    assert_eq!(PermNodes::MODERATION_GLOBAL_LOCK, "basis.moderation.globallock");
    assert_eq!(PermNodes::MODERATION_HEADLESS_AUDIO, "basis.moderation.headlessaudio");
    assert_eq!(PermNodes::MODERATION_OPUS_BITRATE, "basis.moderation.opusbitrate");
    assert_eq!(PermNodes::MODERATION_FULL_QUALITY_BROADCAST, "basis.moderation.fullqualitybroadcast");
    // Gotcha worth pinning: the allowlist node's VALUE says "whitelist".
    assert_eq!(PermNodes::MODERATION_ALLOWLIST, "basis.moderation.whitelist");
    assert_eq!(PermNodes::ADMIN_LOGS, "basis.admin.logs");
    assert_eq!(PermNodes::PERMISSIONS_VIEW, "basis.permissions.view");
    assert_eq!(PermNodes::PERMISSIONS_EDIT, "basis.permissions.edit");
}

// ---- stability contract: permission-name -> bit index wire mapping ----

const EXPECTED_BITSET_ORDER: [&str; 28] = [
    "*",
    "basis.server.stats",
    "basis.resource.load.world",
    "basis.resource.unload.world",
    "basis.resource.load.prop",
    "basis.resource.unload.prop",
    "basis.resource.load.avatar",
    "basis.resource.unload.avatar",
    "basis.ownership.transfer",
    "basis.ownership.remove",
    "basis.ownership.get",
    "basis.contentshare.delete",
    "basis.contentshare.create",
    "basis.protection",
    "basis.configuration",
    "basis.moderation",
    "basis.moderation.ban",
    "basis.moderation.kick",
    "basis.moderation.ipban",
    "basis.moderation.unban",
    "basis.moderation.unbanip",
    "basis.moderation.message",
    "basis.moderation.messageall",
    "basis.moderation.teleport",
    "basis.moderation.shout",
    "basis.permissions.view",
    "basis.permissions.edit",
    "basis.moderation.headlessaudio",
];

#[test]
fn permission_bitset_map_node_to_bit_index_is_pinned() {
    // The bitset rides ServerMetaDataMessage; the map is append-only wire format.
    assert_eq!(PermissionBitsetMap::known_count(), EXPECTED_BITSET_ORDER.len());
    assert_eq!(PermissionBitsetMap::byte_count(), EXPECTED_BITSET_ORDER.len().div_ceil(8));

    // Index 0 is "*" and is special-cased (sets every bit), so start at 1.
    for (i, node) in EXPECTED_BITSET_ORDER.iter().enumerate().skip(1) {
        let (bitset, extras) = PermissionBitsetMap::encode(&strings(&[node]), None);
        assert!(extras.is_empty());
        for bit in 0..PermissionBitsetMap::known_count() {
            let set = (bitset[bit >> 3] & (1 << (bit & 7))) != 0;
            assert_eq!(set, bit == i, "node {node} bit {bit}");
        }
    }
}

#[test]
fn permission_bitset_map_encode_decode_round_trips_wildcard_extras_and_denies() {
    // Wildcard expands to every known node.
    let (bitset, extras) = PermissionBitsetMap::encode(&strings(&["*"]), None);
    assert!(extras.is_empty());
    let all = PermissionBitsetMap::decode(&bitset, &extras);
    assert_eq!(all.len(), PermissionBitsetMap::known_count());
    assert!(all.contains(PermNodes::MODERATION_BAN));

    // Unknown nodes travel through the extras side channel.
    let custom = format!("custom.node.{}", uuid::Uuid::new_v4().simple());
    let (bitset, extras) = PermissionBitsetMap::encode(&strings(&[PermNodes::HELP, &custom]), None);
    assert!(extras.contains(&custom));
    let decoded = PermissionBitsetMap::decode(&bitset, &extras);
    assert!(decoded.contains(&custom));

    // Denied nodes clear their bit even when wildcard set them.
    let (bitset, extras) = PermissionBitsetMap::encode(&strings(&["*"]), Some(&strings(&[PermNodes::MODERATION_KICK])));
    let decoded = PermissionBitsetMap::decode(&bitset, &extras);
    assert!(!decoded.contains(PermNodes::MODERATION_KICK));
    assert!(decoded.contains(PermNodes::MODERATION_BAN));
}

// ---- default role / unknown player ----

#[test]
fn unknown_user_on_fresh_manager_has_no_permissions() {
    let m = create_manager();
    let uuid = new_uuid();
    assert!(!m.has(&uuid, PermNodes::HELP));
    assert!(!m.has(&uuid, PermNodes::ALL));
    assert!(m.get_all_allowed_rules(&uuid).is_empty());
    assert!(m.get_all_denied_rules(&uuid).is_empty());
    assert!(m.try_get_user(&uuid).is_none());
}

#[test]
fn unknown_user_inherits_implicit_default_group_and_cache_invalidates() {
    let m = create_manager();
    let uuid = new_uuid();
    // Prime the effective-permission cache while nothing is granted.
    assert!(!m.has(&uuid, "test.zone.enter"));

    // A group change bumps the version and must invalidate that cached result, even for a user
    // that was never explicitly created.
    m.add_group_node("default", "test.zone.enter");
    assert!(m.has(&uuid, "test.zone.enter"));

    m.remove_group_node("default", "test.zone.enter");
    assert!(!m.has(&uuid, "test.zone.enter"));
}

#[test]
fn ensure_defaults_grants_baseline_to_unknown_users() {
    let m = create_manager();
    m.ensure_defaults();
    let uuid = new_uuid();

    assert!(m.has(&uuid, PermNodes::HELP));
    assert!(m.has(&uuid, PermNodes::RESOURCE_LOAD_AVATAR));
    assert!(m.has(&uuid, PermNodes::OWNERSHIP_TRANSFER));
    assert!(m.has(&uuid, PermNodes::CONTENT_SHARE_CREATE));
    assert!(!m.has(&uuid, PermNodes::MODERATION_KICK));
    assert!(!m.has(&uuid, PermNodes::PROTECTION));
    assert!(!m.has(&uuid, PermNodes::ALL));

    let def = m.try_get_group("default").expect("default group");
    assert!(set_equals(
        &def.nodes.to_vec(),
        &[
            PermNodes::HELP,
            PermNodes::RESOURCE_LOAD_PROP,
            PermNodes::RESOURCE_UNLOAD_PROP,
            PermNodes::RESOURCE_LOAD_AVATAR,
            PermNodes::RESOURCE_UNLOAD_AVATAR,
            PermNodes::RESOURCE_LOAD_WORLD,
            PermNodes::RESOURCE_UNLOAD_WORLD,
            PermNodes::OWNERSHIP_TRANSFER,
            PermNodes::OWNERSHIP_REMOVE,
            PermNodes::OWNERSHIP_GET,
            PermNodes::CONTENT_SHARE_DELETE,
            PermNodes::CONTENT_SHARE_CREATE,
        ]
    ));
}

#[test]
fn ensure_defaults_moderator_inherits_default_admin_gets_wildcard() {
    let m = create_manager();
    m.ensure_defaults();

    let moderator = new_uuid();
    m.add_user_to_group(&moderator, "moderator");
    assert!(m.has(&moderator, PermNodes::MODERATION_KICK));
    assert!(m.has(&moderator, PermNodes::MODERATION_BAN));
    assert!(m.has(&moderator, PermNodes::PERMISSIONS_VIEW));
    assert!(m.has(&moderator, PermNodes::RESOURCE_LOCK_BYPASS_AVATAR));
    assert!(m.has(&moderator, PermNodes::CHAT_LOCK_BYPASS));
    assert!(m.has(&moderator, PermNodes::VOICE_LOCK_BYPASS));
    assert!(m.has(&moderator, PermNodes::MODERATION_FORCE_AVATAR));
    assert!(m.has(&moderator, PermNodes::MODERATION_LOCOMOTION));
    assert!(m.has(&moderator, PermNodes::HELP)); // via "default" parent
    assert!(!m.has(&moderator, PermNodes::PERMISSIONS_EDIT));
    assert!(!m.has(&moderator, PermNodes::CONFIGURATION_EDITOR));
    assert!(!m.has(&moderator, PermNodes::ADMIN_LOGS));
    assert!(!m.has(&moderator, &format!("random.node.{}", uuid::Uuid::new_v4().simple())));

    let admin = new_uuid();
    m.add_user_to_group(&admin, "admin");
    assert!(m.has(&admin, PermNodes::PERMISSIONS_EDIT));
    assert!(m.has(&admin, PermNodes::PROTECTION));
    assert!(m.has(&admin, &format!("random.node.{}", uuid::Uuid::new_v4().simple()))); // "*" wildcard
    assert!(m.get_all_allowed_rules(&admin).iter().any(|r| r == "*"));

    let mod_group = m.try_get_group("moderator").expect("moderator");
    assert_eq!(mod_group.nodes.len(), 22);
    assert!(mod_group.parents.contains("default"));

    let admin_group = m.try_get_group("admin").expect("admin");
    assert!(admin_group.nodes.contains("*"));
    assert!(admin_group.parents.contains("moderator"));
}

#[test]
fn ensure_defaults_is_idempotent_and_never_overwrites_existing_groups() {
    let m = create_manager();
    m.ensure_defaults();
    m.ensure_defaults();
    assert_eq!(m.snapshot().groups.len(), 3);

    // A pre-existing "default" group is left exactly as the operator configured it.
    let custom = create_manager();
    custom.add_group_node("default", "custom.only.node");
    custom.ensure_defaults();

    let uuid = new_uuid();
    assert!(custom.has(&uuid, "custom.only.node"));
    assert!(!custom.has(&uuid, PermNodes::HELP));
    assert_eq!(custom.try_get_group("default").expect("default").nodes.len(), 1);
}

// ---- precedence as implemented (deny-wins decision table) ----

#[test]
fn user_deny_overrides_group_allow() {
    let m = create_manager();
    let uuid = new_uuid();
    m.add_group_node("default", "world.build");
    m.add_user_node(&uuid, "-world.build");

    assert!(!m.has(&uuid, "world.build"));
    assert!(m.get_all_denied_rules(&uuid).iter().any(|r| r == "world.build"));
    assert!(!m.get_all_allowed_rules(&uuid).iter().any(|r| r == "world.build"));
}

#[test]
fn group_deny_cannot_be_reallowed_by_user_grant() {
    // Applying raw nodes never overwrites an existing deny: once "default" denies a node, a
    // direct user-level allow of the same node does NOT re-enable it.
    let m = create_manager();
    let uuid = new_uuid();
    m.add_group_node("default", "-world.destroy");
    m.add_user_node(&uuid, "world.destroy");
    assert!(!m.has(&uuid, "world.destroy"));
}

#[test]
fn group_inheritance_parents_apply_first_deny_always_sticks() {
    let m = create_manager();
    let uuid = new_uuid();
    m.add_group_node("parent-grp", "node.one");
    m.add_group_node("parent-grp", "-node.two");
    m.add_group_node("child-grp", "-node.one"); // child deny overrides parent allow
    m.add_group_node("child-grp", "node.two"); // child allow cannot undo parent deny
    m.add_group_node("child-grp", "node.three");
    m.add_group_parent("child-grp", "parent-grp");
    m.add_user_to_group(&uuid, "child-grp");

    assert!(!m.has(&uuid, "node.one"));
    assert!(!m.has(&uuid, "node.two"));
    assert!(m.has(&uuid, "node.three"));
}

#[test]
fn wildcard_nodes_climb_by_dot_segments() {
    let m = create_manager();
    let uuid = new_uuid();
    m.add_user_node(&uuid, "a.b.*");

    assert!(m.has(&uuid, "a.b.c"));
    assert!(m.has(&uuid, "a.b.c.d")); // deeper nodes climb to a.b.*
    assert!(m.has(&uuid, "a.b.*")); // the wildcard key itself
    assert!(!m.has(&uuid, "a.b")); // "a.b.*" does NOT grant the stem
    assert!(!m.has(&uuid, "a.c"));
    assert!(!m.has(&uuid, "other"));
}

#[test]
fn more_specific_rule_wins_at_query_time_even_against_deny() {
    // Query resolution is exact -> nearest wildcard -> "*". A specific wildcard deny beats the
    // global allow, and an exact allow beats a wildcard deny.
    let m = create_manager();
    let uuid = new_uuid();
    m.add_user_node(&uuid, "*");
    m.add_user_node(&uuid, "-a.b.*");
    m.add_user_node(&uuid, "a.b.special");

    assert!(m.has(&uuid, "x.y")); // global allow
    assert!(!m.has(&uuid, "a.b.c")); // wildcard deny beats "*"
    assert!(m.has(&uuid, "a.b.special")); // exact allow beats wildcard deny
    assert!(!m.has(&uuid, "a.b.*")); // the denied wildcard key itself
}

#[test]
fn uuids_and_nodes_are_case_insensitive_and_trimmed() {
    let m = create_manager();
    let uuid = format!("User-{}", uuid::Uuid::new_v4().simple());
    m.add_user_node(&uuid.to_uppercase(), "  Spaced.Node  ");

    assert!(m.has(&uuid.to_lowercase(), "spaced.node"));
    assert!(m.has(&uuid, " SPACED.NODE "));
    let via_upper = m.try_get_user(&uuid.to_uppercase()).expect("upper");
    let via_lower = m.try_get_user(&uuid.to_lowercase()).expect("lower");
    assert!(via_upper.uuid.eq_ignore_ascii_case(&via_lower.uuid));
    assert!(via_upper.nodes.contains("spaced.node"));
}

#[test]
fn group_parent_cycles_resolve_without_hanging() {
    let m = create_manager();
    let uuid = new_uuid();
    m.add_group_parent("cyc-a", "cyc-b");
    m.add_group_parent("cyc-b", "cyc-a");
    m.add_group_parent("cyc-self", "cyc-self");
    m.add_group_node("cyc-a", "cycle.node");
    m.add_user_to_group(&uuid, "cyc-b");
    m.add_user_to_group(&uuid, "cyc-self");
    assert!(m.has(&uuid, "cycle.node"));
}

// ---- invalid input ----

#[test]
fn invalid_inputs_are_safe_no_ops_or_errors() {
    let m = create_manager();
    let uuid = new_uuid();

    // Whitespace uuid or node: mutators silently do nothing.
    m.add_user_node("", "some.node");
    m.add_user_node("   ", "some.node");
    m.add_user_node(&uuid, "");
    m.add_user_to_group(&uuid, "  ");
    m.add_group_node("", "some.node");
    m.remove_user_node(&uuid, "never.granted");
    m.remove_user_from_group(&new_uuid(), "nope");
    assert!(m.try_get_user(&uuid).is_none()); // nothing above created the user
    assert!(m.snapshot().users.is_empty());
    assert!(m.snapshot().groups.is_empty());

    assert!(!m.delete_group(""));
    assert!(!m.delete_group(&format!("missing-{}", uuid::Uuid::new_v4().simple())));

    // Unknown/blank permission names simply resolve to false.
    assert!(!m.has(&uuid, ""));
    assert!(!m.has(&uuid, "   "));
    assert!(!m.has("", "some.node"));

    // An empty xml path is refused.
    assert!(m.set_xml_path("").is_err());
}

// ---- structure APIs ----

#[test]
fn get_or_create_user_adds_default_membership_and_is_idempotent() {
    let m = create_manager();
    let uuid = new_uuid();

    let first = m.get_or_create_user(&uuid);
    let second = m.get_or_create_user(&uuid);
    assert_eq!(first.uuid, second.uuid);
    assert_eq!(first.uuid, uuid);
    assert!(first.groups.contains("default"));
    assert!(m.try_get_user(&uuid).is_some());

    let g1 = m.get_or_create_group("builders");
    let g2 = m.get_or_create_group("builders");
    assert_eq!(g1.name, g2.name);
    assert_eq!(g1.name, "builders");
}

#[test]
fn mutations_raise_on_permissions_changed_uuid_for_user_ops_none_for_group_ops() {
    let m = create_manager();
    let uuid = new_uuid();
    let events: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    m.set_on_permissions_changed(Some(Arc::new(move |u: Option<&str>| sink.lock().push(u.map(str::to_string)))));

    m.add_user_node(&uuid, "ev.node"); // -> uuid
    m.add_user_node(&uuid, "ev.node"); // duplicate -> no event
    m.add_user_to_group(&uuid, "ev-group"); // -> uuid
    m.add_group_node("ev-group", "g.node"); // -> None
    m.add_group_parent("ev-group", "ev-parent"); // -> None
    m.remove_user_node(&uuid, "ev.node"); // -> uuid
    m.remove_user_node(&uuid, "ev.node"); // already gone -> no event
    m.remove_group_node("ev-group", "g.node"); // -> None
    assert!(m.delete_group("ev-group")); // -> None
    assert!(!m.delete_group("ev-group")); // unknown -> no event

    let expected: Vec<Option<String>> = vec![Some(uuid.clone()), Some(uuid.clone()), None, None, Some(uuid.clone()), None, None];
    let seen: Vec<Option<String>> = events.lock().iter().map(|e| e.as_ref().map(|s| s.to_lowercase())).collect();
    assert_eq!(seen, expected.iter().map(|e| e.as_ref().map(|s| s.to_lowercase())).collect::<Vec<_>>());
}

#[test]
fn snapshot_is_a_deep_copy() {
    let m = create_manager();
    let uuid = new_uuid();
    m.add_user_node(&uuid, "real.node");
    m.add_group_node("snap-group", "group.node");

    let mut snap = m.snapshot();
    snap.users.get_mut(&uuid).expect("user").nodes.insert("evil.injected");
    snap.groups.get_mut("snap-group").expect("group").nodes.insert("evil.group.injected");
    snap.users.clear();

    assert!(!m.has(&uuid, "evil.injected"));
    assert!(!m.try_get_user(&uuid).expect("user").nodes.contains("evil.injected"));
    assert!(!m.try_get_group("snap-group").expect("group").nodes.contains("evil.group.injected"));
}

#[test]
fn delete_group_scrubs_user_memberships_and_group_parents() {
    let m = create_manager();
    let uuid = new_uuid();
    m.add_group_node("doomed", "doomed.node");
    m.add_user_to_group(&uuid, "doomed");
    m.add_group_parent("survivor", "doomed");
    assert!(m.has(&uuid, "doomed.node"));

    assert!(m.delete_group("doomed"));

    assert!(!m.has(&uuid, "doomed.node"));
    assert!(m.try_get_group("doomed").is_none());
    assert!(!m.try_get_user(&uuid).expect("user").groups.contains("doomed"));
    assert!(m.try_get_group("survivor").expect("survivor").parents.is_empty());
}

#[test]
fn enumeration_apis_return_exactly_the_granted_and_denied_rules() {
    let m = create_manager();
    let uuid = new_uuid();
    m.add_user_node(&uuid, "alpha.one");
    m.add_user_node(&uuid, "-beta.two");

    let allowed = m.get_all_allowed_rules(&uuid);
    let denied = m.get_all_denied_rules(&uuid);
    assert_eq!(allowed, vec!["alpha.one"]);
    assert_eq!(denied, vec!["beta.two"]);
}

// ---- persistence ----

#[test]
fn save_to_xml_load_from_xml_round_trips_groups_users_denies_and_parents() {
    let path = unique_xml_path();
    let a = create_manager();
    let u1 = new_uuid();
    let u2 = new_uuid();
    a.add_group_node("staff", "perm.a");
    a.add_group_node("staff", "-perm.b");
    a.add_group_parent("staff", "base");
    a.add_group_node("base", "perm.base");
    a.add_user_to_group(&u1, "staff");
    a.add_user_node(&u1, "user.only");
    a.add_user_node(&u2, "-blocked.node");

    a.save_to_xml(Some(&path)).expect("save");
    assert!(path.exists());

    let b = create_manager();
    let configured_path = b.get_xml_path();
    b.load_from_xml(Some(&path)).expect("load");
    assert_eq!(b.get_xml_path(), configured_path); // the override must not stick

    let staff = b.try_get_group("staff").expect("staff");
    assert!(set_equals(&staff.nodes.to_vec(), &["perm.a", "-perm.b"]));
    assert!(set_equals(&staff.parents.to_vec(), &["base"]));

    let user1 = b.try_get_user(&u1).expect("user1");
    assert!(set_equals(&user1.groups.to_vec(), &["default", "staff"]));
    assert!(set_equals(&user1.nodes.to_vec(), &["user.only"]));

    assert!(b.has(&u1, "perm.a"));
    assert!(b.has(&u1, "perm.base")); // via staff -> base inheritance
    assert!(b.has(&u1, "user.only"));
    assert!(!b.has(&u1, "perm.b")); // deny survived the round trip
    assert!(b.get_all_denied_rules(&u2).iter().any(|r| r == "blocked.node"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_from_xml_missing_file_replaces_store_with_empty() {
    let missing = unique_xml_path();
    assert!(!missing.exists());

    let direct = PermissionXml::load(&missing).expect("a missing file is an empty store");
    assert!(direct.users.is_empty());
    assert!(direct.groups.is_empty());

    let m = create_manager();
    let uuid = new_uuid();
    m.add_user_node(&uuid, "pre.load.node");
    assert!(m.has(&uuid, "pre.load.node"));

    m.load_from_xml(Some(&missing)).expect("load");
    assert!(!m.has(&uuid, "pre.load.node"));
    assert!(m.snapshot().users.is_empty());
}

/// A file that is not permissions XML is refused, and the store in memory is left as it was.
#[test]
fn load_from_xml_malformed_file_is_an_error_and_keeps_the_store() {
    let path = unique_xml_path();
    std::fs::write(&path, "this is not <xml").expect("write");
    let m = create_manager();
    let uuid = new_uuid();
    m.add_user_node(&uuid, "kept.node");

    assert!(m.load_from_xml(Some(&path)).is_err());
    assert!(m.has(&uuid, "kept.node"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn save_to_xml_debounced_writes_after_the_quiet_period() {
    let path = unique_xml_path();
    let m = PermissionManager::new();
    m.set_save_debounce_ms(20);
    m.set_xml_path(&path).expect("xml path");
    let uuid = new_uuid();
    m.add_user_node(&uuid, "debounced.node"); // schedules the save internally

    let started = Instant::now();
    while !path.exists() && started.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(path.exists(), "debounced save never hit the disk");

    let reader = create_manager();
    reader.load_from_xml(Some(&path)).expect("load");
    assert!(reader.has(&uuid, "debounced.node"));
    let _ = std::fs::remove_file(&path);
}

// ---- thread safety smoke ----

#[test]
fn parallel_mutations_and_queries_do_not_corrupt_state() {
    let m = create_manager();
    const WORKERS: usize = 4;
    const PER_WORKER: usize = 100;
    std::thread::scope(|scope| {
        for worker in 0..WORKERS {
            let m = &m;
            scope.spawn(move || {
                for i in 0..PER_WORKER {
                    let uuid = format!("par-user-{worker}-{i}");
                    m.add_user_node(&uuid, &format!("par.node.{worker}.{i}"));
                    m.add_group_node("par-shared", &format!("par.grp.{worker}.{i}"));
                    m.has(&uuid, &format!("par.node.{worker}.{i}"));
                    m.has(&format!("par-user-{worker}-{}", i as i64 - 1), "par.never.granted");
                }
            });
        }
    });
    for w in 0..WORKERS {
        for i in 0..PER_WORKER {
            assert!(m.has(&format!("par-user-{w}-{i}"), &format!("par.node.{w}.{i}")));
        }
    }
    assert_eq!(m.try_get_group("par-shared").expect("shared").nodes.len(), WORKERS * PER_WORKER);
}

// ---- PermissionIntegration singleton (GUID-isolated) ----

#[test]
fn has_valid_requirement_passes_on_exact_node_or_wildcard() {
    let manager = PermissionIntegration::manager();
    let user = format!("itg-user-{}", uuid::Uuid::new_v4().simple());
    let admin = format!("itg-admin-{}", uuid::Uuid::new_v4().simple());
    let node = format!("itg.node.{}", uuid::Uuid::new_v4().simple());

    assert!(!PermissionIntegration::has_valid_requirement_uuid(&user, &node));
    manager.add_user_node(&user, &node);
    assert!(PermissionIntegration::has_valid_requirement_uuid(&user, &node));
    assert!(!PermissionIntegration::has_valid_requirement_uuid(&user, &format!("{node}.deeper")));

    manager.add_user_node(&admin, PermNodes::ALL);
    assert!(PermissionIntegration::has_valid_requirement_uuid(&admin, &node));

    manager.remove_user_node(&user, &node);
    manager.remove_user_node(&admin, PermNodes::ALL);
}

#[test]
fn player_meta_store_query_remove_round_trips() {
    let uuid = format!("meta-user-{}", uuid::Uuid::new_v4().simple());
    let meta = ClientMetaDataMessage { player_uuid: uuid.clone(), player_display_name: "Meta Test Name".into(), player_platform: "test-platform".into() };

    PermissionIntegration::store_player_meta(&uuid, meta);
    let got = PermissionIntegration::try_get_player_meta(&uuid).expect("meta");
    assert_eq!(got.player_uuid, uuid);
    assert_eq!(got.player_display_name, "Meta Test Name");
    assert_eq!(got.player_platform, "test-platform");

    PermissionIntegration::remove_player_meta(&uuid);
    assert!(PermissionIntegration::try_get_player_meta(&uuid).is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// BasisPlayerModeration
// ─────────────────────────────────────────────────────────────────────────────

static PEER_ID_COUNTER: AtomicI32 = AtomicI32::new(50_000);

fn unique_ip() -> String {
    let b = uuid::Uuid::new_v4().into_bytes();
    format!("10.{}.{}.{}", b[0], b[1], b[2])
}

struct Moderation {
    _scope: ServerStaticsScope,
    identity: Arc<MapAuthIdentity>,
}

impl Moderation {
    fn new() -> Self {
        let scope = ServerStaticsScope::new();
        BasisPlayerModeration::set_use_file_on_disc(false);
        let identity = MapAuthIdentity::new();
        NetworkServer::set_auth_identity(Some(identity.clone()));
        Self { _scope: scope, identity }
    }

    fn connect_player(&self, ip: Option<&str>) -> (String, Arc<FakePeer>) {
        let id = PEER_ID_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        let uuid = format!("mod-user-{}", uuid::Uuid::new_v4().simple());
        let ip = ip.map(str::to_string).unwrap_or_else(unique_ip);
        let peer = FakePeer::with_address(id, ip.parse().expect("ip"));
        self.identity.register(&uuid, id);
        NetworkServer::authenticated_peers().insert(id, peer.as_ref());
        (uuid, peer)
    }
}

fn build_admin_payload(mode: AdminRequestMode, write: impl FnOnce(&mut NetDataWriter)) -> NetDataReader {
    let mut w = NetDataWriter::new();
    AdminRequest::default().serialize(&mut w, mode).expect("admin request");
    write(&mut w);
    NetDataReader::new(w.copy_data())
}

/// Parses one captured admin-channel reply; asserts it is a Message and returns the text.
fn read_admin_message(peer: &FakePeer, index: usize) -> String {
    let sent = peer.sent.lock();
    let mut r = NetDataReader::from_slice(&sent[index].data);
    let mut req = AdminRequest::default();
    req.deserialize(&mut r);
    assert_eq!(req.get_admin_request_mode(), Some(AdminRequestMode::Message));
    r.get_string().expect("message")
}

fn disconnect_payload(peer: &FakePeer) -> Vec<u8> {
    peer.disconnect_data.lock().first().cloned().expect("disconnect payload")
}

// ---- direct moderation calls ----

#[test]
#[serial(network_statics)]
fn ban_kick_ip_ban_reject_invalid_arguments_and_offline_players() {
    let _m = Moderation::new();
    assert_eq!(BasisPlayerModeration::ban("", "reason"), "UUID invalid");
    assert_eq!(BasisPlayerModeration::ban("someone", ""), "Reason invalid");
    assert_eq!(BasisPlayerModeration::kick("", "reason"), "UUID invalid");
    assert_eq!(BasisPlayerModeration::ip_ban("someone", ""), "Reason invalid");

    let offline = format!("offline-{}", uuid::Uuid::new_v4().simple());
    assert_eq!(BasisPlayerModeration::ban(&offline, "reason"), "Player not found");
    assert_eq!(BasisPlayerModeration::kick(&offline, "reason"), "Player not found");
    assert_eq!(BasisPlayerModeration::ip_ban(&offline, "reason"), "Player not found");
    assert!(!BasisPlayerModeration::is_banned(&offline));
}

#[test]
#[serial(network_statics)]
fn ban_disconnects_peer_records_reason_and_unban_round_trips() {
    let m = Moderation::new();
    let (uuid, peer) = m.connect_player(None);
    let reason = format!("griefing-{}", uuid::Uuid::new_v4().simple());

    assert_eq!(BasisPlayerModeration::ban(&uuid, &reason), format!("Player {uuid} banned."));
    assert!(BasisPlayerModeration::is_banned(&uuid));
    assert_eq!(BasisPlayerModeration::get_banned_reason(&uuid).as_deref(), Some(reason.as_str()));
    assert_eq!(peer.disconnect_calls(), 1);
    assert_eq!(disconnect_payload(&peer), reason.as_bytes());

    // A plain ban must not create an IP ban.
    assert!(!BasisPlayerModeration::is_ip_banned(&peer.address().to_string()));

    assert!(BasisPlayerModeration::unban(&uuid));
    assert!(!BasisPlayerModeration::is_banned(&uuid));
    assert!(BasisPlayerModeration::get_banned_reason(&uuid).is_none());
    assert!(!BasisPlayerModeration::unban(&uuid)); // second unban fails
}

#[test]
#[serial(network_statics)]
fn ip_ban_records_address_and_unban_ip_clears_every_matching_entry() {
    let m = Moderation::new();
    let shared_ip = unique_ip();
    let (uuid_a, peer_a) = m.connect_player(Some(&shared_ip));
    let (uuid_b, peer_b) = m.connect_player(Some(&shared_ip));
    let (uuid_c, peer_c) = m.connect_player(None);

    assert_eq!(BasisPlayerModeration::ip_ban(&uuid_a, "spam"), format!("Player {uuid_a} and IP {shared_ip} banned."));
    assert_eq!(BasisPlayerModeration::ip_ban(&uuid_b, "spam"), format!("Player {uuid_b} and IP {shared_ip} banned."));
    let other_ip = peer_c.address().to_string();
    BasisPlayerModeration::ip_ban(&uuid_c, "other");

    assert!(BasisPlayerModeration::is_ip_banned(&shared_ip));
    assert!(BasisPlayerModeration::is_ip_banned(&other_ip));
    assert!(BasisPlayerModeration::is_banned(&uuid_a));
    assert!(BasisPlayerModeration::is_banned(&uuid_b));
    assert_eq!(peer_a.disconnect_calls(), 1);
    assert_eq!(peer_b.disconnect_calls(), 1);

    // One unban_ip sweep removes every player banned under that address.
    assert!(BasisPlayerModeration::unban_ip(&shared_ip));
    assert!(!BasisPlayerModeration::is_ip_banned(&shared_ip));
    assert!(!BasisPlayerModeration::is_banned(&uuid_a));
    assert!(!BasisPlayerModeration::is_banned(&uuid_b));
    assert!(!BasisPlayerModeration::unban_ip(&shared_ip)); // nothing left
    assert!(BasisPlayerModeration::is_banned(&uuid_c)); // unrelated ip untouched

    assert!(BasisPlayerModeration::unban_ip(&other_ip));
    assert!(!BasisPlayerModeration::is_banned(&uuid_c));
}

#[test]
#[serial(network_statics)]
fn kick_disconnects_without_recording_a_ban() {
    let m = Moderation::new();
    let (uuid, peer) = m.connect_player(None);

    assert_eq!(BasisPlayerModeration::kick(&uuid, "be nicer"), format!("Player {uuid} kicked."));
    assert_eq!(peer.disconnect_calls(), 1);
    assert_eq!(disconnect_payload(&peer), b"be nicer");
    assert!(!BasisPlayerModeration::is_banned(&uuid));
}

#[test]
#[serial(network_statics)]
fn protected_players_cannot_be_banned_kicked_or_ip_banned() {
    let m = Moderation::new();
    let (uuid, peer) = m.connect_player(None);
    let perms = PermissionIntegration::manager();
    perms.add_user_node(&uuid, PermNodes::PROTECTION);

    let ban = BasisPlayerModeration::ban(&uuid, "nope");
    let kick = BasisPlayerModeration::kick(&uuid, "nope");
    let ip_ban = BasisPlayerModeration::ip_ban(&uuid, "nope");
    let banned = BasisPlayerModeration::is_banned(&uuid);
    perms.remove_user_node(&uuid, PermNodes::PROTECTION);

    assert_eq!(ban, "Target is protected");
    assert_eq!(kick, "Target is protected");
    assert_eq!(ip_ban, "Target is protected");
    assert!(!banned);
    assert_eq!(peer.disconnect_calls(), 0);
}

#[test]
#[serial(network_statics)]
fn unknown_players_query_as_safe_defaults() {
    let _m = Moderation::new();
    let unknown = format!("unknown-{}", uuid::Uuid::new_v4().simple());
    assert!(!BasisPlayerModeration::is_banned(&unknown));
    assert!(BasisPlayerModeration::get_banned_reason(&unknown).is_none());
    assert!(!BasisPlayerModeration::unban(&unknown));
    assert!(!BasisPlayerModeration::is_ip_banned(""));
    assert!(!BasisPlayerModeration::is_ip_banned("   "));
    assert!(!BasisPlayerModeration::unban_ip(&unique_ip()));
}

// ---- persistence ----

struct DiscGuard(Vec<String>);

impl Drop for DiscGuard {
    fn drop(&mut self) {
        for uuid in &self.0 {
            BasisPlayerModeration::unban(uuid);
        }
        BasisPlayerModeration::set_use_file_on_disc(false);
    }
}

fn enable_disc() {
    if let Some(parent) = BasisPlayerModeration::ban_file_path().parent() {
        std::fs::create_dir_all(parent).expect("config dir");
    }
    BasisPlayerModeration::set_use_file_on_disc(true);
}

#[test]
#[serial(network_statics)]
fn ban_state_survives_save_and_load_and_unban_persists() {
    let m = Moderation::new();
    enable_disc();
    let (uuid_a, _peer_a) = m.connect_player(None);
    let (uuid_b, peer_b) = m.connect_player(None);
    let _guard = DiscGuard(vec![uuid_a.clone(), uuid_b.clone()]);
    let ip_b = peer_b.address().to_string();

    let reason_a = format!("persist-{}", uuid::Uuid::new_v4().simple());
    BasisPlayerModeration::ban(&uuid_a, &reason_a);
    BasisPlayerModeration::ip_ban(&uuid_b, "ip persist");

    // Simulated restart: reload state purely from the ban file.
    BasisPlayerModeration::load_banned_players().expect("load");

    assert!(BasisPlayerModeration::is_banned(&uuid_a));
    assert_eq!(BasisPlayerModeration::get_banned_reason(&uuid_a).as_deref(), Some(reason_a.as_str()));
    assert!(BasisPlayerModeration::is_banned(&uuid_b));
    assert!(BasisPlayerModeration::is_ip_banned(&ip_b));

    // Unban also persists (writes the file), so a second reload stays clean.
    assert!(BasisPlayerModeration::unban(&uuid_a));
    assert!(BasisPlayerModeration::unban(&uuid_b));
    BasisPlayerModeration::load_banned_players().expect("load");
    assert!(!BasisPlayerModeration::is_banned(&uuid_a));
    assert!(!BasisPlayerModeration::is_banned(&uuid_b));
    assert!(!BasisPlayerModeration::is_ip_banned(&ip_b));
}

#[test]
#[serial(network_statics)]
fn load_banned_players_missing_file_keeps_in_memory_state_and_recreates_the_file() {
    let m = Moderation::new();
    enable_disc();
    let (uuid, _peer) = m.connect_player(None);
    let _guard = DiscGuard(vec![uuid.clone()]);
    let ban_file = BasisPlayerModeration::ban_file_path();

    BasisPlayerModeration::ban(&uuid, "keep me");
    let _ = std::fs::remove_file(&ban_file);

    BasisPlayerModeration::load_banned_players().expect("load");

    assert!(BasisPlayerModeration::is_banned(&uuid));
    assert!(ban_file.exists());

    // The recreated file really contains the in-memory state.
    BasisPlayerModeration::load_banned_players().expect("load");
    assert!(BasisPlayerModeration::is_banned(&uuid));
}

/// A ban file that has been corrupted on disc is reported, not silently swallowed, and the bans
/// already in memory are kept.
#[test]
#[serial(network_statics)]
fn load_banned_players_corrupt_file_is_an_error_and_keeps_in_memory_state() {
    let m = Moderation::new();
    enable_disc();
    let (uuid, _peer) = m.connect_player(None);
    let _guard = DiscGuard(vec![uuid.clone()]);
    BasisPlayerModeration::ban(&uuid, "keep me");

    let ban_file = BasisPlayerModeration::ban_file_path();
    std::fs::write(&ban_file, "<BannedPlayers><Banned").expect("corrupt");
    assert!(BasisPlayerModeration::load_banned_players().is_err());
    assert!(BasisPlayerModeration::is_banned(&uuid));
    // Persisting again repairs the file.
    assert!(BasisPlayerModeration::unban(&uuid));
    BasisPlayerModeration::load_banned_players().expect("load");
    assert!(!BasisPlayerModeration::is_banned(&uuid));
}

// ---- on_admin_message permission gating (no live sockets needed) ----

#[test]
#[serial(network_statics)]
fn on_admin_message_unauthenticated_peer_gets_uuid_not_found() {
    let _m = Moderation::new();
    // Peer intentionally NOT registered with the auth identity.
    let stranger = FakePeer::with_address(PEER_ID_COUNTER.fetch_add(1, Ordering::Relaxed) + 1, unique_ip().parse().expect("ip"));

    BasisPlayerModeration::on_admin_message(&stranger.as_ref(), build_admin_payload(AdminRequestMode::Ban, |w| {
        w.put_string("victim").expect("put");
        w.put_string("reason").expect("put");
    }));

    assert_eq!(stranger.sent_count(), 1);
    assert_eq!(read_admin_message(&stranger, 0), "UUID not found");
}

#[test]
#[serial(network_statics)]
fn on_admin_message_ban_without_permission_is_refused_and_target_unaffected() {
    let m = Moderation::new();
    let (_admin_uuid, admin_peer) = m.connect_player(None);
    let (target_uuid, target_peer) = m.connect_player(None);

    BasisPlayerModeration::on_admin_message(&admin_peer.as_ref(), build_admin_payload(AdminRequestMode::Ban, |w| {
        w.put_string(&target_uuid).expect("put");
        w.put_string("no rights").expect("put");
    }));

    assert_eq!(admin_peer.sent_count(), 1);
    assert_eq!(read_admin_message(&admin_peer, 0), format!("No permission: {}", PermNodes::MODERATION_BAN));
    assert_eq!(admin_peer.sent.lock()[0].channel, BasisNetworkCommons::ADMIN_CHANNEL);
    assert!(!BasisPlayerModeration::is_banned(&target_uuid));
    assert_eq!(target_peer.disconnect_calls(), 0);
}

#[test]
#[serial(network_statics)]
fn on_admin_message_ban_with_moderation_ban_node_bans_the_target() {
    let m = Moderation::new();
    let (admin_uuid, admin_peer) = m.connect_player(None);
    let (target_uuid, target_peer) = m.connect_player(None);
    let perms = PermissionIntegration::manager();
    perms.add_user_node(&admin_uuid, PermNodes::MODERATION_BAN);

    BasisPlayerModeration::on_admin_message(&admin_peer.as_ref(), build_admin_payload(AdminRequestMode::Ban, |w| {
        w.put_string(&target_uuid).expect("put");
        w.put_string("admin banhammer").expect("put");
    }));

    let banned = BasisPlayerModeration::is_banned(&target_uuid);
    let reply = read_admin_message(&admin_peer, 0);
    BasisPlayerModeration::unban(&target_uuid);
    perms.remove_user_node(&admin_uuid, PermNodes::MODERATION_BAN);

    assert!(banned);
    assert_eq!(target_peer.disconnect_calls(), 1);
    assert_eq!(admin_peer.sent_count(), 1);
    assert_eq!(reply, format!("Player {target_uuid} banned."));
}

/// A truncated admin request must be dropped without a reply and without touching the target.
#[test]
#[serial(network_statics)]
fn on_admin_message_truncated_request_is_dropped() {
    let m = Moderation::new();
    let (admin_uuid, admin_peer) = m.connect_player(None);
    let (target_uuid, target_peer) = m.connect_player(None);
    let perms = PermissionIntegration::manager();
    perms.add_user_node(&admin_uuid, PermNodes::MODERATION_BAN);

    // Mode byte, then a string length that claims more than is present.
    let mut w = NetDataWriter::new();
    AdminRequest::default().serialize(&mut w, AdminRequestMode::Ban).expect("admin request");
    w.put_ushort(400);
    w.put_byte(b'x');
    BasisPlayerModeration::on_admin_message(&admin_peer.as_ref(), NetDataReader::new(w.copy_data()));
    BasisPlayerModeration::on_admin_message(&admin_peer.as_ref(), NetDataReader::from_slice(&[]));

    perms.remove_user_node(&admin_uuid, PermNodes::MODERATION_BAN);
    assert!(!BasisPlayerModeration::is_banned(&target_uuid));
    assert_eq!(target_peer.disconnect_calls(), 0);
}

#[test]
#[serial(network_statics)]
fn on_admin_message_get_permissions_requires_view_then_serializes_the_snapshot() {
    let m = Moderation::new();
    let (admin_uuid, admin_peer) = m.connect_player(None);
    let perms = PermissionIntegration::manager();

    let marker_group = format!("grp-{}", uuid::Uuid::new_v4().simple());
    let marker_parent = format!("par-{}", uuid::Uuid::new_v4().simple());
    let marker_group_node = format!("marker.group.{}", uuid::Uuid::new_v4().simple());
    let marker_user = format!("marker-user-{}", uuid::Uuid::new_v4().simple());
    let marker_user_node = format!("marker.user.{}", uuid::Uuid::new_v4().simple());

    // Without the view node the request is refused outright.
    BasisPlayerModeration::on_admin_message(&admin_peer.as_ref(), build_admin_payload(AdminRequestMode::GetPermissions, |_| {}));
    assert_eq!(read_admin_message(&admin_peer, 0), "No permission: view");
    admin_peer.clear_sent();

    perms.add_group_node(&marker_group, &marker_group_node);
    perms.add_group_parent(&marker_group, &marker_parent);
    perms.add_user_to_group(&marker_user, &marker_group);
    perms.add_user_node(&marker_user, &marker_user_node);
    perms.add_user_node(&admin_uuid, PermNodes::PERMISSIONS_VIEW);

    BasisPlayerModeration::on_admin_message(&admin_peer.as_ref(), build_admin_payload(AdminRequestMode::GetPermissions, |_| {}));

    let outcome = (|| -> Result<(), String> {
        let sent = admin_peer.sent.lock();
        if sent.len() != 1 {
            return Err(format!("expected one reply, got {}", sent.len()));
        }
        let mut r = NetDataReader::from_slice(&sent[0].data);
        let mut reply = AdminRequest::default();
        reply.deserialize(&mut r);
        if reply.get_admin_request_mode() != Some(AdminRequestMode::GetPermissions) {
            return Err("reply is not GetPermissions".into());
        }
        let read = |r: &mut NetDataReader| r.get_string().map_err(|e| e.to_string());
        let count = |r: &mut NetDataReader| r.get_int().map_err(|e| e.to_string());

        // [int groupCount] { name, [int nodes] n*, [int parents] p* } then
        // [int userCount]  { uuid, [int groups] g*, [int nodes] n* }.
        let mut saw_group = false;
        let group_count = count(&mut r)?;
        for _ in 0..group_count {
            let name = read(&mut r)?;
            let node_count = count(&mut r)?;
            let mut nodes = Vec::new();
            for _ in 0..node_count {
                nodes.push(read(&mut r)?);
            }
            let parent_count = count(&mut r)?;
            let mut parents = Vec::new();
            for _ in 0..parent_count {
                parents.push(read(&mut r)?);
            }
            if name.eq_ignore_ascii_case(&marker_group) {
                saw_group = true;
                if !nodes.iter().any(|n| n.eq_ignore_ascii_case(&marker_group_node)) || !parents.iter().any(|p| p.eq_ignore_ascii_case(&marker_parent)) {
                    return Err("marker group content missing".into());
                }
            }
        }
        let mut saw_user = false;
        let user_count = count(&mut r)?;
        for _ in 0..user_count {
            let uuid = read(&mut r)?;
            let group_memberships = count(&mut r)?;
            let mut groups = Vec::new();
            for _ in 0..group_memberships {
                groups.push(read(&mut r)?);
            }
            let node_count = count(&mut r)?;
            let mut nodes = Vec::new();
            for _ in 0..node_count {
                nodes.push(read(&mut r)?);
            }
            if uuid.eq_ignore_ascii_case(&marker_user) {
                saw_user = true;
                if !groups.iter().any(|g| g.eq_ignore_ascii_case(&marker_group)) || !nodes.iter().any(|n| n.eq_ignore_ascii_case(&marker_user_node)) {
                    return Err("marker user content missing".into());
                }
            }
        }
        if !saw_group {
            return Err("marker group missing from GetPermissions payload".into());
        }
        if !saw_user {
            return Err("marker user missing from GetPermissions payload".into());
        }
        if r.available_bytes() != 0 {
            return Err("trailing bytes".into());
        }
        Ok(())
    })();

    perms.delete_group(&marker_group);
    perms.remove_user_node(&marker_user, &marker_user_node);
    perms.remove_user_node(&admin_uuid, PermNodes::PERMISSIONS_VIEW);
    outcome.unwrap_or_else(|e| panic!("{e}"));
}
