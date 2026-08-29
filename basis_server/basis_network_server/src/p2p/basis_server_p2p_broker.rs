//! Port of `P2P/BasisServerP2PBroker.cs`: brokers direct peer-to-peer sessions between two
//! clients and tracks which pairs the relay may skip.
//!
//! Signalling (Request/Accept/Decline/Cancel/LinkLost/LinkUp) is transport-neutral. The NAT
//! introduction step differs: LiteNetLib punched through its NAT module, while iroh peers each
//! send an `IntroduceRequest` carrying their serialized `EndpointAddr`; once both halves are in,
//! the broker hands each side the other's address (`Introduce`) and the initiator dials.

use std::collections::HashSet;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};

use basis_network_core::SerializableBasis::{BasisP2PIntroduce, BasisP2PIntroduceRequest, BasisP2PSignalMessage};
use basis_network_core::p2p::PeerIntroduction;
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetPacketReader, NetPeerRef};
use dashmap::DashMap;
use parking_lot::Mutex;

use crate::NetworkServer;
use crate::security::{BasisGlobalLockManager, PermNodes, PermissionIntegration};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SessionState {
    Awaiting,
    ReadyForPunch,
    Punched,
}

#[derive(Debug)]
pub struct Session {
    pub token: String,
    pub initiator_peer_id: i32,
    pub target_peer_id: i32,
    pub state: SessionState,
    /// Arrival-ordered introduction halves; the introduce step is symmetric.
    pub endpoint_a: Option<PeerIntroduction>,
    pub endpoint_b: Option<PeerIntroduction>,
    /// Which peer supplied which half, so the initiator can be told to dial.
    pub endpoint_a_peer: i32,
    pub endpoint_b_peer: i32,
    pub initiator_link_up: bool,
    pub target_link_up: bool,
}

static SESSIONS: LazyLock<DashMap<String, Arc<Mutex<Session>>>> = LazyLock::new(DashMap::new);
static PEER_SESSIONS: LazyLock<DashMap<i32, HashSet<String>>> = LazyLock::new(DashMap::new);
static OFFLOADED_PAIRS: LazyLock<DashMap<i64, ()>> = LazyLock::new(DashMap::new);
static OFFLOADED_PAIR_COUNT: AtomicI64 = AtomicI64::new(0);
static INITIALIZED_MANAGER: Mutex<Option<u64>> = Mutex::new(None);

pub struct BasisServerP2PBroker;

impl BasisServerP2PBroker {
    /// A direct link needs one session per peer you punch to, so a real mesh is bounded by the
    /// instance population. This ceiling only exists to stop one client opening unbounded
    /// sessions as a memory/flood vector.
    const MAX_SESSIONS_PER_PEER: usize = 4096;

    fn pack_pair(a: i32, b: i32) -> i64 {
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        (i64::from(lo) << 32) | i64::from(hi as u32)
    }

    /// False when no pair is offloaded at all, which is the overwhelmingly common case: the
    /// avatar send loop tests this before `is_p2p_offloaded` so a server with no direct sessions
    /// pays a load per pair instead of a map lookup.
    pub fn has_offloaded_pairs() -> bool {
        OFFLOADED_PAIR_COUNT.load(Ordering::Relaxed) != 0
    }

    pub fn is_p2p_offloaded(a: i32, b: i32) -> bool {
        if a == b || OFFLOADED_PAIR_COUNT.load(Ordering::Relaxed) == 0 {
            return false;
        }
        OFFLOADED_PAIRS.contains_key(&Self::pack_pair(a, b))
    }

    /// Re-arms the broker for the current transport. Keyed on the transport identity so a
    /// restart resets sessions (peer ids are reissued to entirely different players) while a
    /// repeat call on the same transport is a no-op.
    pub fn initialize() {
        let Some(server) = NetworkServer::server() else {
            BNL::log_error("[P2P] NetManager not initialised, cannot start P2P broker.");
            return;
        };
        let identity = Arc::as_ptr(&server) as *const () as u64;
        let mut current = INITIALIZED_MANAGER.lock();
        if *current == Some(identity) {
            return;
        }
        if current.is_some() {
            Self::reset_sessions();
        }
        *current = Some(identity);
        BNL::log("[P2P] Broker initialised.");
    }

    fn reset_sessions() {
        SESSIONS.clear();
        PEER_SESSIONS.clear();
        OFFLOADED_PAIRS.clear();
        OFFLOADED_PAIR_COUNT.store(0, Ordering::Relaxed);
    }

    pub fn handle_p2p_message(mut reader: NetPacketReader, peer: &NetPeerRef) {
        let Ok(sub) = reader.get_byte() else {
            return;
        };
        if sub == BasisNetworkCommons::P2P_SUB_INTRODUCE_REQUEST {
            let mut request = BasisP2PIntroduceRequest::default();
            if request.deserialize(&mut reader).is_err() {
                BNL::log_error(format!("[P2P] Malformed introduce request from peer {}.", peer.id()));
                return;
            }
            Self::on_introduce_request(peer.id(), &request.session_token, request.endpoint_addr);
            return;
        }
        let mut msg = BasisP2PSignalMessage::default();
        if msg.deserialize(&mut reader).is_err() {
            BNL::log_error(format!("[P2P] Malformed signal message (sub {sub}) from peer {}.", peer.id()));
            return;
        }
        match sub {
            BasisNetworkCommons::P2P_SUB_REQUEST => Self::handle_request(peer, &msg),
            BasisNetworkCommons::P2P_SUB_ACCEPT => Self::handle_accept(peer, &msg),
            BasisNetworkCommons::P2P_SUB_DECLINE => Self::forward_and_drop(peer, &msg, BasisNetworkCommons::P2P_SUB_DECLINE),
            BasisNetworkCommons::P2P_SUB_CANCEL => Self::forward_and_drop(peer, &msg, BasisNetworkCommons::P2P_SUB_CANCEL),
            BasisNetworkCommons::P2P_SUB_LINK_LOST => Self::apply_link_lost(peer.id(), &msg.session_token, i32::from(msg.other_player_id)),
            BasisNetworkCommons::P2P_SUB_LINK_UP => Self::apply_link_up(peer.id(), &msg.session_token),
            _ => BNL::log_error(format!("[P2P] Unknown sub-type {sub} from peer {}.", peer.id())),
        }
    }

    fn session(token: &str) -> Option<Arc<Mutex<Session>>> {
        SESSIONS.get(token).map(|s| s.clone())
    }

    fn peer(id: i32) -> Option<NetPeerRef> {
        NetworkServer::authenticated_peers().get(&id).map(|p| p.value().clone())
    }

    /// Core LinkUp handling, keyed by peer id. Exposed so the offload lifecycle can be
    /// exercised without live peers.
    pub fn apply_link_up(sender_id: i32, session_token: &str) {
        let Some(session) = Self::session(session_token) else {
            return;
        };
        let (initiator, target, both_up, token) = {
            let mut s = session.lock();
            if sender_id == s.initiator_peer_id {
                s.initiator_link_up = true;
            } else if sender_id == s.target_peer_id {
                s.target_link_up = true;
            } else {
                return;
            }
            BNL::log(format!(
                "[P2P] LinkUp from peer {sender_id} (token {}); flags InitiatorUp={} TargetUp={}.",
                Self::preview(&s.token),
                s.initiator_link_up,
                s.target_link_up
            ));
            (s.initiator_peer_id, s.target_peer_id, s.initiator_link_up && s.target_link_up, s.token.clone())
        };
        if !both_up {
            return;
        }
        if OFFLOADED_PAIRS.insert(Self::pack_pair(initiator, target), ()).is_none() {
            OFFLOADED_PAIR_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        BNL::log(format!("[P2P] OFFLOADED pair ({initiator},{target}) — server will skip relaying voice + avatar between them."));
        // Positive confirmation to BOTH peers that the pair is fully direct now. A client that
        // reached Connected but never sees this treats its link as partial and falls back to the
        // relay.
        if let Some(initiator_peer) = Self::peer(initiator) {
            Self::send_sub(&initiator_peer, BasisNetworkCommons::P2P_SUB_OFFLOADED, &token, target as u16, None);
        }
        if let Some(target_peer) = Self::peer(target) {
            Self::send_sub(&target_peer, BasisNetworkCommons::P2P_SUB_OFFLOADED, &token, initiator as u16, None);
        }
    }

    fn handle_request(sender: &NetPeerRef, msg: &BasisP2PSignalMessage) {
        if msg.session_token.is_empty() {
            BNL::log_error(format!("[P2P] Empty session token from peer {}, dropping Request.", sender.id()));
            return;
        }
        // Admin-controlled instance lockout: non-admins may not establish direct connections.
        if BasisGlobalLockManager::direct_connect_locked() && !PermissionIntegration::has_valid_requirement(sender, PermNodes::MODERATION_GLOBAL_LOCK) {
            BNL::log(format!("[P2P] DirectConnectLocked: rejecting Request from non-admin peer {}.", sender.id()));
            Self::send_sub(sender, BasisNetworkCommons::P2P_SUB_CANCEL, &msg.session_token, msg.other_player_id, None);
            return;
        }
        let other_id = i32::from(msg.other_player_id);
        if other_id == sender.id() {
            BNL::log_error(format!("[P2P] Peer {} tried to request a session with itself.", sender.id()));
            return;
        }
        let Some(target) = Self::peer(other_id) else {
            Self::send_sub(sender, BasisNetworkCommons::P2P_SUB_CANCEL, &msg.session_token, msg.other_player_id, None);
            return;
        };
        // Reuse of an existing token by the same peer just refreshes that session; only a
        // genuinely new token grows the set, so cap on distinct outstanding sessions per peer.
        let over_cap = PEER_SESSIONS
            .get(&sender.id())
            .is_some_and(|open| open.len() >= Self::MAX_SESSIONS_PER_PEER && !open.contains(&msg.session_token));
        if over_cap {
            BNL::log_error(format!("[P2P] Peer {} exceeded the per-peer session cap ({}); dropping Request.", sender.id(), Self::MAX_SESSIONS_PER_PEER));
            Self::send_sub(sender, BasisNetworkCommons::P2P_SUB_CANCEL, &msg.session_token, msg.other_player_id, None);
            return;
        }
        let session = Session {
            token: msg.session_token.clone(),
            initiator_peer_id: sender.id(),
            target_peer_id: other_id,
            state: SessionState::Awaiting,
            endpoint_a: None,
            endpoint_b: None,
            endpoint_a_peer: 0,
            endpoint_b_peer: 0,
            initiator_link_up: false,
            target_link_up: false,
        };
        SESSIONS.insert(msg.session_token.clone(), Arc::new(Mutex::new(session)));
        Self::track_peer_session(sender.id(), &msg.session_token);
        Self::track_peer_session(other_id, &msg.session_token);

        BNL::log(format!("[P2P] Forwarding Request from peer {} to peer {other_id} (token {}).", sender.id(), Self::preview(&msg.session_token)));
        Self::send_sub(&target, BasisNetworkCommons::P2P_SUB_REQUEST, &msg.session_token, sender.id() as u16, msg.ephemeral_public_key.clone());
        // ServerArmed confirms registration before either side starts punching, avoiding a race.
        Self::send_sub(sender, BasisNetworkCommons::P2P_SUB_SERVER_ARMED, &msg.session_token, msg.other_player_id, None);
    }

    fn handle_accept(sender: &NetPeerRef, msg: &BasisP2PSignalMessage) {
        let Some(session) = Self::session(&msg.session_token) else {
            BNL::log_error(format!("[P2P] Accept for unknown token from peer {}.", sender.id()));
            return;
        };
        let (initiator, token) = {
            let mut s = session.lock();
            if s.target_peer_id != sender.id() || s.initiator_peer_id != i32::from(msg.other_player_id) {
                BNL::log_error(format!(
                    "[P2P] Accept from peer {} doesn't match session pair ({},{}).",
                    sender.id(),
                    s.initiator_peer_id,
                    s.target_peer_id
                ));
                return;
            }
            s.state = SessionState::ReadyForPunch;
            (s.initiator_peer_id, s.token.clone())
        };
        match Self::peer(initiator) {
            Some(initiator_peer) => {
                BNL::log(format!("[P2P] Accept from peer {} (token {}); session armed, forwarding to initiator {initiator}.", sender.id(), Self::preview(&token)));
                Self::send_sub(&initiator_peer, BasisNetworkCommons::P2P_SUB_ACCEPT, &token, sender.id() as u16, msg.ephemeral_public_key.clone());
            }
            None => {
                BNL::log_warning(format!("[P2P] Accept arrived but initiator {initiator} already gone; dropping session {}.", Self::preview(&token)));
                Self::remove_session(&token);
            }
        }
    }

    /// Core LinkLost handling, keyed by peer id. Re-arms the session + clears the offload so the
    /// relay resumes during the re-punch window, then forwards LinkLost to the other peer.
    pub fn apply_link_lost(sender_id: i32, session_token: &str, other_player_id: i32) {
        if let Some(session) = Self::session(session_token) {
            let mut s = session.lock();
            let pair = Self::pack_pair(s.initiator_peer_id, s.target_peer_id);
            let was_offloaded = OFFLOADED_PAIRS.contains_key(&pair);
            s.endpoint_a = None;
            s.endpoint_b = None;
            s.initiator_link_up = false;
            s.target_link_up = false;
            s.state = SessionState::ReadyForPunch;
            if OFFLOADED_PAIRS.remove(&pair).is_some() {
                OFFLOADED_PAIR_COUNT.fetch_sub(1, Ordering::Relaxed);
            }
            BNL::log(format!(
                "[P2P] LinkLost from peer {sender_id} (token {}); re-armed for punch, offload {}.",
                Self::preview(&s.token),
                if was_offloaded { "cleared (relay resumed)" } else { "already cleared" }
            ));
        }
        if let Some(other) = Self::peer(other_player_id) {
            Self::send_sub(&other, BasisNetworkCommons::P2P_SUB_LINK_LOST, session_token, sender_id as u16, None);
        }
    }

    fn forward_and_drop(sender: &NetPeerRef, msg: &BasisP2PSignalMessage, sub: u8) {
        if let Some(other) = Self::peer(i32::from(msg.other_player_id)) {
            Self::send_sub(&other, sub, &msg.session_token, sender.id() as u16, None);
        }
        if !msg.session_token.is_empty() {
            Self::remove_session(&msg.session_token);
        }
    }

    /// One side's introduction half. When both halves are in, each side is told the other's
    /// address; the initiator dials.
    pub fn on_introduce_request(sender_id: i32, token: &str, endpoint_addr: Vec<u8>) {
        if token.is_empty() {
            return;
        }
        let Some(session) = Self::session(token) else {
            BNL::log_warning(format!("[P2P] IntroduceRequest with unknown token {} — dropping.", Self::preview(token)));
            return;
        };
        let mut s = session.lock();
        if s.state < SessionState::ReadyForPunch {
            BNL::log_warning(format!("[P2P] IntroduceRequest for token {} in state {:?} — not ready, dropping.", Self::preview(token), s.state));
            return;
        }
        if sender_id != s.initiator_peer_id && sender_id != s.target_peer_id {
            BNL::log_warning(format!("[P2P] IntroduceRequest for token {} from peer {sender_id} outside the pair — dropping.", Self::preview(token)));
            return;
        }
        let introduction = PeerIntroduction { internal: None, external: None, iroh_addr: endpoint_addr };
        // Arrival order labels the slots; a repeat from the same peer refreshes its slot.
        if s.endpoint_a.is_none() || s.endpoint_a_peer == sender_id {
            s.endpoint_a = Some(introduction);
            s.endpoint_a_peer = sender_id;
        } else if s.endpoint_b.is_none() || s.endpoint_b_peer == sender_id {
            s.endpoint_b = Some(introduction);
            s.endpoint_b_peer = sender_id;
        }
        BNL::log(format!("[P2P] IntroduceRequest token={}; HasA={} HasB={}.", Self::preview(token), s.endpoint_a.is_some(), s.endpoint_b.is_some()));
        if let (Some(a), Some(b)) = (s.endpoint_a.clone(), s.endpoint_b.clone()) {
            let first_fire = s.state != SessionState::Punched;
            let (a_peer, b_peer, initiator) = (s.endpoint_a_peer, s.endpoint_b_peer, s.initiator_peer_id);
            s.state = SessionState::Punched;
            drop(s);
            if first_fire {
                BNL::log(format!("[P2P] Both endpoints collected for token {}. Introducing.", Self::preview(token)));
            }
            Self::send_introduce(a_peer, b_peer, &b, token, a_peer == initiator);
            Self::send_introduce(b_peer, a_peer, &a, token, b_peer == initiator);
        }
    }

    /// The `IPeerIntroducer::introduce` entry: both halves already collected by the caller.
    pub fn introduce(a: &PeerIntroduction, b: &PeerIntroduction, token: &str) {
        let Some(session) = Self::session(token) else {
            return;
        };
        let (initiator, target) = {
            let s = session.lock();
            (s.initiator_peer_id, s.target_peer_id)
        };
        Self::send_introduce(initiator, target, b, token, true);
        Self::send_introduce(target, initiator, a, token, false);
    }

    fn send_introduce(to_peer_id: i32, other_peer_id: i32, other: &PeerIntroduction, token: &str, dial: bool) {
        let Some(to) = Self::peer(to_peer_id) else {
            return;
        };
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(BasisNetworkCommons::P2P_SUB_INTRODUCE);
        let mut body = BasisP2PIntroduce {
            session_token: token.to_string(),
            other_player_id: other_peer_id as u16,
            dial,
            endpoint_addr: other.iroh_addr.clone(),
        };
        if body.serialize(&mut writer).is_ok() {
            NetworkServer::try_send(&to, &writer, BasisNetworkCommons::P2P_CHANNEL, DeliveryMethod::ReliableOrdered);
        }
        NetworkServer::return_writer(writer);
    }

    fn preview(token: &str) -> String {
        if token.is_empty() {
            return "(empty)".to_string();
        }
        token.chars().take(8).collect()
    }

    pub fn remove_peer(peer_id: i32) {
        let Some((_, tokens)) = PEER_SESSIONS.remove(&peer_id) else {
            return;
        };
        BNL::log(format!("[P2P] Peer {peer_id} disconnected; closing out {} P2P session(s).", tokens.len()));
        for token in tokens {
            let Some(session) = Self::session(&token) else {
                continue;
            };
            let other_id = {
                let s = session.lock();
                if s.initiator_peer_id == peer_id { s.target_peer_id } else { s.initiator_peer_id }
            };
            if let Some(other) = Self::peer(other_id) {
                BNL::log(format!("[P2P] Notifying peer {other_id} via Cancel that peer {peer_id} is gone (token {}).", Self::preview(&token)));
                Self::send_sub(&other, BasisNetworkCommons::P2P_SUB_CANCEL, &token, peer_id as u16, None);
            }
            Self::remove_session(&token);
        }
    }

    fn remove_session(token: &str) {
        let Some((_, session)) = SESSIONS.remove(token) else {
            return;
        };
        let (initiator, target) = {
            let s = session.lock();
            (s.initiator_peer_id, s.target_peer_id)
        };
        Self::untrack_peer_session(initiator, token);
        Self::untrack_peer_session(target, token);
        if OFFLOADED_PAIRS.remove(&Self::pack_pair(initiator, target)).is_some() {
            OFFLOADED_PAIR_COUNT.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn track_peer_session(peer_id: i32, token: &str) {
        PEER_SESSIONS.entry(peer_id).or_default().insert(token.to_string());
    }

    fn untrack_peer_session(peer_id: i32, token: &str) {
        if let Some(mut inner) = PEER_SESSIONS.get_mut(&peer_id) {
            inner.remove(token);
        }
    }

    fn send_sub(to: &NetPeerRef, sub: u8, token: &str, other_player_id: u16, ephemeral_public_key: Option<Vec<u8>>) {
        let mut writer = NetworkServer::rent_writer();
        writer.put_byte(sub);
        let mut body = BasisP2PSignalMessage { other_player_id, session_token: token.to_string(), ephemeral_public_key };
        if body.serialize(&mut writer).is_ok() {
            NetworkServer::try_send(to, &writer, BasisNetworkCommons::P2P_CHANNEL, DeliveryMethod::ReliableOrdered);
        }
        NetworkServer::return_writer(writer);
    }

    // ── Test seams ────────────────────────────────────────────────────────

    /// Clears all broker state so each test starts from a clean slate.
    pub fn reset_for_tests() {
        Self::reset_sessions();
        *INITIALIZED_MANAGER.lock() = None;
    }

    /// Registers a session the way handle_request would (session record + per-peer tracking),
    /// without needing peers. State starts past Awaiting (as it would be after Accept).
    pub fn register_session_for_tests(token: &str, initiator_id: i32, target_id: i32) {
        let session = Session {
            token: token.to_string(),
            initiator_peer_id: initiator_id,
            target_peer_id: target_id,
            state: SessionState::ReadyForPunch,
            endpoint_a: None,
            endpoint_b: None,
            endpoint_a_peer: 0,
            endpoint_b_peer: 0,
            initiator_link_up: false,
            target_link_up: false,
        };
        SESSIONS.insert(token.to_string(), Arc::new(Mutex::new(session)));
        Self::track_peer_session(initiator_id, token);
        Self::track_peer_session(target_id, token);
    }

    /// True if the broker currently holds a session under this token.
    pub fn has_session_for_tests(token: &str) -> bool {
        SESSIONS.contains_key(token)
    }

    pub fn session_state_for_tests(token: &str) -> Option<SessionState> {
        Self::session(token).map(|s| s.lock().state)
    }
}
