//! Port of `HealthCheckTests.cs`: the health endpoint over real HTTP.

use basis_network_core::configuration::Configuration;
use basis_network_server::NetworkServer;
use basis_network_server::diagnostics::BasisNetworkHealthCheck;
use basis_network_server::reduction::basis_server_reduction_system_events::now_ticks;
use basis_network_server::reduction::profiling::BSRProfiler;
use basis_rest_api_tests::support::HttpClient;
use serial_test::serial;

fn get_health(mut config: Configuration) -> serde_json::Value {
    config.health_check_host = "localhost".to_string();
    config.health_check_port = 0;
    config.health_path = "/health".to_string();

    let previous = NetworkServer::configuration();
    NetworkServer::set_configuration(config.clone());
    let mut check = BasisNetworkHealthCheck::new(&config).unwrap_or_else(|e| panic!("{}", e.report()));
    let response = HttpClient::new(check.bound_addr()).get("/health");
    check.stop();
    match previous {
        Some(previous) => NetworkServer::set_configuration((*previous).clone()),
        None => NetworkServer::clear_configuration(),
    }
    // 200 when the server is up, 503 while it is not; either way the body is the document.
    assert!(response.status == 200 || response.status == 503, "unexpected status {}: {}", response.status, response.body);
    response.json()
}

#[test]
#[serial]
fn health_omits_bsr_when_disabled() {
    let root = get_health(Configuration { health_include_bsr_profiling: false, ..Configuration::default() });
    assert!(root.get("bsr").is_none());
    assert_eq!(root["listening"], true);
    assert!(root.get("version").is_some());
}

#[test]
#[serial]
fn health_includes_live_load_when_enabled() {
    let root = get_health(Configuration { health_include_bsr_profiling: true, ..Configuration::default() });
    let load = &root["bsr"]["load"];
    for key in ["tickMs", "overrunRatio", "intervalMs", "hz", "shedTier", "sliceCount"] {
        assert!(load[key].is_number(), "{key} must be a number: {load}");
    }
    assert!(load["shedTierName"].is_string());
}

// BSRProfiler is process-global mutable state, so a single test owns its whole lifecycle rather
// than splitting it across siblings that can interleave.
#[test]
#[serial]
fn health_serializes_profiling_window() {
    BSRProfiler::reset_for_tests();

    let before = get_health(Configuration { health_include_bsr_profiling: true, ..Configuration::default() });
    assert!(before["bsr"]["window"].is_null(), "{before}");

    BSRProfiler::set_enabled(true);
    // 40 ticks carrying 900 messages between them.
    for i in 0..40 {
        BSRProfiler::add_tick(if i == 39 { 900 - 39 * 22 } else { 22 });
    }
    BSRProfiler::add_drain_ticks(1000); // 1 ms in the µs tick
    BSRProfiler::add_process_ticks(2000); // 2 ms
    BSRProfiler::local(|c| {
        use std::sync::atomic::Ordering;
        c.sends.store(120, Ordering::Relaxed);
        c.bundles_emitted.store(8, Ordering::Relaxed);
        c.bundle_messages.store(64, Ordering::Relaxed);
        c.bundle_raw_bytes.store(4096, Ordering::Relaxed);
        c.bundle_compressed_bytes.store(1024, Ordering::Relaxed);
    });
    BSRProfiler::flush_window_for_tests(now_ticks());
    assert!(BSRProfiler::latest().is_some());

    let root = get_health(Configuration { health_include_bsr_profiling: true, ..Configuration::default() });
    let window = &root["bsr"]["window"];
    assert_eq!(window["ticks"], 40, "{window}");
    assert_eq!(window["messages"], 900);
    assert_eq!(window["sends"], 120);

    let per_tick = &window["msPerTick"];
    assert!(per_tick["drain"].is_number());
    assert!(per_tick["total"].as_f64().unwrap() > 0.0);

    let bundles = &window["bundles"];
    assert_eq!(bundles["emitted"], 8);
    assert_eq!(bundles["savedBytes"], 3072);
    assert!((bundles["ratio"].as_f64().unwrap() - 0.25).abs() < 1e-3);
    assert!((bundles["avgMessages"].as_f64().unwrap() - 8.0).abs() < 1e-3);

    BSRProfiler::reset_for_tests();
    BSRProfiler::set_enabled(true);
    for _ in 0..5 {
        BSRProfiler::add_tick(0);
    }
    BSRProfiler::flush_window_for_tests(now_ticks());

    let sparse = get_health(Configuration { health_include_bsr_profiling: true, ..Configuration::default() });
    let sparse_window = &sparse["bsr"]["window"];
    assert_eq!(sparse_window["msPerTick"]["total"].as_f64(), Some(0.0), "{sparse_window}");
    assert_eq!(sparse_window["bundles"]["ratio"].as_f64(), Some(0.0));
    assert_eq!(sparse_window["bundles"]["avgDeflateUs"].as_f64(), Some(0.0));

    BSRProfiler::reset_for_tests();
}

#[test]
#[serial]
fn a_busy_health_port_is_reported_not_panicked() {
    let config = Configuration { health_check_host: "localhost".to_string(), health_check_port: 0, ..Configuration::default() };
    let mut first = BasisNetworkHealthCheck::new(&config).unwrap_or_else(|e| panic!("{}", e.report()));
    let busy = Configuration { health_check_port: first.bound_addr().port(), ..config.clone() };
    let err = BasisNetworkHealthCheck::new(&busy).err().expect("the second bind must fail");
    assert!(err.is_transient(), "{}", err.report());
    first.stop();

    let bad_host = Configuration { health_check_host: "nowhere.invalid.example".to_string(), ..config };
    let err = BasisNetworkHealthCheck::new(&bad_host).err().expect("a bad host must fail");
    assert!(!err.is_transient(), "{}", err.report());
}
