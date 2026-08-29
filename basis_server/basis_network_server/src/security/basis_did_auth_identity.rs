//! Port of `Security/BasisDIDAuthIdentity.cs`: the DID challenge/response identity provider.

use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

use basis_crypto::Signature;
use basis_did::did_auth::{Challenge, Config, DidAuthentication, Response};
use basis_did::newtypes::{Did, DidUrlFragment};
use basis_error::{BasisError, BasisResult, ErrorCode, ResultExt};
use basis_network_core::SerializableBasis::{BytesMessage, ReadyMessage};
use basis_network_core::configuration::Configuration;
use basis_network_core::transport::basis_network_shell::{SubscriptionId, peers_equal};
use basis_network_core::transport::iroh_network_impl::IrohRuntime;
use basis_network_core::{BNL, BasisNetworkCommons, ConnectionRequest, DeliveryMethod, NetDataReader, NetPacketReader, NetPeerRef};
use dashmap::DashMap;
use dashmap::mapref::entry::Entry;
use parking_lot::Mutex;
use tokio::task::AbortHandle;

use crate::NetworkServer;
use crate::auth::{IAuthIdentity, IAuthIdentitySupport};
use crate::core::basis_server_handle_events::BasisServerHandleEvents;
use crate::security::BasisPlayerModeration;

/// One in-flight or completed authentication: what the peer sent to connect, the challenge it
/// was given, and the identity it claimed.
#[derive(Clone)]
pub struct OnAuth {
    pub ready_message: ReadyMessage,
    pub challenge: Challenge,
    pub did: Did,
    pub peer: NetPeerRef,
}

pub struct BasisDIDAuthIdentity {
    did_auth: DidAuthentication,
    auth_identity: DashMap<i32, OnAuth>,
    timeouts: DashMap<i32, AbortHandle>,
    did_counts: DashMap<String, i32>,
    admins: DashMap<String, ()>,
    subscription: Mutex<Option<SubscriptionId>>,
    weak: Weak<BasisDIDAuthIdentity>,
}

impl BasisDIDAuthIdentity {
    // The handshake round trip degrades with how many peers are already on the server: measured
    // on a 32-core box it runs under 50 ms into a near-empty instance and 7.7 s at ~2,400 peers,
    // while verification itself stays at 0.16 ms. A flat window therefore stops being a
    // liveness check during a mass join and starts evicting peers whose reply is merely queued.
    // Scale the allowance with population so a lone unresponsive peer is still cut at the
    // configured value, and cap it so a dead peer can never linger indefinitely.
    const AUTH_TIMEOUT_PER_PEER_MS: i64 = 12;
    const AUTH_TIMEOUT_MAX_EXTRA_MS: i64 = 45_000;

    /// `config/admins.xml` next to the executable.
    pub fn file_path() -> PathBuf {
        NetworkServer::config_directory().join("admins.xml")
    }

    /// Builds the identity, loads the admin list and subscribes to the auth-received event.
    pub fn new() -> Arc<Self> {
        let this = Arc::new_cyclic(|weak| Self {
            did_auth: DidAuthentication::new(Config::default()),
            auth_identity: DashMap::new(),
            timeouts: DashMap::new(),
            did_counts: DashMap::new(),
            admins: DashMap::new(),
            subscription: Mutex::new(None),
            weak: weak.clone(),
        });
        for admin in Self::load_admins(&Self::file_path()) {
            this.admins.insert(admin, ());
        }
        let admins_list = this.admins().join(", ");
        BNL::log(format!("Loaded Admins {} {admins_list}", this.admins.len()));

        let weak = this.weak.clone();
        let id = BasisServerHandleEvents::subscribe_auth_received(Arc::new(move |reader: NetPacketReader, peer: NetPeerRef| {
            if let Some(this) = weak.upgrade() {
                this.on_auth_received(reader, peer);
            }
        }));
        *this.subscription.lock() = Some(id);
        BNL::log("DidAuthIdentity initialized.");
        this
    }

    pub fn unpack_string(compressed_bytes: &[u8]) -> String {
        String::from_utf8_lossy(compressed_bytes).into_owned()
    }

    /// The pending/authenticated entries, for diagnostics and tests.
    /// Installs an authenticated entry directly, bypassing the challenge round trip, so a test
    /// can stage which connection owns a peer id.
    pub fn register_for_tests(&self, id: i32, uuid: &str, peer: NetPeerRef) {
        let did = Did::new(uuid);
        let challenge = self.make_challenge(&did);
        self.auth_identity.insert(id, OnAuth { ready_message: ReadyMessage::default(), challenge, did, peer });
    }

    /// The entry currently holding `id`, if any.
    pub fn auth_entry(&self, id: i32) -> Option<OnAuth> {
        self.auth_identity.get(&id).map(|e| e.value().clone())
    }

    pub fn auth_entries(&self) -> Vec<(i32, OnAuth)> {
        self.auth_identity.iter().map(|e| (*e.key(), e.value().clone())).collect()
    }

    pub fn check_for_duplicates(&self, did: &Did) -> i32 {
        self.did_counts.get(did.v()).map(|c| *c).unwrap_or(0)
    }

    fn retain_did(&self, did: &Did) {
        *self.did_counts.entry(did.v().to_string()).or_insert(0) += 1;
    }

    fn release_did(&self, did: &Did) {
        let key = did.v();
        if key.is_empty() {
            return;
        }
        if let Entry::Occupied(mut entry) = self.did_counts.entry(key.to_string()) {
            if *entry.get() > 1 {
                *entry.get_mut() -= 1;
            } else {
                entry.remove();
            }
        }
    }

    fn try_process_connection(
        &self,
        configuration: &Configuration,
        _connection_request: &Arc<dyn ConnectionRequest>,
        mut reader: NetDataReader,
        new_peer: &NetPeerRef,
    ) -> BasisResult<()> {
        if configuration.log_connection_handshake {
            BNL::log(format!("Processing connection from peer {}.", new_peer.id()));
        }
        let mut ready_message = ReadyMessage::default();
        if ready_message.deserialize(&mut reader).is_err() || !ready_message.was_deserialized_correctly() {
            BasisServerHandleEvents::reject_with_reason(new_peer, "Invalid ReadyMessage received.");
            return Ok(());
        }

        if let Some(reason) = BasisServerHandleEvents::is_headless_disallowed(&ready_message.player_meta_data_message) {
            BasisServerHandleEvents::reject_with_reason(new_peer, &reason);
            return Ok(());
        }

        let uuid = ready_message.player_meta_data_message.player_uuid.clone();
        let player_did = Did::new(uuid.clone());
        if BasisPlayerModeration::is_banned(&uuid) {
            match BasisPlayerModeration::get_banned_reason(&uuid) {
                Some(reason) => BasisServerHandleEvents::reject_with_reason(new_peer, &format!("Banned User!  Reason {reason}")),
                None => BasisServerHandleEvents::reject_with_reason(new_peer, " Banned User!"),
            }
            return Ok(());
        }

        let stale = self.auth_identity.get(&new_peer.id()).map(|entry| entry.peer.clone());
        if let Some(stale) = stale
            && !peers_equal(&stale, new_peer)
        {
            BNL::log(format!(
                "Auth slot {} still held by a stale connection; releasing it for the incoming peer.",
                new_peer.id()
            ));
            self.remove_connection_expected(new_peer.id(), &stale);
        }

        if configuration.how_many_duplicate_auth_can_exist <= self.check_for_duplicates(&player_did) {
            BasisServerHandleEvents::reject_with_reason(new_peer, "To Many Auths From this DID!");
            return Ok(());
        }

        let challenge = self.make_challenge(&player_did);
        let nonce = challenge.nonce.v().to_vec();
        let on_auth = OnAuth { did: player_did.clone(), challenge, ready_message, peer: new_peer.clone() };

        match self.auth_identity.entry(new_peer.id()) {
            Entry::Occupied(_) => {
                BasisServerHandleEvents::reject_with_reason(new_peer, "Payload Provided was invalid! potential Duplication");
                return Ok(());
            }
            Entry::Vacant(slot) => {
                slot.insert(on_auth);
            }
        }
        self.retain_did(&player_did);

        let mut writer = NetworkServer::rent_writer();
        let serialized = BytesMessage.serialize(&mut writer, &nonce);
        if serialized.is_ok() {
            if configuration.log_connection_handshake {
                BNL::log(format!("Sending out Writer with size : {}", writer.length()));
            }
            NetworkServer::try_send(new_peer, &writer, BasisNetworkCommons::AUTH_IDENTITY_CHANNEL, DeliveryMethod::ReliableOrdered);
        }
        NetworkServer::return_writer(writer);
        serialized.context("serializing the auth challenge")?;

        let timeout_ms = Self::get_auth_timeout_ms(NetworkServer::connected_peers_count());
        let weak = self.weak.clone();
        let peer = new_peer.clone();
        let handle = IrohRuntime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(0))).await;
            if let Some(this) = weak.upgrade() {
                this.time_out(&peer, &uuid);
            }
        })
        .context("scheduling the authentication timeout")?;
        if let Some(previous) = self.timeouts.insert(new_peer.id(), handle.abort_handle()) {
            previous.abort();
        }
        Ok(())
    }

    pub fn get_auth_timeout_ms(population: i32) -> i32 {
        let configured = NetworkServer::configuration_or_default().auth_validation_time_out_miliseconds;
        if population <= 0 {
            return configured;
        }
        let extra = (i64::from(population) * Self::AUTH_TIMEOUT_PER_PEER_MS).min(Self::AUTH_TIMEOUT_MAX_EXTRA_MS);
        i32::try_from(i64::from(configured) + extra).unwrap_or(i32::MAX)
    }

    fn time_out(&self, new_peer: &NetPeerRef, uuid: &str) {
        if !self.remove_connection_expected(new_peer.id(), new_peer) {
            return;
        }
        BNL::log(format!("Authentication timeout for {uuid}."));
        BasisServerHandleEvents::reject_with_reason(new_peer, "Authentication timeout");
    }

    fn on_auth_received(&self, mut reader: NetPacketReader, new_peer: NetPeerRef) {
        if let Some((_, timeout)) = self.timeouts.remove(&new_peer.id()) {
            timeout.abort();
        }

        let Some(sig_bytes) = BytesMessage.deserialize(&mut reader) else {
            BNL::log_error(format!("Malformed auth response from peer {}: bad signature data", new_peer.id()));
            BasisServerHandleEvents::reject_with_reason(&new_peer, "Malformed auth response: bad signature data");
            return;
        };
        let Some(frag_bytes) = BytesMessage.deserialize(&mut reader) else {
            BNL::log_error(format!("Malformed auth response from peer {}: bad fragment data", new_peer.id()));
            BasisServerHandleEvents::reject_with_reason(&new_peer, "Malformed auth response: bad fragment data");
            return;
        };

        let mut fragment = Self::unpack_string(&frag_bytes);
        if fragment == "N/A" {
            fragment.clear();
        }
        let response = Response { signature: Signature::new(sig_bytes), did_url_fragment: DidUrlFragment::new(fragment) };

        let Some((challenge, ready_message, did)) = self
            .auth_identity
            .get(&new_peer.id())
            .map(|entry| (entry.challenge.clone(), entry.ready_message.clone(), entry.did.clone()))
        else {
            return;
        };

        match self.recv_challenge_response(&response, &challenge) {
            Ok(true) => BasisServerHandleEvents::on_network_accepted(&new_peer, ready_message, did.v()),
            Ok(false) => {
                BNL::log_error(format!("Authentication failed for {}.", did.v()));
                BasisServerHandleEvents::reject_with_reason(&new_peer, "was unable to authenticate!");
            }
            Err(e) => {
                BNL::log(format!("Error during authentication: {}", e.report()));
                BasisServerHandleEvents::reject_with_reason(&new_peer, "Authentication failed.");
            }
        }
    }

    pub fn make_challenge(&self, challenging_did: &Did) -> Challenge {
        self.did_auth.make_challenge(challenging_did.clone())
    }

    /// `Ok(false)` is a signature that does not verify; `Err` is a response the server cannot
    /// evaluate at all (a key fragment, which is not supported yet).
    pub fn recv_challenge_response(&self, response: &Response, challenge: &Challenge) -> BasisResult<bool> {
        if !response.did_url_fragment.v().is_empty() {
            return Err(BasisError::permanent(ErrorCode::Unsupported, "multiple pubkeys not yet supported"));
        }
        Ok(self.did_auth.verify_response(response, challenge).is_ok())
    }

    pub fn is_net_peer_admin(&self, uuid: &str) -> bool {
        self.admins.contains_key(uuid)
    }

    pub fn admins(&self) -> Vec<String> {
        self.admins.iter().map(|e| e.key().clone()).collect()
    }

    pub fn add_net_peer_as_admin(&self, uuid: &str) -> bool {
        if uuid.is_empty() {
            BNL::log(format!("can't add was empty or null! {uuid}"));
            return false;
        }
        BNL::log(format!("AddNetPeerAsAdmin {uuid}"));
        self.admins.insert(uuid.to_string(), ());
        Self::save_admins(&self.admins(), &Self::file_path());
        true
    }

    pub fn remove_net_peer_as_admin(&self, uuid: &str) -> bool {
        BNL::log(format!("RemoveNetPeerAsAdmin {uuid}"));
        if self.admins.remove(uuid).is_some() {
            Self::save_admins(&self.admins(), &Self::file_path());
            true
        } else {
            false
        }
    }

    fn save_admins(admins: &[String], file_path: &std::path::Path) {
        if !IAuthIdentitySupport::has_file_support() {
            return;
        }
        if let Err(e) = Self::write_admins(admins, file_path) {
            BNL::log_error(format!("Error saving admins: {e}"));
        }
    }

    fn write_admins(admins: &[String], file_path: &std::path::Path) -> std::io::Result<()> {
        let mut xml = String::from(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ArrayOfString xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\">\n",
        );
        for admin in admins {
            xml.push_str("  <string>");
            xml.push_str(&quick_xml::escape::escape(admin.as_str()));
            xml.push_str("</string>\n");
        }
        xml.push_str("</ArrayOfString>");
        if let Some(parent) = file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(file_path, xml)
    }

    /// Parses the `.NET XmlSerializer` `string[]` document.
    pub fn parse_admins(xml: &str) -> Result<Vec<String>, String> {
        use quick_xml::Reader;
        use quick_xml::events::Event;
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut admins = Vec::new();
        let mut saw_root = false;
        let mut depth = 0usize;
        loop {
            match reader.read_event_into(&mut buf).map_err(|e| e.to_string())? {
                Event::Start(e) => {
                    depth += 1;
                    let name = e.name().as_ref().to_owned();
                    if name == "ArrayOfString" {
                        saw_root = true;
                    } else if name == "string" {
                        let end = e.to_end().into_owned();
                        let text = reader.read_text(end.name()).map_err(|e| e.to_string())?;
                        let text = quick_xml::escape::unescape(&text).map(|c| c.into_owned()).unwrap_or_else(|_| text.to_string());
                        depth -= 1;
                        admins.push(text);
                    }
                }
                Event::End(_) => depth = depth.saturating_sub(1),
                Event::Eof => {
                    if depth > 0 {
                        return Err("unexpected end of document".to_string());
                    }
                    break;
                }
                _ => {}
            }
            buf.clear();
        }
        if !saw_root {
            return Err("missing <ArrayOfString> root".to_string());
        }
        Ok(admins)
    }

    fn load_admins(file_path: &std::path::Path) -> Vec<String> {
        if !IAuthIdentitySupport::has_file_support() {
            return Vec::new();
        }
        if file_path.exists() {
            match std::fs::read_to_string(file_path).map_err(|e| e.to_string()).and_then(|xml| Self::parse_admins(&xml)) {
                Ok(admins) => return admins,
                Err(e) => {
                    BNL::log_error(format!("Error loading admins (possibly corrupted file), deleting and recreating: {e}"));
                    if let Err(e) = std::fs::remove_file(file_path) {
                        BNL::log_error(format!("Could not delete the corrupted admins file: {e}"));
                    }
                }
            }
        }
        // If file is missing or corrupted, create a new one
        BNL::log("Creating a new admin list...");
        Self::save_admins(&[], file_path);
        Vec::new()
    }
}

impl IAuthIdentity for BasisDIDAuthIdentity {
    fn process_connection(&self, configuration: &Configuration, connection_request: &Arc<dyn ConnectionRequest>, data: NetDataReader, net_peer: &NetPeerRef) {
        if let Err(e) = self.try_process_connection(configuration, connection_request, data, net_peer) {
            BNL::log(format!("Error processing connection: {}", e.report()));
            BasisServerHandleEvents::reject_with_reason(net_peer, "Connection could not be processed.");
        }
    }

    fn de_initialize(&self) {
        if let Some(id) = self.subscription.lock().take() {
            BasisServerHandleEvents::unsubscribe_auth_received(id);
        }
        BNL::log("DidAuthIdentity deinitialized.");
    }

    fn remove_connection(&self, net_peer: i32) {
        let Some((_, entry)) = self.auth_identity.remove(&net_peer) else {
            return;
        };
        self.release_did(&entry.did);
        if let Some((_, timeout)) = self.timeouts.remove(&net_peer) {
            timeout.abort();
        }
    }

    fn remove_connection_expected(&self, id: i32, expected: &NetPeerRef) -> bool {
        let Some((_, entry)) = self.auth_identity.remove_if(&id, |_, held| peers_equal(&held.peer, expected)) else {
            return false;
        };
        self.release_did(&entry.did);
        if let Some((_, timeout)) = self.timeouts.remove(&id) {
            timeout.abort();
        }
        true
    }

    fn net_id_to_uuid(&self, peer: &NetPeerRef) -> Option<String> {
        let entry = self.auth_identity.get(&peer.id())?;
        peers_equal(&entry.peer, peer).then(|| entry.did.v().to_string())
    }

    fn uuid_to_net_id(&self, uuid: &str) -> Option<i32> {
        self.auth_identity.iter().find(|entry| entry.did.v() == uuid).map(|entry| *entry.key())
    }
}
