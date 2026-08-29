//! `BasisNetworkOwnership`, `BasisSavedState` and `BasisNetworkIDDatabase`, exercised with an
//! offline stand-in peer: sends are counted, and every server broadcast in these paths iterates the
//! peer snapshot, which stays empty because no test ever starts a server. All three poke shared
//! process statics, so they serialise on one key.

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

use basis_network_core::configuration::Configuration;
use basis_network_core::serializable::{ClientAvatarChangeMessage, ClientBodyFitMessage, ClientMetaDataMessage, OwnershipTransferMessage, PlayerIdMessage, ReadyMessage, VoiceReceiversMessage};
use basis_network_core::transport::basis_network_shell::NetPeer;
use basis_network_core::{NetDataReader, NetDataWriter};
use basis_network_server::NetworkServer;
use basis_network_server::identity::basis_network_id_database::BasisNetworkIDDatabase;
use basis_network_server::networking::{BasisNetworkOwnership, BasisSavedState};
use basis_server_tests::support::FakePeer;
use serial_test::serial;

fn build_reader(player_id: u16, ownership_id: &str) -> NetDataReader {
    let mut message = OwnershipTransferMessage { player_id_message: PlayerIdMessage { player_id }, ownership_id: ownership_id.to_string() };
    let mut writer = NetDataWriter::with_capacity(64);
    message.serialize(&mut writer).expect("serialize");
    NetDataReader::new(writer.copy_data())
}

fn run_parallel(count: usize, body: impl Fn(usize) + Send + Sync + 'static) {
    let body = Arc::new(body);
    let handles: Vec<_> = (0..count)
        .map(|i| {
            let body = body.clone();
            std::thread::spawn(move || body(i))
        })
        .collect();
    for h in handles {
        h.join().expect("worker panicked");
    }
}

// ── BasisNetworkOwnership ── object keys are unique per test; tests never share keys.

#[test]
#[serial(network_statics)]
fn network_request_new_or_existing_first_requester_wins_second_sees_existing_owner() {
    let key = "own:first-wins";
    let message = OwnershipTransferMessage { ownership_id: key.to_string(), ..Default::default() };
    assert_eq!(BasisNetworkOwnership::network_request_new_or_existing(&message, 42), (true, 42));
    assert_eq!(BasisNetworkOwnership::network_request_new_or_existing(&message, 43), (false, 42));
}

#[test]
#[serial(network_statics)]
fn get_ownership_information_unknown_object_returns_none() {
    assert_eq!(BasisNetworkOwnership::get_ownership_information("own:unknown"), None);
    assert!(!BasisNetworkOwnership::does_object_exist_in_database("own:unknown"));
}

#[test]
#[serial(network_statics)]
fn add_ownership_second_add_for_same_object_is_rejected_and_keeps_first_owner() {
    let key = "own:add-twice";
    assert!(BasisNetworkOwnership::add_ownership(key, 1));
    assert!(!BasisNetworkOwnership::add_ownership(key, 2));
    assert_eq!(BasisNetworkOwnership::get_ownership_information(key), Some(1));
}

#[test]
#[serial(network_statics)]
fn switch_ownership_transfers_existing_object_to_new_owner() {
    let key = "own:switch";
    assert!(BasisNetworkOwnership::add_ownership(key, 10));
    assert!(BasisNetworkOwnership::switch_ownership(key, 20));
    assert_eq!(BasisNetworkOwnership::get_ownership_information(key), Some(20));
}

#[test]
#[serial(network_statics)]
fn switch_ownership_unknown_object_implicitly_acquires_it() {
    let key = "own:switch-unknown";
    assert!(BasisNetworkOwnership::switch_ownership(key, 33));
    assert_eq!(BasisNetworkOwnership::get_ownership_information(key), Some(33));
}

#[test]
#[serial(network_statics)]
fn remove_object_removes_existing_reports_false_for_unknown() {
    let key = "own:remove";
    assert!(!BasisNetworkOwnership::remove_object(key));
    assert!(BasisNetworkOwnership::add_ownership(key, 3));
    assert!(BasisNetworkOwnership::remove_object(key));
    assert!(!BasisNetworkOwnership::does_object_exist_in_database(key));
    assert!(!BasisNetworkOwnership::remove_object(key));
}

#[test]
#[serial(network_statics)]
fn owner_ids_ushort_edge_values_round_trip() {
    assert!(BasisNetworkOwnership::add_ownership("own:edge:zero", 0));
    assert!(BasisNetworkOwnership::add_ownership("own:edge:max", u16::MAX));
    assert_eq!(BasisNetworkOwnership::get_ownership_information("own:edge:zero"), Some(0));
    assert_eq!(BasisNetworkOwnership::get_ownership_information("own:edge:max"), Some(u16::MAX));
    assert!(BasisNetworkOwnership::switch_ownership("own:edge:max", 0));
    assert_eq!(BasisNetworkOwnership::get_ownership_information("own:edge:max"), Some(0));
}

#[test]
#[serial(network_statics)]
fn remove_player_ownership_strips_only_that_players_objects() {
    const VICTIM: u16 = 41001;
    const BYSTANDER: u16 = 41002;
    assert!(BasisNetworkOwnership::add_ownership("own:rpo:a", VICTIM));
    assert!(BasisNetworkOwnership::add_ownership("own:rpo:b", VICTIM));
    assert!(BasisNetworkOwnership::add_ownership("own:rpo:c", BYSTANDER));

    BasisNetworkOwnership::remove_player_ownership(i32::from(VICTIM));

    assert!(!BasisNetworkOwnership::does_object_exist_in_database("own:rpo:a"));
    assert!(!BasisNetworkOwnership::does_object_exist_in_database("own:rpo:b"));
    assert_eq!(BasisNetworkOwnership::get_ownership_information("own:rpo:c"), Some(BYSTANDER));
}

#[test]
#[serial(network_statics)]
fn remove_player_ownership_player_without_objects_is_a_no_op() {
    BasisNetworkOwnership::remove_player_ownership(64000);
    assert!(!BasisNetworkOwnership::does_object_exist_in_database("own:rpo:none"));
}

#[test]
#[serial(network_statics)]
fn ownership_response_wire_path_first_requester_wins_second_gets_existing_owner() {
    let key = "own:wire:response";

    let first = FakePeer::new(7);
    BasisNetworkOwnership::ownership_response(build_reader(0, key), &first.as_ref());
    assert_eq!(BasisNetworkOwnership::get_ownership_information(key), Some(7));
    assert_eq!(first.sent_count(), 1);

    let second = FakePeer::new(9);
    BasisNetworkOwnership::ownership_response(build_reader(0, key), &second.as_ref());
    assert_eq!(BasisNetworkOwnership::get_ownership_information(key), Some(7));
    assert_eq!(second.sent_count(), 1);
}

#[test]
#[serial(network_statics)]
fn ownership_transfer_wire_path_switches_existing_and_implicitly_acquires_unknown() {
    let existing = "own:wire:transfer-existing";
    assert!(BasisNetworkOwnership::add_ownership(existing, 5));

    let peer = FakePeer::new(8);
    BasisNetworkOwnership::ownership_transfer(build_reader(5, existing), &peer.as_ref());
    assert_eq!(BasisNetworkOwnership::get_ownership_information(existing), Some(8));

    let unknown = "own:wire:transfer-unknown";
    BasisNetworkOwnership::ownership_transfer(build_reader(0, unknown), &peer.as_ref());
    assert_eq!(BasisNetworkOwnership::get_ownership_information(unknown), Some(8));
}

#[test]
#[serial(network_statics)]
fn remove_ownership_wire_path_only_current_owner_can_remove() {
    let key = "own:wire:remove";
    assert!(BasisNetworkOwnership::add_ownership(key, 11));

    // A non-owner cannot release the object even by naming the real owner in the packet:
    // authorization comes from the sending peer, not the client-supplied player id.
    BasisNetworkOwnership::remove_ownership(build_reader(11, key), &FakePeer::new(12).as_ref());
    assert!(BasisNetworkOwnership::does_object_exist_in_database(key));

    let peer = FakePeer::new(11);
    BasisNetworkOwnership::remove_ownership(build_reader(11, "own:wire:remove-unknown"), &peer.as_ref());
    assert!(!BasisNetworkOwnership::does_object_exist_in_database("own:wire:remove-unknown"));

    // The owner's own request succeeds; the redundant player id field is ignored.
    BasisNetworkOwnership::remove_ownership(build_reader(12, key), &peer.as_ref());
    assert!(!BasisNetworkOwnership::does_object_exist_in_database(key));
}

/// A malformed ownership packet is a protocol error from one client; it must be dropped, not
/// allowed to panic the server or to leave a half-written record behind.
#[test]
#[serial(network_statics)]
fn truncated_ownership_packets_are_dropped_without_side_effects() {
    let peer = FakePeer::new(13);
    for reader in [NetDataReader::from_slice(&[]), NetDataReader::from_slice(&[0x01]), NetDataReader::from_slice(&[0x01, 0x00, 0x09, 0x00, 0x00, 0x00, b'o', b'w'])] {
        BasisNetworkOwnership::ownership_response(reader.clone(), &peer.as_ref());
        BasisNetworkOwnership::ownership_transfer(reader.clone(), &peer.as_ref());
        BasisNetworkOwnership::remove_ownership(reader, &peer.as_ref());
    }
    assert_eq!(peer.sent_count(), 0);
    assert!(!BasisNetworkOwnership::does_object_exist_in_database("ow"));
    assert!(!BasisNetworkOwnership::does_object_exist_in_database(""));
}

#[test]
#[serial(network_statics)]
fn concurrent_request_storm_same_object_produces_exactly_one_owner() {
    const KEY: &str = "own:storm:same-object";
    const THREADS: usize = 64;
    let winners = Arc::new(AtomicUsize::new(0));
    let winner_requester = Arc::new(AtomicI32::new(0));
    let (w, r) = (winners.clone(), winner_requester.clone());
    run_parallel(THREADS, move |i| {
        let message = OwnershipTransferMessage { ownership_id: KEY.to_string(), ..Default::default() };
        let (won, assigned) = BasisNetworkOwnership::network_request_new_or_existing(&message, (i + 1) as u16);
        if won {
            w.fetch_add(1, Ordering::Relaxed);
            r.store((i + 1) as i32, Ordering::Relaxed);
            assert_eq!(assigned, (i + 1) as u16);
        }
    });
    assert_eq!(winners.load(Ordering::Relaxed), 1);
    assert!(BasisNetworkOwnership::does_object_exist_in_database(KEY));
    assert_eq!(BasisNetworkOwnership::get_ownership_information(KEY).map(i32::from), Some(winner_requester.load(Ordering::Relaxed)));
}

#[test]
#[serial(network_statics)]
fn concurrent_switch_storm_all_succeed_final_owner_is_one_of_the_requesters() {
    const KEY: &str = "own:storm:switch";
    assert!(BasisNetworkOwnership::add_ownership(KEY, 500));
    const THREADS: usize = 32;
    let failures = Arc::new(AtomicUsize::new(0));
    let f = failures.clone();
    run_parallel(THREADS, move |i| {
        if !BasisNetworkOwnership::switch_ownership(KEY, (i + 1) as u16) {
            f.fetch_add(1, Ordering::Relaxed);
        }
    });
    assert_eq!(failures.load(Ordering::Relaxed), 0);
    let final_owner = BasisNetworkOwnership::get_ownership_information(KEY).expect("owner");
    assert!((1..=THREADS as u16).contains(&final_owner));
}

// ── BasisSavedState ── player ids are unique per test (51xxx / 52xxx) and every test removes
// the players it created.

fn avatar(load_mode: u8, bytes: &[u8], index: u8) -> ClientAvatarChangeMessage {
    ClientAvatarChangeMessage { load_mode, byte_array: Some(bytes.to_vec()), local_avatar_index: index, ..Default::default() }
}

fn fit(arm: f32, leg: f32, torso: f32) -> ClientBodyFitMessage {
    ClientBodyFitMessage { arm_scale: arm, leg_scale: leg, torso_scale: torso }
}

#[test]
#[serial(network_statics)]
fn ready_message_stores_avatar_change_and_meta_data() {
    let peer = FakePeer::new(51000);
    let ready = ReadyMessage {
        player_meta_data_message: ClientMetaDataMessage { player_uuid: "uuid-51000".into(), player_display_name: "Tester".into(), player_platform: "xunit".into() },
        client_avatar_change_message: avatar(2, &[1, 2, 3], 9),
        ..Default::default()
    };
    BasisSavedState::add_last_ready_message(&peer.as_ref(), &ready);

    let stored = BasisSavedState::get_last_avatar_change_state(&peer.as_ref()).expect("avatar");
    assert_eq!(stored.load_mode, 2);
    assert_eq!(stored.byte_array.as_deref(), Some(&[1u8, 2, 3][..]));
    assert_eq!(stored.local_avatar_index, 9);

    let meta = BasisSavedState::get_last_player_meta_data(&peer.as_ref()).expect("meta");
    assert_eq!(meta.player_uuid, "uuid-51000");
    assert_eq!(meta.player_display_name, "Tester");
    assert_eq!(meta.player_platform, "xunit");
    BasisSavedState::remove_player(peer.id());
}

#[test]
#[serial(network_statics)]
fn avatar_change_latest_write_wins() {
    let peer = FakePeer::new(51001);
    BasisSavedState::add_last_avatar_change(&peer.as_ref(), avatar(0, &[1], 1));
    BasisSavedState::add_last_avatar_change(&peer.as_ref(), avatar(1, &[7, 7], 2));

    let stored = BasisSavedState::get_last_avatar_change_state(&peer.as_ref()).expect("avatar");
    assert_eq!(stored.load_mode, 1);
    assert_eq!(stored.byte_array.as_deref(), Some(&[7u8, 7][..]));
    assert_eq!(stored.local_avatar_index, 2);
    BasisSavedState::remove_player(peer.id());
}

// Body fit: stored on the avatar record so the late-join replay carries it, but it arrives on its
// own message. A fit update must never disturb which avatar is worn, and an avatar change must
// never silently revert the wearer's proportions.

#[test]
#[serial(network_statics)]
fn body_fit_merges_into_the_avatar_record_without_disturbing_the_avatar() {
    let peer = FakePeer::new(51010);
    BasisSavedState::add_last_avatar_change(&peer.as_ref(), ClientAvatarChangeMessage { arm_scale: 1.0, leg_scale: 1.0, torso_scale: 1.0, ..avatar(1, &[4, 5, 6], 12) });
    BasisSavedState::update_body_fit(&peer.as_ref(), &fit(1.0625, 0.9375, 1.125));

    let stored = BasisSavedState::get_last_avatar_change_state(&peer.as_ref()).expect("avatar");
    assert_eq!(stored.arm_scale, 1.0625);
    assert_eq!(stored.leg_scale, 0.9375);
    assert_eq!(stored.torso_scale, 1.125);
    // The avatar itself must be untouched — a recalibration is not an avatar swap.
    assert_eq!(stored.load_mode, 1);
    assert_eq!(stored.byte_array.as_deref(), Some(&[4u8, 5, 6][..]));
    assert_eq!(stored.local_avatar_index, 12);
    BasisSavedState::remove_player(peer.id());
}

/// A fit can land before any avatar change (recalibrating while the avatar is still loading). It
/// has to be held, not dropped, or that player renders unfitted until they recalibrate again.
#[test]
#[serial(network_statics)]
fn body_fit_arriving_before_any_avatar_is_held_on_a_placeholder() {
    let peer = FakePeer::new(51011);
    BasisSavedState::update_body_fit(&peer.as_ref(), &fit(1.05, 0.95, 1.02));

    let stored = BasisSavedState::get_last_avatar_change_state(&peer.as_ref()).expect("avatar");
    assert!(stored.byte_array.is_none());
    assert_eq!(stored.arm_scale, 1.05);
    assert_eq!(stored.leg_scale, 0.95);
    assert_eq!(stored.torso_scale, 1.02);
    BasisSavedState::remove_player(peer.id());
}

#[test]
#[serial(network_statics)]
fn body_fit_latest_write_wins() {
    let peer = FakePeer::new(51012);
    BasisSavedState::update_body_fit(&peer.as_ref(), &fit(1.1, 1.1, 1.1));
    BasisSavedState::update_body_fit(&peer.as_ref(), &fit(0.9, 0.8, 1.2));

    let stored = BasisSavedState::get_last_avatar_change_state(&peer.as_ref()).expect("avatar");
    assert_eq!((stored.arm_scale, stored.leg_scale, stored.torso_scale), (0.9, 0.8, 1.2));
    BasisSavedState::remove_player(peer.id());
}

/// An avatar change carries its own fit, so it legitimately replaces the stored one.
#[test]
#[serial(network_statics)]
fn avatar_change_after_body_fit_carries_its_own_fit() {
    let peer = FakePeer::new(51013);
    BasisSavedState::update_body_fit(&peer.as_ref(), &fit(1.1, 1.1, 1.1));
    BasisSavedState::add_last_avatar_change(&peer.as_ref(), ClientAvatarChangeMessage { arm_scale: 1.03, leg_scale: 1.04, torso_scale: 1.05, ..avatar(0, &[9], 3) });

    let stored = BasisSavedState::get_last_avatar_change_state(&peer.as_ref()).expect("avatar");
    assert_eq!(stored.byte_array.as_deref(), Some(&[9u8][..]));
    assert_eq!((stored.arm_scale, stored.leg_scale, stored.torso_scale), (1.03, 1.04, 1.05));
    BasisSavedState::remove_player(peer.id());
}

#[test]
#[serial(network_statics)]
fn body_fit_is_cleared_with_the_player() {
    let peer = FakePeer::new(51014);
    BasisSavedState::update_body_fit(&peer.as_ref(), &fit(1.1, 1.1, 1.1));
    BasisSavedState::remove_player(peer.id());
    assert!(BasisSavedState::get_last_avatar_change_state(&peer.as_ref()).is_none());
}

#[test]
#[serial(network_statics)]
fn unknown_player_every_getter_returns_safe_default() {
    let stranger = FakePeer::new(51999);
    assert!(BasisSavedState::get_last_avatar_change_state(&stranger.as_ref()).is_none());
    assert!(BasisSavedState::get_last_player_meta_data(&stranger.as_ref()).is_none());
    assert!(BasisSavedState::get_resolved_voice_peers(&stranger.as_ref()).is_none());
    assert!(!BasisSavedState::is_in_shout_mode(stranger.id()));
    assert!(!BasisSavedState::get_all_shout_mode_players().contains(&stranger.id()));
}

#[test]
#[serial(network_statics)]
fn remove_player_clears_every_stored_state_for_that_player() {
    let peer = FakePeer::new(51002);
    let ready = ReadyMessage {
        player_meta_data_message: ClientMetaDataMessage { player_uuid: "u".into(), player_display_name: "n".into(), player_platform: "p".into() },
        client_avatar_change_message: avatar(1, &[5], 1),
        ..Default::default()
    };
    BasisSavedState::add_last_ready_message(&peer.as_ref(), &ready);
    BasisSavedState::get_or_create_resolved_list(peer.id());
    BasisSavedState::set_shout_mode(peer.id(), true);

    BasisSavedState::remove_player(peer.id());

    assert!(BasisSavedState::get_last_avatar_change_state(&peer.as_ref()).is_none());
    assert!(BasisSavedState::get_last_player_meta_data(&peer.as_ref()).is_none());
    assert!(BasisSavedState::get_resolved_voice_peers(&peer.as_ref()).is_none());
    assert!(!BasisSavedState::is_in_shout_mode(peer.id()));
}

#[test]
#[serial(network_statics)]
fn remove_player_purges_the_disconnected_peer_from_other_players_voice_lists() {
    let host = FakePeer::new(51003);
    let leaving = FakePeer::new(51004);
    let staying = FakePeer::new(51005);
    let list = BasisSavedState::get_or_create_resolved_list(host.id());
    {
        let mut guard = list.lock();
        guard.push(leaving.as_ref());
        guard.push(staying.as_ref());
    }

    BasisSavedState::remove_player(leaving.id());

    let after = BasisSavedState::get_resolved_voice_peers(&host.as_ref()).expect("resolved");
    assert!(Arc::ptr_eq(&list, &after));
    let after = after.lock();
    assert!(!after.iter().any(|p| p.id() == leaving.id()));
    assert!(after.iter().any(|p| p.id() == staying.id()));
    assert_eq!(after.len(), 1);
    drop(after);
    BasisSavedState::remove_player(host.id());
    BasisSavedState::remove_player(staying.id());
}

#[test]
#[serial(network_statics)]
fn voice_receivers_resolve_against_authenticated_peers_empty_clears_none_keeps() {
    let host = FakePeer::new(51010);
    let target = FakePeer::new(51011);
    NetworkServer::authenticated_peers().insert(target.id(), target.as_ref());

    let mut users = VoiceReceiversMessage { users: Some(vec![target.id() as u16, 51012]), users_length: 2 };
    BasisSavedState::add_last_voice_receivers(&host.as_ref(), &mut users);
    let resolved = BasisSavedState::get_resolved_voice_peers(&host.as_ref()).expect("resolved");
    {
        let guard = resolved.lock();
        assert_eq!(guard.len(), 1);
        assert_eq!(guard[0].id(), target.id());
    }

    let mut none = VoiceReceiversMessage { users: None, users_length: 0 };
    BasisSavedState::add_last_voice_receivers(&host.as_ref(), &mut none);
    assert_eq!(BasisSavedState::get_resolved_voice_peers(&host.as_ref()).expect("resolved").lock().len(), 1);

    let mut empty = VoiceReceiversMessage { users: Some(Vec::new()), users_length: 0 };
    BasisSavedState::add_last_voice_receivers(&host.as_ref(), &mut empty);
    assert!(BasisSavedState::get_resolved_voice_peers(&host.as_ref()).expect("resolved").lock().is_empty());

    NetworkServer::authenticated_peers().remove(&target.id());
    BasisSavedState::remove_player(host.id());
}

#[test]
#[serial(network_statics)]
fn get_or_create_resolved_list_is_stable_per_player_until_remove_player() {
    const ID: i32 = 51007;
    let first = BasisSavedState::get_or_create_resolved_list(ID);
    let second = BasisSavedState::get_or_create_resolved_list(ID);
    assert!(Arc::ptr_eq(&first, &second));

    BasisSavedState::remove_player(ID);
    let third = BasisSavedState::get_or_create_resolved_list(ID);
    assert!(!Arc::ptr_eq(&first, &third));
    BasisSavedState::remove_player(ID);
}

#[test]
#[serial(network_statics)]
fn shout_mode_set_query_and_enumerate() {
    const ID: i32 = 51008;
    assert!(!BasisSavedState::is_in_shout_mode(ID));

    BasisSavedState::set_shout_mode(ID, true);
    assert!(BasisSavedState::is_in_shout_mode(ID));
    assert!(BasisSavedState::get_all_shout_mode_players().contains(&ID));

    BasisSavedState::set_shout_mode(ID, true);
    assert!(BasisSavedState::is_in_shout_mode(ID));

    BasisSavedState::set_shout_mode(ID, false);
    assert!(!BasisSavedState::is_in_shout_mode(ID));
    assert!(!BasisSavedState::get_all_shout_mode_players().contains(&ID));
}

#[test]
#[serial(network_statics)]
fn concurrent_store_and_read_keeps_every_players_state_intact() {
    const BASE_ID: i32 = 52000;
    const PLAYERS: usize = 128;
    run_parallel(PLAYERS, |i| {
        let id = BASE_ID + i as i32;
        let peer = FakePeer::new(id);
        BasisSavedState::add_last_avatar_change(&peer.as_ref(), avatar(1, &[i as u8], i as u8));
        BasisSavedState::set_shout_mode(id, i & 1 == 0);
        let stored = BasisSavedState::get_last_avatar_change_state(&peer.as_ref()).expect("avatar");
        assert_eq!(stored.local_avatar_index, i as u8);
    });
    for i in 0..PLAYERS {
        let peer = FakePeer::new(BASE_ID + i as i32);
        let stored = BasisSavedState::get_last_avatar_change_state(&peer.as_ref()).expect("avatar");
        assert_eq!(stored.byte_array.as_deref().and_then(|b| b.first().copied()), Some(i as u8));
        assert_eq!(BasisSavedState::is_in_shout_mode(BASE_ID + i as i32), i & 1 == 0);
    }
    for i in 0..PLAYERS {
        BasisSavedState::remove_player(BASE_ID + i as i32);
    }
}

// ── BasisNetworkIDDatabase ── every test starts with reset() so counter assertions are
// deterministic.

fn stored(id: &str) -> Option<u16> {
    BasisNetworkIDDatabase::ushort_network_database().get(id).map(|v| *v)
}

#[test]
#[serial(network_statics)]
fn add_or_find_assigns_sequential_ids_starting_at_zero() {
    BasisNetworkIDDatabase::reset();
    let peer = FakePeer::new(2);
    BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:a").expect("add");
    BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:b").expect("add");
    assert_eq!(stored("net:a"), Some(0));
    assert_eq!(stored("net:b"), Some(1));
    assert_eq!(peer.sent_count(), 0);
}

#[test]
#[serial(network_statics)]
fn add_or_find_duplicate_string_id_keeps_mapping_and_does_not_burn_an_id() {
    BasisNetworkIDDatabase::reset();
    let peer = FakePeer::new(3);
    BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:dup").expect("add");
    BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:dup").expect("find");

    assert_eq!(peer.sent_count(), 1);
    assert_eq!(stored("net:dup"), Some(0));
    assert_eq!(BasisNetworkIDDatabase::ushort_network_database().len(), 1);

    BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:after-dup").expect("add");
    assert_eq!(stored("net:after-dup"), Some(1));
}

#[test]
#[serial(network_statics)]
fn unknown_id_is_absent_and_get_all_on_empty_returns_none() {
    BasisNetworkIDDatabase::reset();
    assert_eq!(stored("net:never-added"), None);
    assert!(BasisNetworkIDDatabase::get_all_network_id().is_none());
}

#[test]
#[serial(network_statics)]
fn get_all_network_id_returns_every_stored_mapping() {
    BasisNetworkIDDatabase::reset();
    let peer = FakePeer::new(4);
    for id in ["net:all:x", "net:all:y", "net:all:z"] {
        BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), id).expect("add");
    }
    let messages = BasisNetworkIDDatabase::get_all_network_id().expect("messages");
    assert_eq!(messages.len(), 3);
    for message in &messages {
        assert_eq!(stored(&message.net_id_message.player_id), Some(message.ushort_unique_id_message.unique_id_ushort));
    }
}

#[test]
#[serial(network_statics)]
fn remove_ushort_network_id_removes_only_that_mapping_and_never_reuses_ids() {
    BasisNetworkIDDatabase::reset();
    let peer = FakePeer::new(5);
    BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:rm:a").expect("add");
    BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:rm:b").expect("add");

    BasisNetworkIDDatabase::remove_ushort_network_id(0);
    assert_eq!(stored("net:rm:a"), None);
    assert_eq!(stored("net:rm:b"), Some(1));

    BasisNetworkIDDatabase::remove_ushort_network_id(12345);
    assert_eq!(BasisNetworkIDDatabase::ushort_network_database().len(), 1);

    BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:rm:a").expect("add");
    assert_eq!(stored("net:rm:a"), Some(2));
}

#[test]
#[serial(network_statics)]
fn many_sequential_adds_produce_unique_contiguous_ids() {
    BasisNetworkIDDatabase::reset();
    let peer = FakePeer::new(6);
    const COUNT: usize = 500;
    for i in 0..COUNT {
        BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), &format!("net:many:{i}")).expect("add");
    }
    assert_eq!(BasisNetworkIDDatabase::ushort_network_database().len(), COUNT);
    let mut ids: Vec<u16> = BasisNetworkIDDatabase::ushort_network_database().iter().map(|e| *e.value()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), COUNT);
    assert_eq!(ids[0], 0);
    assert_eq!(ids[COUNT - 1], (COUNT - 1) as u16);
}

#[test]
#[serial(network_statics)]
fn concurrent_add_storm_distinct_string_ids_no_id_collisions() {
    BasisNetworkIDDatabase::reset();
    let peer = FakePeer::new(7);
    const COUNT: usize = 256;
    let peer_ref = peer.as_ref();
    run_parallel(COUNT, move |i| {
        BasisNetworkIDDatabase::add_or_find_network_id(&peer_ref, &format!("net:storm:{i}")).expect("add");
    });
    assert_eq!(BasisNetworkIDDatabase::ushort_network_database().len(), COUNT);
    let mut ids: Vec<u16> = BasisNetworkIDDatabase::ushort_network_database().iter().map(|e| *e.value()).collect();
    ids.sort_unstable();
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(*id, i as u16);
    }
    assert_eq!(peer.sent_count(), 0);
}

#[test]
#[serial(network_statics)]
fn reset_clears_mappings_and_restarts_counter() {
    BasisNetworkIDDatabase::reset();
    let peer = FakePeer::new(8);
    BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:reset:a").expect("add");
    BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:reset:b").expect("add");

    BasisNetworkIDDatabase::reset();

    assert!(BasisNetworkIDDatabase::ushort_network_database().is_empty());
    BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:reset:c").expect("add");
    assert_eq!(stored("net:reset:c"), Some(0));
}

struct ConfigGuard(Option<Arc<Configuration>>);

impl ConfigGuard {
    fn install(config: Configuration) -> Self {
        let previous = NetworkServer::configuration();
        NetworkServer::set_configuration(config);
        Self(previous)
    }
}

impl Drop for ConfigGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(previous) => NetworkServer::set_configuration((*previous).clone()),
            None => NetworkServer::clear_configuration(),
        }
    }
}

/// The shared ushort space (65,536 ids across the whole instance) driven to exhaustion by one
/// peer. At the ceiling requests fail as a `Limit` error and are dropped, never panicked: ids
/// arrive per client message, and a panic per message would be a fault storm through the
/// message processor.
#[test]
#[serial(network_statics)]
fn counter_exhaustion_drops_at_ushort_limit_and_stays_full_until_reset() {
    BasisNetworkIDDatabase::reset();
    let _guard = ConfigGuard::install(Configuration { max_network_ids_per_player: i32::from(u16::MAX) + 10, ..Configuration::default() });

    let peer = FakePeer::new(9);
    for i in 0..=u32::from(u16::MAX) {
        BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), &format!("net:cap:{i}")).expect("add");
    }
    assert_eq!(stored(&format!("net:cap:{}", u16::MAX)), Some(u16::MAX));

    let overflow = BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:cap:overflow");
    assert!(overflow.is_err(), "the id space is full; the request must be refused");
    assert!(BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:cap:overflow-2").is_err());
    assert_eq!(stored("net:cap:overflow"), None);
    assert_eq!(BasisNetworkIDDatabase::ushort_network_database().len(), usize::from(u16::MAX) + 1);

    BasisNetworkIDDatabase::reset();
    BasisNetworkIDDatabase::add_or_find_network_id(&peer.as_ref(), "net:cap:post-reset").expect("add");
    assert_eq!(stored("net:cap:post-reset"), Some(0));
    BasisNetworkIDDatabase::reset();
}

#[test]
#[serial(network_statics)]
fn per_peer_id_cap_limits_one_client_but_not_others_or_existing_ids() {
    BasisNetworkIDDatabase::reset();
    let _guard = ConfigGuard::install(Configuration { max_network_ids_per_player: 4, ..Configuration::default() });

    let greedy = FakePeer::new(20);
    let mut refused = 0;
    for i in 0..20 {
        if BasisNetworkIDDatabase::add_or_find_network_id(&greedy.as_ref(), &format!("net:greedy:{i}")).is_err() {
            refused += 1;
        }
    }
    // Capped at its allowance no matter how many distinct ids it asks for — one client can no
    // longer consume the shared id space and lock everyone else out.
    assert_eq!(BasisNetworkIDDatabase::ushort_network_database().len(), 4);
    assert_eq!(refused, 16);

    // A different peer still gets ids; the greedy peer's exhausted allowance is its own.
    let other = FakePeer::new(21);
    BasisNetworkIDDatabase::add_or_find_network_id(&other.as_ref(), "net:other:0").expect("add");
    assert!(stored("net:other:0").is_some());
    assert_eq!(BasisNetworkIDDatabase::ushort_network_database().len(), 5);

    // Looking up an id it already owns is not a new assignment and is never blocked by the cap.
    BasisNetworkIDDatabase::add_or_find_network_id(&greedy.as_ref(), "net:greedy:0").expect("find");
    assert_eq!(BasisNetworkIDDatabase::ushort_network_database().len(), 5);

    // The count is per session: it clears on disconnect so a rejoin starts fresh.
    BasisNetworkIDDatabase::remove_peer(greedy.id());
    BasisNetworkIDDatabase::add_or_find_network_id(&greedy.as_ref(), "net:greedy:after-rejoin").expect("add");
    assert!(stored("net:greedy:after-rejoin").is_some());
    BasisNetworkIDDatabase::reset();
}
