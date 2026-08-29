//! `BasisServerP2PBroker`'s offload lifecycle — the server-side gate that makes the server STOP
//! relaying voice + avatar between a direct-connected (P2P) pair once both report LinkUp. This is
//! the one voice suppressor with no client-side self-heal, so if it fails to clear when a peer
//! disconnects, the pair goes silent until the server restarts.
//!
//! Peer ids are reused on reconnect, so a rejoiner usually returns with the SAME id. These pin that
//! a stale offload entry never survives to collide with the reused id.

use basis_network_server::p2p::BasisServerP2PBroker as B;
use serial_test::serial;

fn reset() {
    B::reset_for_tests();
}

/// Establish a fully-offloaded pair, as the broker would after Request/Accept and both sides
/// reporting LinkUp.
fn offload(a: i32, b: i32, token: Option<&str>) -> String {
    let token = token.map(str::to_string).unwrap_or_else(|| format!("tok-{a}-{b}"));
    B::register_session_for_tests(&token, a, b);
    B::apply_link_up(a, &token);
    B::apply_link_up(b, &token);
    token
}

#[test]
#[serial(network_statics)]
fn offload_requires_both_sides() {
    reset();
    const TOK: &str = "t";
    B::register_session_for_tests(TOK, 3, 5);
    assert!(!B::is_p2p_offloaded(3, 5));

    B::apply_link_up(3, TOK);
    assert!(!B::is_p2p_offloaded(3, 5)); // only one side up

    B::apply_link_up(5, TOK);
    assert!(B::is_p2p_offloaded(3, 5)); // both up -> offloaded
}

#[test]
#[serial(network_statics)]
fn offload_is_symmetric() {
    reset();
    offload(3, 5, None);
    assert!(B::is_p2p_offloaded(3, 5));
    assert!(B::is_p2p_offloaded(5, 3));
}

#[test]
#[serial(network_statics)]
fn same_peer_id_is_never_offloaded() {
    reset();
    assert!(!B::is_p2p_offloaded(5, 5));
}

// --- The core rejoin path ---

#[test]
#[serial(network_statics)]
fn peer_disconnect_clears_offload_and_session() {
    reset();
    offload(3, 5, None);
    assert!(B::is_p2p_offloaded(3, 5));

    // Peer 5 leaves the server (cleanup_peer_subsystems -> remove_peer).
    B::remove_peer(5);

    // If this stays true the server keeps skipping the voice relay between the pair.
    assert!(!B::is_p2p_offloaded(3, 5));
    assert!(!B::has_session_for_tests("tok-3-5"));
}

#[test]
#[serial(network_statics)]
fn initiator_disconnect_clears_offload() {
    reset();
    offload(3, 5, None);
    B::remove_peer(3); // the side that sent the original Request leaves
    assert!(!B::is_p2p_offloaded(3, 5));
}

#[test]
#[serial(network_statics)]
fn rejoin_with_reused_id_is_not_still_offloaded() {
    reset();
    offload(3, 5, None);
    B::remove_peer(5);
    // B rejoins and gets the same id 5 back. Direct connect does NOT auto re-establish, so there
    // is no session for the pair — voice must relay through the server.
    assert!(!B::is_p2p_offloaded(3, 5));
}

#[test]
#[serial(network_statics)]
fn re_establish_after_rejoin_offloads_again() {
    reset();
    offload(3, 5, None);
    B::remove_peer(5);
    assert!(!B::is_p2p_offloaded(3, 5));

    // They manually re-establish the direct connection after the rejoin (fresh token).
    offload(3, 5, Some("t2"));
    assert!(B::is_p2p_offloaded(3, 5));
}

#[test]
#[serial(network_statics)]
fn reused_id_with_different_partner_has_no_stale_suppression() {
    reset();
    offload(3, 5, None);
    B::remove_peer(5);

    // A different player joins, reuses id 5, and direct-connects to peer 7.
    offload(5, 7, Some("t2"));

    assert!(B::is_p2p_offloaded(5, 7));
    assert!(!B::is_p2p_offloaded(3, 5)); // old pair not resurrected
}

// --- Ordering / race edges ---

#[test]
#[serial(network_statics)]
fn link_up_after_disconnect_does_not_reoffload() {
    reset();
    // A LinkUp that was already in flight when the peer dropped must not re-create the offload
    // after the session was torn down.
    const TOK: &str = "t";
    B::register_session_for_tests(TOK, 3, 5);
    B::apply_link_up(3, TOK); // first side up
    B::remove_peer(5); // peer 5 drops mid-handshake
    B::apply_link_up(5, TOK); // stale, late LinkUp

    assert!(!B::is_p2p_offloaded(3, 5));
}

#[test]
#[serial(network_statics)]
fn link_lost_clears_offload_but_re_arms_session() {
    reset();
    let tok = offload(3, 5, None);
    // The starved side reports the link died (client watchdog / on_p2p_peer_disconnected).
    B::apply_link_lost(3, &tok, 5);

    assert!(!B::is_p2p_offloaded(3, 5)); // relay resumes immediately
    assert!(B::has_session_for_tests(&tok)); // session kept, re-armed
}

#[test]
#[serial(network_statics)]
fn double_disconnect_is_idempotent() {
    reset();
    offload(3, 5, None);
    B::remove_peer(5);
    B::remove_peer(5); // second disconnect for the same id
    assert!(!B::is_p2p_offloaded(3, 5));
}

#[test]
#[serial(network_statics)]
fn remove_unknown_peer_is_no_op() {
    reset();
    B::remove_peer(99); // no sessions at all
    assert!(!B::is_p2p_offloaded(3, 99));
}

// --- Isolation between pairs ---

#[test]
#[serial(network_statics)]
fn disconnecting_one_pair_leaves_other_pairs_offloaded() {
    reset();
    offload(3, 5, Some("a"));
    offload(10, 11, Some("b"));

    B::remove_peer(5);

    assert!(!B::is_p2p_offloaded(3, 5));
    assert!(B::is_p2p_offloaded(10, 11)); // unrelated pair untouched
}

#[test]
#[serial(network_statics)]
fn peer_in_two_direct_connections_disconnect_clears_both() {
    reset();
    // Peer 5 is direct-connected to both 3 and 7 at once.
    offload(3, 5, Some("a"));
    offload(5, 7, Some("b"));
    assert!(B::is_p2p_offloaded(3, 5));
    assert!(B::is_p2p_offloaded(5, 7));

    B::remove_peer(5);

    assert!(!B::is_p2p_offloaded(3, 5));
    assert!(!B::is_p2p_offloaded(5, 7));
}
