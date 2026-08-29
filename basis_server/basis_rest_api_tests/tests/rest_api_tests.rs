//! Port of `RestApiTests.cs`: every route of the REST API over real HTTP.

use std::sync::Arc;

use basis_network_core::SerializableBasis::LocalLoadResource;
use basis_network_core::configuration::Configuration;
use basis_network_server::core::basis_server_control::{IServerControl, PlayerInfo, SwitchWorldParams, WorldInfo, WorldLoadParams};
use basis_network_server::resources::{BasisNetworkPreloadResourceManagement, BasisNetworkResourceManagement};
use basis_network_server::rest_api::BasisRestApiHandler;
use basis_rest_api_tests::support::{HttpClient, HttpResponse};
use serial_test::serial;
use tokio_util::sync::CancellationToken;

const API_KEY: &str = "test-secret-key";

struct Fixture {
    handler: BasisRestApiHandler,
    authed: HttpClient,
    anon: HttpClient,
}

impl Fixture {
    fn new() -> Self {
        BasisNetworkResourceManagement::clear_for_tests();
        BasisNetworkPreloadResourceManagement::reset();
        let handler = BasisRestApiHandler::new(&api_config(0), None).unwrap_or_else(|e| panic!("{}", e.report()));
        let addr = handler.bound_addr();
        Self { handler, authed: HttpClient::with_bearer(addr, API_KEY), anon: HttpClient::new(addr) }
    }

    fn post_json(&self, path: &str, json: &str) -> HttpResponse {
        self.authed.post_json(path, json)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.handler.stop();
        BasisNetworkResourceManagement::clear_for_tests();
        BasisNetworkPreloadResourceManagement::reset();
    }
}

fn api_config(port: u16) -> Configuration {
    Configuration { api_enabled: true, api_key: API_KEY.to_string(), api_host: "localhost".to_string(), api_port: port, ..Configuration::default() }
}

fn database() -> &'static dashmap::DashMap<String, LocalLoadResource> {
    BasisNetworkResourceManagement::ushort_network_database()
}

fn resource(net_id: &str, mode: u8, url: &str) -> LocalLoadResource {
    LocalLoadResource { loaded_net_id: net_id.to_string(), mode, combined_url: url.to_string(), ..Default::default() }
}

// ── Auth ──

#[test]
#[serial]
fn no_auth_header_returns_401() {
    let f = Fixture::new();
    assert_eq!(f.anon.get("/api/worlds").status, 401);
}

#[test]
#[serial]
fn wrong_token_returns_401() {
    let f = Fixture::new();
    let client = HttpClient::with_bearer(f.handler.bound_addr(), "wrong-token");
    assert_eq!(client.get("/api/worlds").status, 401);
}

#[test]
#[serial]
fn valid_token_does_not_return_401() {
    let f = Fixture::new();
    assert_ne!(f.authed.get("/api/worlds").status, 401);
}

// ── Routing ──

#[test]
#[serial]
fn unknown_path_returns_404() {
    let f = Fixture::new();
    assert_eq!(f.authed.get("/api/doesnotexist").status, 404);
}

#[test]
#[serial]
fn wrong_method_returns_405() {
    let f = Fixture::new();
    let res = f.authed.delete("/api/announce");
    assert_eq!(res.status, 405);
    assert!(res.header("allow").is_some(), "a 405 names the allowed methods");
}

// ── GET /api/worlds ──

#[test]
#[serial]
fn get_worlds_empty_returns_empty_list() {
    let f = Fixture::new();
    let res = f.authed.get("/api/worlds");
    assert_eq!(res.status, 200);
    assert_eq!(res.json()["worlds"].as_array().map(|a| a.len()), Some(0));
}

#[test]
#[serial]
fn get_worlds_returns_only_scenes() {
    let f = Fixture::new();
    database().insert("scene1".into(), resource("scene1", 1, "https://example.com/world.bee"));
    database().insert("prop1".into(), resource("prop1", 0, "https://example.com/prop.bee"));

    let worlds = f.authed.get("/api/worlds").json()["worlds"].clone();
    assert_eq!(worlds.as_array().map(|a| a.len()), Some(1));
    assert_eq!(worlds[0]["netId"], "scene1");
}

#[test]
#[serial]
fn get_worlds_fields_are_mapped_correctly() {
    let f = Fixture::new();
    database().insert("w1".into(), LocalLoadResource { persist: true, is_admin_locked: true, load_strategy: 0, ..resource("w1", 1, "https://example.com/world.bee") });

    let world = f.authed.get("/api/worlds").json()["worlds"][0].clone();
    assert_eq!(world["netId"], "w1");
    assert_eq!(world["url"], "https://example.com/world.bee");
    assert_eq!(world["persistent"], true);
    assert_eq!(world["adminLocked"], true);
    assert_eq!(world["strategy"], 0);
}

// ── POST /api/worlds ──

#[test]
#[serial]
fn load_world_missing_url_returns_400() {
    let f = Fixture::new();
    assert_eq!(f.post_json("/api/worlds", r#"{"password":"pass"}"#).status, 400);
}

#[test]
#[serial]
fn load_world_missing_password_returns_400() {
    let f = Fixture::new();
    assert_eq!(f.post_json("/api/worlds", r#"{"url":"https://example.com/w.bee"}"#).status, 400);
}

#[test]
#[serial]
fn load_world_immediate_returns_200_and_adds_to_database() {
    let f = Fixture::new();
    let res = f.post_json("/api/worlds", r#"{"url":"https://example.com/world.bee","password":"pass","strategy":"immediate"}"#);
    assert_eq!(res.status, 200, "{}", res.body);
    let doc = res.json();
    assert_eq!(doc["ok"], true);
    let net_id = doc["netId"].as_str().unwrap().to_string();
    assert!(database().contains_key(&net_id));
}

#[test]
#[serial]
fn load_world_synchronized_returns_200_and_adds_to_database() {
    let f = Fixture::new();
    let res = f.post_json("/api/worlds", r#"{"url":"https://example.com/world.bee","password":"pass","strategy":"synchronized"}"#);
    assert_eq!(res.status, 200, "{}", res.body);
    let net_id = res.json()["netId"].as_str().unwrap().to_string();
    assert!(database().contains_key(&net_id));
}

// ── DELETE /api/worlds/{netId} ──

#[test]
#[serial]
fn unload_world_not_found_returns_404() {
    let f = Fixture::new();
    assert_eq!(f.authed.delete("/api/worlds/nonexistent-id").status, 404);
}

#[test]
#[serial]
fn unload_world_found_returns_200_and_removes_from_database() {
    let f = Fixture::new();
    database().insert("to-delete".into(), resource("to-delete", 1, "https://example.com/w.bee"));
    let res = f.authed.delete("/api/worlds/to-delete");
    assert_eq!(res.status, 200, "{}", res.body);
    assert!(!database().contains_key("to-delete"));
}

// ── POST /api/announce ──

#[test]
#[serial]
fn announce_all_missing_message_returns_400() {
    let f = Fixture::new();
    assert_eq!(f.post_json("/api/announce", "{}").status, 400);
}

#[test]
#[serial]
fn announce_all_empty_message_returns_400() {
    let f = Fixture::new();
    assert_eq!(f.post_json("/api/announce", r#"{"message":""}"#).status, 400);
}

#[test]
#[serial]
fn announce_all_message_too_long_returns_400() {
    let f = Fixture::new();
    let body = serde_json::json!({ "message": "a".repeat(513) }).to_string();
    assert_eq!(f.post_json("/api/announce", &body).status, 400);
}

#[test]
#[serial]
fn announce_all_non_string_message_returns_400() {
    let f = Fixture::new();
    assert_eq!(f.post_json("/api/announce", r#"{"message":42}"#).status, 400);
}

#[test]
#[serial]
fn announce_all_valid_message_returns_200() {
    let f = Fixture::new();
    assert_eq!(f.post_json("/api/announce", r#"{"message":"hello world"}"#).status, 200);
}

// ── GET /api/players ──

#[test]
#[serial]
fn get_players_empty_returns_empty_list() {
    let f = Fixture::new();
    let res = f.authed.get("/api/players");
    assert_eq!(res.status, 200);
    assert_eq!(res.json()["players"].as_array().map(|a| a.len()), Some(0));
}

struct FakeControl {
    players: Vec<PlayerInfo>,
}

impl IServerControl for FakeControl {
    fn announce_all(&self, _message: &str) {}
    fn announce_player(&self, _uuid: &str, _message: &str) -> bool {
        false
    }
    fn load_world(&self, _p: &WorldLoadParams) -> String {
        "0".to_string()
    }
    fn unload_world(&self, _net_id: &str) -> bool {
        false
    }
    fn clear_all_worlds(&self) -> i32 {
        0
    }
    fn list_worlds(&self) -> Vec<WorldInfo> {
        Vec::new()
    }
    fn list_players(&self) -> Vec<PlayerInfo> {
        self.players.clone()
    }
    fn switch_world(&self, _p: &SwitchWorldParams, _cancellation: CancellationToken) -> String {
        "0".to_string()
    }
}

#[test]
#[serial]
fn get_players_position_is_array_when_known_null_when_not() {
    let _f = Fixture::new();
    let control = Arc::new(FakeControl {
        players: vec![
            PlayerInfo { net_id: 1, uuid: "uuid-1".into(), display_name: "Alice".into(), platform: "desktop".into(), position: Some([1.5, 2.0, -3.25]) },
            PlayerInfo { net_id: 2, uuid: "uuid-2".into(), display_name: "Bob".into(), platform: "vr".into(), position: None },
        ],
    });
    let mut handler = BasisRestApiHandler::new(&api_config(0), Some(control)).unwrap_or_else(|e| panic!("{}", e.report()));
    let client = HttpClient::with_bearer(handler.bound_addr(), API_KEY);

    let res = client.get("/api/players");
    assert_eq!(res.status, 200);
    let players = res.json()["players"].clone();
    let pos = players[0]["position"].as_array().cloned().unwrap();
    assert_eq!(pos.len(), 3);
    assert_eq!(pos[0].as_f64().unwrap() as f32, 1.5);
    assert_eq!(pos[1].as_f64().unwrap() as f32, 2.0);
    assert_eq!(pos[2].as_f64().unwrap() as f32, -3.25);
    assert!(players[1]["position"].is_null());
    handler.stop();
}

// ── POST /api/announce/{uuid} ──

#[test]
#[serial]
fn announce_player_unknown_uuid_returns_404() {
    let f = Fixture::new();
    assert_eq!(f.post_json("/api/announce/unknown-uuid-123", r#"{"message":"hi"}"#).status, 404);
}

#[test]
#[serial]
fn load_world_password_embedded_in_url_returns_200_and_stores_clean_url() {
    let f = Fixture::new();
    let res = f.post_json("/api/worlds", r#"{"url":"https://example.com/world.bee#secretpassword"}"#);
    assert_eq!(res.status, 200, "{}", res.body);
    let net_id = res.json()["netId"].as_str().unwrap().to_string();
    let r = database().get(&net_id).expect("stored");
    assert_eq!(r.combined_url, "https://example.com/world.bee");
    assert_eq!(r.unlock_password, "secretpassword");
}

#[test]
#[serial]
fn load_world_explicit_password_overrides_embedded() {
    let f = Fixture::new();
    let res = f.post_json("/api/worlds", r#"{"url":"https://example.com/world.bee#embedded","password":"explicit"}"#);
    assert_eq!(res.status, 200, "{}", res.body);
    let net_id = res.json()["netId"].as_str().unwrap().to_string();
    assert_eq!(database().get(&net_id).expect("stored").unlock_password, "explicit");
}

// ── POST /api/worlds/switch ──

#[test]
#[serial]
fn switch_world_missing_url_returns_400() {
    let f = Fixture::new();
    assert_eq!(f.post_json("/api/worlds/switch", r#"{"password":"pass"}"#).status, 400);
}

#[test]
#[serial]
fn switch_world_missing_password_no_fragment_returns_400() {
    let f = Fixture::new();
    assert_eq!(f.post_json("/api/worlds/switch", r#"{"url":"https://example.com/next.bee"}"#).status, 400);
}

#[test]
#[serial]
fn switch_world_valid_returns_200_and_adds_to_database() {
    let f = Fixture::new();
    let res = f.post_json("/api/worlds/switch", r#"{"url":"https://example.com/next.bee","password":"pass","announceMessage":"Switching!"}"#);
    assert_eq!(res.status, 200, "{}", res.body);
    let doc = res.json();
    assert_eq!(doc["ok"], true);
    let net_id = doc["netId"].as_str().unwrap().to_string();
    assert!(database().contains_key(&net_id));
}

#[test]
#[serial]
fn switch_world_password_embedded_in_url_returns_200_and_stores_clean_url() {
    let f = Fixture::new();
    let clean_url = "https://beefile.io/7ec036b1a8fdd4e7f439339be9cbf54d";
    let password = "MGFmZWU0Y2ZlMjExMzlkY2Y5MDJlMjQ3NTc1ZDhiODAwODk3ZjZiZWM4NWVmMzkyODA5YTk3NDRhMjE3NTQzZQ==";
    let res = f.post_json("/api/worlds/switch", &format!(r#"{{"url":"{clean_url}#{password}"}}"#));
    assert_eq!(res.status, 200, "{}", res.body);
    let net_id = res.json()["netId"].as_str().unwrap().to_string();
    let r = database().get(&net_id).expect("stored");
    assert_eq!(r.combined_url, clean_url);
    // Fragment passwords are base64-encoded; the server decodes them before storing.
    use base64::Engine;
    let decoded = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(password).unwrap()).unwrap();
    assert_eq!(r.unlock_password, decoded);
}

#[test]
#[serial]
fn switch_world_invalid_delay_returns_400() {
    let f = Fixture::new();
    assert_eq!(f.post_json("/api/worlds/switch", r#"{"url":"https://example.com/next.bee","password":"pass","delay":-1}"#).status, 400);
}

#[test]
#[serial]
fn switch_world_delay_too_large_returns_400() {
    let f = Fixture::new();
    assert_eq!(f.post_json("/api/worlds/switch", r#"{"url":"https://example.com/next.bee","password":"pass","delay":301}"#).status, 400);
}

#[test]
#[serial]
fn switch_world_with_delay_net_id_returned_immediately_load_deferred() {
    let f = Fixture::new();
    // delay > 0: announce is sent first (cross-channel ordering), load starts after delay
    let res = f.post_json("/api/worlds/switch", r#"{"url":"https://example.com/next.bee","password":"pass","delay":1,"announceMessage":"Loading in 1s"}"#);
    assert_eq!(res.status, 200, "{}", res.body);
    let net_id = res.json()["netId"].as_str().unwrap().to_string();
    assert!(!database().contains_key(&net_id), "load should not be in DB until delay expires");
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert!(database().contains_key(&net_id), "load should be in DB after delay expires");
}

// ── Negative paths the C# suite did not reach: the transport itself failing ──

#[test]
#[serial]
fn a_port_already_in_use_is_a_transient_error_not_a_panic() {
    let first = Fixture::new();
    let port = first.handler.bound_addr().port();
    let second = BasisRestApiHandler::new(&api_config(port), None);
    let err = second.err().expect("binding the same port twice must fail");
    assert!(err.is_transient(), "a busy port is transient: {}", err.report());
}

#[test]
#[serial]
fn a_host_that_is_not_an_address_is_a_permanent_error() {
    let config = Configuration { api_host: "not a host".to_string(), ..api_config(0) };
    let err = BasisRestApiHandler::new(&config, None).err().expect("an unparseable host must fail");
    assert!(!err.is_transient(), "{}", err.report());
}

#[test]
#[serial]
fn malformed_json_and_oversized_bodies_are_400_not_500() {
    let f = Fixture::new();
    assert_eq!(f.post_json("/api/announce", "{not json").status, 400);
    let huge = serde_json::json!({ "message": "x".repeat(2 * 1024 * 1024) }).to_string();
    let res = f.post_json("/api/announce", &huge);
    assert!(res.status == 400 || res.status == 413, "oversized body answered {}", res.status);
}
