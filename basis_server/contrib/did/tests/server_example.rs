//! Port of `Contrib/Auth/Did.Tests/ServerExample.cs`: a worked example of the challenge flow.
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};

use basis_crypto::{Ed25519, Payload, PrivKey, PubKey};
use basis_did::{Challenge, Config, Did, DidAuthentication, DidKeyResolver, DidUrlFragment, Response};
use rand::Rng;

struct ConnectionState {
    did: Option<Did>,
    challenge: Option<Challenge>,
}

impl ConnectionState {
    fn new() -> Self {
        Self { did: None, challenge: None }
    }

    fn player(&self) -> Option<&Did> {
        self.did.as_ref()
    }

    /// Returns false if connection should be terminated
    fn recv_did(&mut self, server: &Server, player_did: Did) -> bool {
        let banned = server.banned_dids.contains(&player_did);
        self.did = Some(player_did);
        !banned
    }

    fn send_challenge(&mut self, server: &Server) -> Challenge {
        let challenge = server
            .did_auth
            .make_challenge(self.did.clone().expect("call RecvDid first"));
        self.challenge = Some(challenge.clone());
        challenge
    }

    /// Returns false if connection should be terminated
    fn recv_challenge_response(&self, server: &Server, response: &Response) -> bool {
        if !response.did_url_fragment.0.is_empty() {
            panic!("multiple pubkeys not yet supported");
        }
        let challenge = self.challenge.as_ref().expect("call SendChallenge first");
        server.did_auth.verify_response(response, challenge).is_ok()
    }
}

struct Server {
    did_auth: DidAuthentication,
    banned_dids: HashSet<Did>,
    banned_ips: HashSet<IpAddr>,
    connections: HashMap<IpAddr, ConnectionState>,
}

impl Server {
    fn new(did_auth: DidAuthentication) -> Self {
        Self {
            did_auth,
            banned_dids: HashSet::new(),
            banned_ips: HashSet::new(),
            connections: HashMap::new(),
        }
    }

    fn ban(&mut self, player_ip: IpAddr) {
        self.banned_ips.insert(player_ip);
        let Some(conn) = self.connections.remove(&player_ip) else {
            // No such connection
            return;
        };
        if let Some(did) = conn.player() {
            self.banned_dids.insert(did.clone());
        }
    }

    /// Registers a connection whose challenge is sent to the player.
    fn on_connection(&mut self, remote_addr: IpAddr) -> IpAddr {
        self.connections.insert(remote_addr, ConnectionState::new());
        remote_addr
    }
}

fn random_key_pair(rng: &mut impl Rng) -> (PubKey, PrivKey) {
    let mut priv_key_bytes = vec![0u8; Ed25519::PRIVKEY_SIZE];
    rng.fill_bytes(&mut priv_key_bytes);
    let priv_key = PrivKey(priv_key_bytes);
    let pub_key = Ed25519::convert_privkey_to_pubkey(&priv_key).expect("privkey was invalid");
    (pub_key, priv_key)
}

#[test]
fn main() {
    // Client
    let mut rng = rand::rng();
    let (pub_key, priv_key) = random_key_pair(&mut rng);
    let player_did = DidKeyResolver::encode_pubkey_as_did(&pub_key);
    let player_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);

    // Server
    let cfg = Config::default();
    let mut server = Server::new(DidAuthentication::new(cfg));
    let key = server.on_connection(player_ip);
    let mut conn = server.connections.remove(&key).unwrap();
    assert!(conn.recv_did(&server, player_did.clone()));
    let challenge = conn.send_challenge(&server);

    // Client
    let payload_to_sign = Payload(challenge.nonce.0.clone());
    let sig = Ed25519::sign(&priv_key, &payload_to_sign)
        .expect("signing with a valid privkey should always succeed");
    assert!(Ed25519::verify(&pub_key, &sig, &payload_to_sign), "sanity check: verifying sig");
    // for simplicity, use an empty fragment since the client only has one pubkey
    let response = Response {
        signature: sig,
        did_url_fragment: DidUrlFragment(String::new()),
    };

    // Server
    let is_authenticated = conn.recv_challenge_response(&server, &response);
    assert!(is_authenticated, "the response should have been valid");
    server.connections.insert(key, conn);

    // Next we ban the player
    server.ban(player_ip);

    // Client tries to connect again, but from a different IP
    let banned_key = server.on_connection(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)));
    let mut banned_conn = server.connections.remove(&banned_key).unwrap();
    // Connection terminated when DID matches ban list
    assert!(!banned_conn.recv_did(&server, player_did));
}
