//! The DID auth identity's entry ownership: an entry is released only by the connection that
//! created it, so a recycled peer id can never evict or read the connection that inherited it.

use basis_network_core::transport::basis_network_shell::peers_equal;
use basis_network_server::auth::IAuthIdentity;
use basis_network_server::security::BasisDIDAuthIdentity;
use basis_server_tests::support::LifecycleSupport as L;

#[test]
fn remove_connection_releases_the_entry_its_own_peer_created() {
    let identity = BasisDIDAuthIdentity::new();
    let id = L::next_peer_id();
    let owner = L::peer(id);
    identity.register_for_tests(id, &L::new_uuid(), owner.as_ref());

    assert!(identity.remove_connection_expected(id, &owner.as_ref()));
    assert!(identity.auth_entry(id).is_none());
    assert!(!identity.remove_connection_expected(id, &owner.as_ref()));
    identity.de_initialize();
}

#[test]
fn remove_connection_leaves_an_entry_that_belongs_to_another_connection() {
    let identity = BasisDIDAuthIdentity::new();
    let id = L::next_peer_id();
    let stale = L::peer(id);
    let live = L::peer(id);
    identity.register_for_tests(id, &L::new_uuid(), live.as_ref());

    assert!(!identity.remove_connection_expected(id, &stale.as_ref()));
    let entry = identity.auth_entry(id).expect("the live entry survives");
    assert!(peers_equal(&entry.peer, &live.as_ref()));
    identity.de_initialize();
}

#[test]
fn remove_connection_without_a_peer_removes_whichever_entry_holds_the_id() {
    let identity = BasisDIDAuthIdentity::new();
    let id = L::next_peer_id();
    identity.register_for_tests(id, &L::new_uuid(), L::peer(id).as_ref());

    identity.remove_connection(id);
    assert!(identity.auth_entry(id).is_none());
    identity.de_initialize();
}

#[test]
fn net_id_to_uuid_answers_only_for_the_connection_that_owns_the_entry() {
    let identity = BasisDIDAuthIdentity::new();
    let id = L::next_peer_id();
    let owner = L::peer(id);
    let recycled = L::peer(id);
    let uuid = L::new_uuid();
    identity.register_for_tests(id, &uuid, owner.as_ref());

    assert_eq!(identity.net_id_to_uuid(&owner.as_ref()).as_deref(), Some(uuid.as_str()));
    assert!(identity.net_id_to_uuid(&recycled.as_ref()).is_none());
    identity.de_initialize();
}

#[test]
fn a_stale_timeout_cannot_evict_the_connection_that_inherited_the_id() {
    let identity = BasisDIDAuthIdentity::new();
    let id = L::next_peer_id();
    let timed_out = L::peer(id);
    let inherited = L::peer(id);
    identity.register_for_tests(id, &L::new_uuid(), inherited.as_ref());

    assert!(!identity.remove_connection_expected(id, &timed_out.as_ref()));
    let entry = identity.auth_entry(id).expect("the inheriting entry survives");
    assert!(peers_equal(&entry.peer, &inherited.as_ref()));
    identity.de_initialize();
}
