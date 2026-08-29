//! Boots the server on an ephemeral port with file support off, checks the statics are wired,
//! and stops it again — the cheapest guard against a broken static initializer.

use basis_network_core::configuration::Configuration;
use basis_network_server::NetworkServer;
use basis_network_server::core::basis_server_control::{BasisServerControl, IServerControl};
use basis_network_server::rest_api::{ApiResponse, BasisRestApiRoutes};
use serial_test::serial;
use tokio_util::sync::CancellationToken;

fn test_configuration() -> Configuration {
    let mut configuration = Configuration {
        has_file_support: false,
        set_port: 0,
        enable_console: false,
        api_enabled: false,
        ..Configuration::default()
    };
    configuration
}

#[test]
#[serial]
fn the_server_starts_and_stops_without_file_support() {
    let started = NetworkServer::start_server(test_configuration());
    assert!(started.is_ok(), "{:?}", started.err());
    assert!(NetworkServer::server().is_some());
    assert!(NetworkServer::listener().is_some());
    assert!(NetworkServer::auth().is_some());
    assert!(NetworkServer::auth_identity().is_some());
    assert_eq!(NetworkServer::authenticated_peers().len(), 0);
    NetworkServer::stop_server();
    assert!(NetworkServer::server().is_none());
    assert!(NetworkServer::listener().is_none());
}

#[test]
#[serial]
fn a_second_start_replaces_the_first() {
    NetworkServer::start_server(test_configuration()).unwrap();
    let first = NetworkServer::server().unwrap();
    NetworkServer::start_server(test_configuration()).unwrap();
    let second = NetworkServer::server().unwrap();
    assert!(!std::sync::Arc::ptr_eq(&first, &second));
    NetworkServer::stop_server();
}

#[test]
fn the_rest_routes_answer_without_a_socket() {
    let routes = BasisRestApiRoutes::new(BasisServerControl::shared());
    let players = routes.dispatch("GET", &["api", "players"], b"", CancellationToken::new());
    assert_eq!(players.status, 200);
    assert!(players.body_str().starts_with("{\"players\":["));
    let wrong_method = routes.dispatch("DELETE", &["api", "players"], b"", CancellationToken::new());
    assert_eq!(wrong_method, ApiResponse { status: 405, body: Vec::new(), allow: Some("GET".to_string()) });
    let bad_json = routes.dispatch("POST", &["api", "announce"], b"{not json", CancellationToken::new());
    assert_eq!(bad_json.status, 400);
    assert!(bad_json.body_str().contains("invalid JSON body"));
}

#[test]
fn a_switch_world_with_a_delay_can_be_cancelled() {
    let control = BasisServerControl;
    let token = CancellationToken::new();
    let net_id = control.switch_world(
        &basis_network_server::core::basis_server_control::SwitchWorldParams {
            url: "https://example.invalid/world.bee".to_string(),
            password: "pw".to_string(),
            persistent: false,
            announce_message: String::new(),
            delay: 30,
        },
        token.clone(),
    );
    assert_eq!(net_id.len(), 32);
    token.cancel();
    assert!(control.list_worlds().iter().all(|w| w.net_id != net_id));
}
