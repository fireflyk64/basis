//! File-backed allow/ban lists, the runtime-only rejoin lockdown, the server-wide lock toggles
//! and their wire layout, and the content-share resource caps.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use basis_network_core::BasisNetworkCommons;
use basis_network_core::SerializableBasis::{AdminRequestMode, ClientMetaDataMessage};
use basis_network_core::configuration::Configuration;
use basis_network_core::identity::BasisUserRestrictionMode;
use basis_network_core::transport::DeliveryMethod;
use basis_network_core::{ConnectionRequest, NetDataReader, NetPeerRef};
use basis_network_server::NetworkServer;
use basis_network_server::auth::IAuthIdentity;
use basis_network_server::core::basis_server_handle_events::BasisServerHandleEvents;
use basis_network_server::networking::BasisNetworkChat;
use basis_network_server::security::{BasisAllowList, BasisBanList, BasisGlobalLockManager, BasisRejoinLockManager, BasisResourceLimitManager, PermNodes, PermissionIntegration};
use basis_server_tests::support::FakePeer;
use serial_test::serial;

struct TempFile(PathBuf);

impl TempFile {
    fn new(prefix: &str) -> Self {
        Self(std::env::temp_dir().join(format!("{prefix}-{}.txt", uuid::Uuid::new_v4().simple())))
    }

    fn lines(&self) -> Vec<String> {
        std::fs::read_to_string(&self.0).expect("read").lines().map(str::to_string).collect()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}

// ── BasisAllowList: ordinal case-sensitive membership, one id per line ──

#[test]
fn allow_list_missing_file_nothing_allowed_and_no_file_created() {
    let file = TempFile::new("BasisAllowListTests");
    let list = BasisAllowList::with_file(&file.0);
    assert!(!list.is_allowed(&new_id("Player")));
    assert!(!list.is_allowed(""));
    assert!(!file.0.exists());
}

#[test]
fn allow_list_add_allows_case_sensitively_and_appends_to_file() {
    let file = TempFile::new("BasisAllowListTests");
    let list = BasisAllowList::with_file(&file.0);
    let id = new_id("Player-MiXeD");
    list.add_to_allowlist(&id).expect("add");

    assert!(list.is_allowed(&id));
    assert!(!list.is_allowed(&id.to_lowercase()));
    assert!(!list.is_allowed(&id.to_uppercase()));
    assert!(file.lines().contains(&id));
}

#[test]
fn allow_list_add_same_id_twice_appends_only_once() {
    let file = TempFile::new("BasisAllowListTests");
    let list = BasisAllowList::with_file(&file.0);
    let id = new_id("Player");
    assert!(list.add_to_allowlist(&id).expect("add"));
    assert!(!list.add_to_allowlist(&id).expect("add again"));
    assert_eq!(file.lines(), vec![id]);
}

#[test]
fn allow_list_remove_disallows_and_rewrites_file_without_the_id() {
    let file = TempFile::new("BasisAllowListTests");
    let list = BasisAllowList::with_file(&file.0);
    let keep = new_id("keep");
    let drop = new_id("drop");
    list.add_to_allowlist(&keep).expect("add");
    list.add_to_allowlist(&drop).expect("add");

    list.remove_from_allowlist(&drop).expect("remove");

    assert!(!list.is_allowed(&drop));
    assert!(list.is_allowed(&keep));
    assert_eq!(file.lines(), vec![keep]);
}

#[test]
fn allow_list_remove_unknown_id_is_a_no_op() {
    let file = TempFile::new("BasisAllowListTests");
    let list = BasisAllowList::with_file(&file.0);
    assert!(!list.remove_from_allowlist(&new_id("Player")).expect("remove"));
    assert!(!file.0.exists());
}

#[test]
fn allow_list_persisted_list_is_visible_to_a_second_instance_after_reload() {
    let file = TempFile::new("BasisAllowListTests");
    let writer = BasisAllowList::with_file(&file.0);
    let id_a = new_id("a");
    let id_b = new_id("b");
    writer.add_to_allowlist(&id_a).expect("add");
    writer.add_to_allowlist(&id_b).expect("add");

    let reader = BasisAllowList::with_file(&file.0);
    reader.reload_allowlist().expect("reload");

    assert!(reader.is_allowed(&id_a));
    assert!(reader.is_allowed(&id_b));
    assert!(!reader.is_allowed(&new_id("absent")));
}

#[test]
fn allow_list_load_trims_whitespace_and_skips_blank_lines() {
    let file = TempFile::new("BasisAllowListTests");
    let id = new_id("Player");
    std::fs::write(&file.0, format!("  {id}  \n\n   \n")).expect("write");

    let list = BasisAllowList::with_file(&file.0);
    list.reload_allowlist().expect("reload");

    assert!(list.is_allowed(&id));
    assert!(!list.is_allowed(&format!("  {id}  ")));
    assert!(!list.is_allowed(""));
}

/// A list whose file cannot be written reports the failure instead of silently keeping the
/// entry only in memory.
#[test]
fn allow_list_add_into_an_unwritable_path_is_an_error() {
    let dir = std::env::temp_dir().join(format!("BasisAllowListTests-dir-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    // The "file" is a directory: appending to it must fail.
    let list = BasisAllowList::with_file(&dir);
    assert!(list.add_to_allowlist(&new_id("Player")).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

// ── BasisBanList: same persistence contract, loaded synchronously on construction ──

#[test]
fn ban_list_missing_file_nobody_banned_and_no_file_created() {
    let file = TempFile::new("BasisBanListTests");
    let list = BasisBanList::with_file(&file.0);
    assert!(!list.is_banned(&new_id("Player")));
    assert!(!file.0.exists());
}

#[test]
fn ban_list_add_bans_case_sensitively_and_appends_to_file() {
    let file = TempFile::new("BasisBanListTests");
    let list = BasisBanList::with_file(&file.0);
    let id = new_id("Griefer-MiXeD");
    list.add_to_ban_list(&id).expect("add");

    assert!(list.is_banned(&id));
    assert!(!list.is_banned(&id.to_lowercase()));
    assert!(!list.is_banned(&id.to_uppercase()));
    assert!(file.lines().contains(&id));
}

#[test]
fn ban_list_add_same_id_twice_appends_only_once() {
    let file = TempFile::new("BasisBanListTests");
    let list = BasisBanList::with_file(&file.0);
    let id = new_id("Player");
    list.add_to_ban_list(&id).expect("add");
    list.add_to_ban_list(&id).expect("add again");
    assert_eq!(file.lines(), vec![id]);
}

#[test]
fn ban_list_remove_unbans_and_rewrites_file_without_the_id() {
    let file = TempFile::new("BasisBanListTests");
    let list = BasisBanList::with_file(&file.0);
    let keep = new_id("keep");
    let drop = new_id("drop");
    list.add_to_ban_list(&keep).expect("add");
    list.add_to_ban_list(&drop).expect("add");

    list.remove_from_ban_list(&drop).expect("remove");

    assert!(!list.is_banned(&drop));
    assert!(list.is_banned(&keep));
    assert_eq!(file.lines(), vec![keep]);
}

#[test]
fn ban_list_constructor_loads_existing_file_synchronously_trimming_and_skipping_blanks() {
    let file = TempFile::new("BasisBanListTests");
    let id = new_id("Player");
    std::fs::write(&file.0, format!("  {id}  \n\n   \n")).expect("write");

    let list = BasisBanList::with_file(&file.0);

    assert!(list.is_banned(&id));
    assert!(!list.is_banned(&format!("  {id}  ")));
}

#[test]
fn ban_list_reload_picks_up_external_file_edits() {
    let file = TempFile::new("BasisBanListTests");
    let list = BasisBanList::with_file(&file.0);
    let id = new_id("Player");
    assert!(!list.is_banned(&id));

    std::fs::write(&file.0, format!("{id}\n")).expect("write");
    list.reload_ban_list().expect("reload");

    assert!(list.is_banned(&id));
}

// ── BasisRejoinLockManager: runtime-only, populated only by capturing the current peers ──

struct FixedUuidAuthIdentity(String);

impl IAuthIdentity for FixedUuidAuthIdentity {
    fn process_connection(&self, _c: &Configuration, _r: &Arc<dyn ConnectionRequest>, _d: NetDataReader, _p: &NetPeerRef) {}
    fn de_initialize(&self) {}
    fn remove_connection(&self, _net_peer: i32) {}
    fn remove_connection_expected(&self, _net_peer: i32, _expected: &NetPeerRef) -> bool {
        false
    }
    fn net_id_to_uuid(&self, _peer: &NetPeerRef) -> Option<String> {
        Some(self.0.clone())
    }
    fn uuid_to_net_id(&self, _uuid: &str) -> Option<i32> {
        None
    }
}

struct IdentityGuard(Option<Arc<dyn IAuthIdentity>>, Vec<i32>);

impl IdentityGuard {
    fn new() -> Self {
        Self(NetworkServer::auth_identity(), Vec::new())
    }

    fn add_peer(&mut self, id: i32) {
        NetworkServer::authenticated_peers().insert(id, FakePeer::new(id).as_ref());
        self.1.push(id);
    }
}

impl Drop for IdentityGuard {
    fn drop(&mut self) {
        for id in &self.1 {
            NetworkServer::authenticated_peers().remove(id);
        }
        NetworkServer::set_auth_identity(self.0.take());
        BasisRejoinLockManager::clear();
    }
}

#[test]
#[serial(network_statics)]
fn rejoin_fresh_or_cleared_nothing_is_allowed() {
    BasisRejoinLockManager::clear();
    assert_eq!(BasisRejoinLockManager::count(), 0);
    assert!(!BasisRejoinLockManager::is_allowed("nobody"));
    assert!(!BasisRejoinLockManager::is_allowed(""));
}

#[test]
#[serial(network_statics)]
fn rejoin_capture_without_auth_identity_leaves_the_set_empty() {
    let _g = IdentityGuard::new();
    NetworkServer::set_auth_identity(None);
    BasisRejoinLockManager::capture_current_population();
    assert_eq!(BasisRejoinLockManager::count(), 0);
}

#[test]
#[serial(network_statics)]
fn rejoin_capture_snapshots_connected_uuids_so_they_may_rejoin() {
    let mut g = IdentityGuard::new();
    let uuid = new_id("rejoin");
    NetworkServer::set_auth_identity(Some(Arc::new(FixedUuidAuthIdentity(uuid.clone()))));
    g.add_peer(910001);

    BasisRejoinLockManager::capture_current_population();

    assert_eq!(BasisRejoinLockManager::count(), 1);
    assert!(BasisRejoinLockManager::is_allowed(&uuid));
    assert!(!BasisRejoinLockManager::is_allowed(&format!("stranger-{uuid}")));
}

#[test]
#[serial(network_statics)]
fn rejoin_capture_replaces_the_previous_snapshot() {
    let mut g = IdentityGuard::new();
    let first = new_id("first");
    let second = new_id("second");
    g.add_peer(910002);

    NetworkServer::set_auth_identity(Some(Arc::new(FixedUuidAuthIdentity(first.clone()))));
    BasisRejoinLockManager::capture_current_population();
    assert!(BasisRejoinLockManager::is_allowed(&first));

    NetworkServer::set_auth_identity(Some(Arc::new(FixedUuidAuthIdentity(second.clone()))));
    BasisRejoinLockManager::capture_current_population();

    assert!(!BasisRejoinLockManager::is_allowed(&first));
    assert!(BasisRejoinLockManager::is_allowed(&second));
    assert_eq!(BasisRejoinLockManager::count(), 1);
}

#[test]
#[serial(network_statics)]
fn rejoin_clear_revokes_every_captured_uuid() {
    let mut g = IdentityGuard::new();
    let uuid = new_id("revoked");
    NetworkServer::set_auth_identity(Some(Arc::new(FixedUuidAuthIdentity(uuid.clone()))));
    g.add_peer(910003);
    BasisRejoinLockManager::capture_current_population();
    assert!(BasisRejoinLockManager::is_allowed(&uuid));

    BasisRejoinLockManager::clear();

    assert!(!BasisRejoinLockManager::is_allowed(&uuid));
    assert_eq!(BasisRejoinLockManager::count(), 0);
}

// ── BasisGlobalLockManager ──

fn all_unlocked() -> Configuration {
    Configuration {
        avatars_locked: false,
        props_locked: false,
        worlds_locked: false,
        servers_locked: false,
        third_person_disabled: false,
        additional_avatar_data_lock: false,
        camera_metadata_disallow_mask: 0,
        playspace_mover_locked: false,
        direct_connect_locked: false,
        cilbox_locked: false,
        images_locked: false,
        end_effector_ik_disabled: false,
        text_chat_locked: false,
        voice_chat_locked: false,
        media_player_locked: false,
        camera_capture_locked: false,
        prop_grabbing_locked: false,
        safe_display_names_forced: false,
        ..Configuration::default()
    }
}

fn all_locked() -> Configuration {
    Configuration {
        avatars_locked: true,
        props_locked: true,
        worlds_locked: true,
        servers_locked: true,
        third_person_disabled: true,
        additional_avatar_data_lock: true,
        camera_metadata_disallow_mask: 0xAB,
        playspace_mover_locked: true,
        direct_connect_locked: true,
        cilbox_locked: true,
        images_locked: true,
        end_effector_ik_disabled: true,
        text_chat_locked: true,
        voice_chat_locked: true,
        media_player_locked: true,
        camera_capture_locked: true,
        prop_grabbing_locked: true,
        safe_display_names_forced: true,
        ..Configuration::default()
    }
}

fn assert_all_flags(expected: bool) {
    assert_eq!(BasisGlobalLockManager::avatars_locked(), expected);
    assert_eq!(BasisGlobalLockManager::props_locked(), expected);
    assert_eq!(BasisGlobalLockManager::worlds_locked(), expected);
    assert_eq!(BasisGlobalLockManager::servers_locked(), expected);
    assert_eq!(BasisGlobalLockManager::third_person_disabled(), expected);
    assert_eq!(BasisGlobalLockManager::additional_avatar_data_lock(), expected);
    assert_eq!(BasisGlobalLockManager::playspace_mover_locked(), expected);
    assert_eq!(BasisGlobalLockManager::direct_connect_locked(), expected);
    assert_eq!(BasisGlobalLockManager::cilbox_locked(), expected);
    assert_eq!(BasisGlobalLockManager::images_locked(), expected);
    assert_eq!(BasisGlobalLockManager::end_effector_ik_disabled(), expected);
    assert_eq!(BasisGlobalLockManager::text_chat_locked(), expected);
    assert_eq!(BasisGlobalLockManager::voice_chat_locked(), expected);
    assert_eq!(BasisGlobalLockManager::media_player_locked(), expected);
    assert_eq!(BasisGlobalLockManager::camera_capture_locked(), expected);
    assert_eq!(BasisGlobalLockManager::prop_grabbing_locked(), expected);
    assert_eq!(BasisGlobalLockManager::safe_display_names_forced(), expected);
}

struct UnlockOnDrop;

impl Drop for UnlockOnDrop {
    fn drop(&mut self) {
        BasisGlobalLockManager::initialize_from_config(&all_unlocked());
    }
}

/// Every lock seeds itself from configuration at boot, so a toggle that never reaches
/// configuration silently reverts on restart. write_to_config must carry every field back.
#[test]
#[serial(network_statics)]
fn write_to_config_round_trips_every_flag_and_the_mask() {
    let _u = UnlockOnDrop;
    BasisGlobalLockManager::initialize_from_config(&all_locked());

    let mut persisted = all_unlocked();
    BasisGlobalLockManager::write_to_config(&mut persisted);

    BasisGlobalLockManager::initialize_from_config(&all_unlocked());
    assert_all_flags(false);
    BasisGlobalLockManager::initialize_from_config(&persisted);
    assert_all_flags(true);
    assert_eq!(BasisGlobalLockManager::camera_metadata_disallow_mask(), 0xAB);
}

#[test]
#[serial(network_statics)]
fn initialize_from_config_seeds_every_flag_and_the_mask() {
    {
        let _u = UnlockOnDrop;
        BasisGlobalLockManager::initialize_from_config(&all_locked());
        assert_all_flags(true);
        assert_eq!(BasisGlobalLockManager::camera_metadata_disallow_mask(), 0xAB);
    }
    assert_all_flags(false);
    assert_eq!(BasisGlobalLockManager::camera_metadata_disallow_mask(), 0);
}

#[test]
#[serial(network_statics)]
fn default_configuration_boots_with_only_worlds_locked() {
    let _u = UnlockOnDrop;
    BasisGlobalLockManager::initialize_from_config(&Configuration::default());
    assert!(BasisGlobalLockManager::worlds_locked());
    assert!(!BasisGlobalLockManager::avatars_locked());
    assert!(!BasisGlobalLockManager::props_locked());
    assert!(!BasisGlobalLockManager::servers_locked());
    assert!(!BasisGlobalLockManager::third_person_disabled());
    assert!(!BasisGlobalLockManager::additional_avatar_data_lock());
    assert!(!BasisGlobalLockManager::playspace_mover_locked());
    assert!(!BasisGlobalLockManager::direct_connect_locked());
    assert!(!BasisGlobalLockManager::cilbox_locked());
    assert!(!BasisGlobalLockManager::images_locked());
    assert!(!BasisGlobalLockManager::end_effector_ik_disabled());
    assert!(!BasisGlobalLockManager::text_chat_locked());
    assert!(!BasisGlobalLockManager::voice_chat_locked());
    assert!(!BasisGlobalLockManager::media_player_locked());
    assert!(!BasisGlobalLockManager::camera_capture_locked());
    assert!(!BasisGlobalLockManager::prop_grabbing_locked());
    assert_eq!(BasisGlobalLockManager::camera_metadata_disallow_mask(), 0);
}

#[test]
#[serial(network_statics)]
fn every_toggle_flips_its_flag_and_returns_the_new_state() {
    let _u = UnlockOnDrop;
    BasisGlobalLockManager::initialize_from_config(&all_unlocked());
    let toggles: [(fn() -> bool, fn() -> bool); 17] = [
        (BasisGlobalLockManager::toggle_avatars, BasisGlobalLockManager::avatars_locked),
        (BasisGlobalLockManager::toggle_props, BasisGlobalLockManager::props_locked),
        (BasisGlobalLockManager::toggle_worlds, BasisGlobalLockManager::worlds_locked),
        (BasisGlobalLockManager::toggle_servers, BasisGlobalLockManager::servers_locked),
        (BasisGlobalLockManager::toggle_third_person, BasisGlobalLockManager::third_person_disabled),
        (BasisGlobalLockManager::toggle_additional_avatar_data_lock, BasisGlobalLockManager::additional_avatar_data_lock),
        (BasisGlobalLockManager::toggle_playspace_mover, BasisGlobalLockManager::playspace_mover_locked),
        (BasisGlobalLockManager::toggle_direct_connect, BasisGlobalLockManager::direct_connect_locked),
        (BasisGlobalLockManager::toggle_cilbox, BasisGlobalLockManager::cilbox_locked),
        (BasisGlobalLockManager::toggle_images, BasisGlobalLockManager::images_locked),
        (BasisGlobalLockManager::toggle_end_effector_ik, BasisGlobalLockManager::end_effector_ik_disabled),
        (BasisGlobalLockManager::toggle_text_chat, BasisGlobalLockManager::text_chat_locked),
        (BasisGlobalLockManager::toggle_voice_chat, BasisGlobalLockManager::voice_chat_locked),
        (BasisGlobalLockManager::toggle_media_player, BasisGlobalLockManager::media_player_locked),
        (BasisGlobalLockManager::toggle_camera_capture, BasisGlobalLockManager::camera_capture_locked),
        (BasisGlobalLockManager::toggle_prop_grabbing, BasisGlobalLockManager::prop_grabbing_locked),
        (BasisGlobalLockManager::toggle_safe_display_names, BasisGlobalLockManager::safe_display_names_forced),
    ];
    for (toggle, state) in toggles {
        assert!(!state());
        assert!(toggle());
        assert!(state());
        assert!(!toggle());
        assert!(!state());
    }
}

#[test]
#[serial(network_statics)]
fn camera_metadata_mask_round_trips_the_whole_byte() {
    BasisGlobalLockManager::set_camera_metadata_disallow_mask(0x5A);
    assert_eq!(BasisGlobalLockManager::camera_metadata_disallow_mask(), 0x5A);
    BasisGlobalLockManager::set_camera_metadata_disallow_mask(u8::MAX);
    assert_eq!(BasisGlobalLockManager::camera_metadata_disallow_mask(), u8::MAX);
    BasisGlobalLockManager::set_camera_metadata_disallow_mask(0);
    assert_eq!(BasisGlobalLockManager::camera_metadata_disallow_mask(), 0);
}

#[test]
#[serial(network_statics)]
fn parallel_toggles_alternate_atomically() {
    let _u = UnlockOnDrop;
    BasisGlobalLockManager::initialize_from_config(&all_unlocked());
    let locked_results = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let locked_results = &locked_results;
            scope.spawn(move || {
                for _ in 0..25 {
                    if BasisGlobalLockManager::toggle_images() {
                        locked_results.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
        }
    });
    // 100 atomic flips from unlocked: the state alternates strictly, so exactly half of the calls
    // observe the locked state and the final state is unlocked again.
    assert_eq!(locked_results.load(Ordering::Relaxed), 50);
    assert!(!BasisGlobalLockManager::images_locked());
}

#[test]
#[serial(network_statics)]
fn send_lock_state_to_peer_writes_the_append_only_wire_layout() {
    let _u = UnlockOnDrop;
    let previous = NetworkServer::configuration();
    NetworkServer::set_configuration(Configuration { basis_user_restriction_mode: BasisUserRestrictionMode::AllowList, ..Configuration::default() });
    BasisGlobalLockManager::initialize_from_config(&Configuration {
        avatars_locked: true,
        props_locked: false,
        worlds_locked: true,
        servers_locked: false,
        third_person_disabled: true,
        additional_avatar_data_lock: false,
        camera_metadata_disallow_mask: 0x5A,
        playspace_mover_locked: true,
        direct_connect_locked: false,
        cilbox_locked: true,
        images_locked: false,
        end_effector_ik_disabled: true,
        text_chat_locked: true,
        voice_chat_locked: false,
        media_player_locked: true,
        camera_capture_locked: false,
        prop_grabbing_locked: true,
        safe_display_names_forced: true,
        ..Configuration::default()
    });

    let peer = FakePeer::new(1);
    BasisGlobalLockManager::send_lock_state_to_peer(&peer.as_ref());

    let sent = peer.sent.lock();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].channel, BasisNetworkCommons::ADMIN_CHANNEL);
    assert_eq!(sent[0].delivery, DeliveryMethod::ReliableOrdered);
    assert_eq!(
        sent[0].data,
        vec![
            AdminRequestMode::GlobalGetLockState as u8,
            1, 0, 1, 0, // avatars, props, worlds, servers
            1, 0, // third person, additional avatar data
            0x5A, // camera metadata disallow mask
            BasisUserRestrictionMode::AllowList as u8, // restriction mode
            1, 0, // playspace mover, direct connect
            1, 0, // cilbox, images
            1, // end-effector IK disabled
            1, // text chat locked
            0, 1, 0, 1, // voice, media player, camera capture, prop grabbing
            1, // safe display names forced
        ]
    );
    drop(sent);

    BasisGlobalLockManager::broadcast_lock_state(); // zero connected peers: must be a safe no-op
    match previous {
        Some(c) => NetworkServer::set_configuration((*c).clone()),
        None => NetworkServer::clear_configuration(),
    }
}

/// The text-chat lock is enforced server-side, so the gate itself is the security boundary — it
/// must block only while the lock is on, and must let bypass holders through.
#[test]
#[serial(network_statics)]
fn is_chat_blocked_for_uuid_blocks_only_locked_users_without_the_bypass_node() {
    let _u = UnlockOnDrop;
    let manager = PermissionIntegration::manager();
    let plain = new_id("chat-plain");
    let bypass = new_id("chat-bypass");
    manager.add_user_node(&bypass, PermNodes::CHAT_LOCK_BYPASS);

    BasisGlobalLockManager::initialize_from_config(&all_unlocked());
    assert!(!BasisNetworkChat::is_chat_blocked_for_uuid(&plain));
    assert!(!BasisNetworkChat::is_chat_blocked_for_uuid(&bypass));

    assert!(BasisGlobalLockManager::toggle_text_chat());
    let blocked_plain = BasisNetworkChat::is_chat_blocked_for_uuid(&plain);
    let blocked_bypass = BasisNetworkChat::is_chat_blocked_for_uuid(&bypass);
    manager.remove_user_node(&bypass, PermNodes::CHAT_LOCK_BYPASS);
    assert!(blocked_plain);
    assert!(!blocked_bypass);
}

/// Voice is the other server-enforced lock, and it gates both the normal and shout paths.
#[test]
#[serial(network_statics)]
fn is_voice_blocked_for_uuid_blocks_only_locked_users_without_the_bypass_node() {
    let _u = UnlockOnDrop;
    let manager = PermissionIntegration::manager();
    let plain = new_id("voice-plain");
    let bypass = new_id("voice-bypass");
    manager.add_user_node(&bypass, PermNodes::VOICE_LOCK_BYPASS);

    BasisGlobalLockManager::initialize_from_config(&all_unlocked());
    assert!(!BasisServerHandleEvents::is_voice_blocked_for_uuid(&plain));
    assert!(!BasisServerHandleEvents::is_voice_blocked_for_uuid(&bypass));

    assert!(BasisGlobalLockManager::toggle_voice_chat());
    let blocked_plain = BasisServerHandleEvents::is_voice_blocked_for_uuid(&plain);
    let blocked_bypass = BasisServerHandleEvents::is_voice_blocked_for_uuid(&bypass);
    manager.remove_user_node(&bypass, PermNodes::VOICE_LOCK_BYPASS);
    assert!(blocked_plain);
    assert!(!blocked_bypass);
}

// ── BasisResourceLimitManager ──

struct RestoreLimits;

impl Drop for RestoreLimits {
    fn drop(&mut self) {
        BasisResourceLimitManager::set_limits(32);
    }
}

#[test]
#[serial(network_statics)]
fn set_limits_sanitizes_the_cap() {
    let _r = RestoreLimits;
    for (spheres, expected) in [(40, 40), (1, 1), (0, 32), (-7, 32), (4096, 4096), (i32::MAX, 4096)] {
        BasisResourceLimitManager::set_limits(spheres);
        assert_eq!(BasisResourceLimitManager::max_content_spheres_per_player(), expected, "{spheres}");
    }
}

#[test]
#[serial(network_statics)]
fn set_limits_reports_whether_anything_actually_changed() {
    let _r = RestoreLimits;
    BasisResourceLimitManager::set_limits(32);
    assert!(!BasisResourceLimitManager::set_limits(32));
    assert!(!BasisResourceLimitManager::set_limits(-1)); // sanitized straight back to the default
    assert!(BasisResourceLimitManager::set_limits(33));
    assert!(BasisResourceLimitManager::set_limits(32));
    assert!(!BasisResourceLimitManager::set_limits(32));
}

#[test]
#[serial(network_statics)]
fn initialize_from_config_applies_the_configured_caps() {
    let _r = RestoreLimits;
    BasisResourceLimitManager::initialize_from_config(&Configuration { max_content_spheres_per_player: 64, ..Configuration::default() });
    assert_eq!(BasisResourceLimitManager::max_content_spheres_per_player(), 64);
}

#[test]
#[serial(network_statics)]
fn send_state_to_peer_writes_mode_byte_then_the_cap() {
    let _r = RestoreLimits;
    BasisResourceLimitManager::set_limits(44);
    let peer = FakePeer::new(2);
    BasisResourceLimitManager::send_state_to_peer(&peer.as_ref());

    let sent = peer.sent.lock();
    assert_eq!(sent.len(), 1);
    let mut reader = NetDataReader::from_slice(&sent[0].data);
    assert_eq!(reader.get_byte().expect("mode"), AdminRequestMode::GlobalGetResourceLimits as u8);
    assert_eq!(reader.get_int().expect("cap"), 44);
    assert_eq!(reader.available_bytes(), 0);
    assert_eq!(sent[0].channel, BasisNetworkCommons::ADMIN_CHANNEL);
    drop(sent);

    BasisResourceLimitManager::broadcast_state(); // zero connected peers: must be a safe no-op
}

#[allow(dead_code)]
fn unused(_: ClientMetaDataMessage) {}
