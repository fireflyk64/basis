//! Port of `Diagnostics/BasisNetworkHealthCheck.cs`: the unauthenticated HTTP health endpoint.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Method, Response, StatusCode, Uri, header};
use basis_error::{BasisError, BasisResult, ErrorCode, ResultExt, io_fault_kind};
use basis_network_core::BasisNetworkVersion;
use basis_network_core::compression::BasisAvatarBundleZstd;
use basis_network_core::configuration::{BasisTransportConfigStore, Configuration, IrohTransportConfig};
use basis_network_core::transport::basis_network_stack_registry::BasisNetworkStackRegistry;
use basis_network_core::transport::host_udp_capabilities::HostUdpCapabilities;
use basis_network_core::transport::iroh_network_impl::IrohRuntime;
use basis_network_core::transport::{IrohNetManager, LnlNetManager, MixedNetManager};
use basis_network_core::BNL;
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinHandle;

use crate::NetworkServer;
use crate::reduction::{BSRProfiler, BasisServerReductionSystemEvents};
use crate::util::{json_num, utc_now_iso8601, working_set_bytes};

struct HealthState {
    path_normalized: String,
    start_time_utc: String,
    // Same backpressure as the REST handler: an aggressive scraper (or a scanner — this port has
    // no auth) must not fan out arbitrarily many in-flight requests.
    semaphore: Semaphore,
}

pub struct BasisNetworkHealthCheck {
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    pub bound_addr: SocketAddr,
}

impl BasisNetworkHealthCheck {
    const MAX_CONCURRENT_REQUESTS: usize = 32;

    /// Binds and starts serving. A port already in use is a transient error; a bad host is
    /// permanent.
    pub fn new(config: &Configuration) -> BasisResult<Self> {
        let addr = Self::bind_address(&config.health_check_host, config.health_check_port)?;
        let path_normalized = Self::normalize_path(&config.health_path);
        let listener = IrohRuntime::block_on(async move { tokio::net::TcpListener::bind(addr).await })?
            .map_err(|e| BasisError::wrap(io_fault_kind(e.kind()), ErrorCode::Io, e))
            .with_context(|| format!("binding the health check listener on {addr}"))?;
        let bound_addr = listener.local_addr().unwrap_or(addr);
        let state = Arc::new(HealthState {
            path_normalized: path_normalized.clone(),
            start_time_utc: utc_now_iso8601(),
            semaphore: Semaphore::new(Self::MAX_CONCURRENT_REQUESTS),
        });
        let app = Router::new().fallback(Self::handle_request).with_state(state);
        let (shutdown, rx) = oneshot::channel::<()>();
        let task = IrohRuntime::spawn(async move {
            let served = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await;
            if let Err(e) = served {
                BNL::log_warning(format!("HTTP health check loop error: {e}"));
            }
        })?;
        BNL::log(format!("HTTP health check started at 'http://{}:{}{path_normalized}'", Self::format_host(&config.health_check_host), bound_addr.port()));
        Ok(Self { shutdown: Some(shutdown), task: Some(task), bound_addr })
    }

    pub(crate) fn bind_address(host: &str, port: u16) -> BasisResult<SocketAddr> {
        let host = host.trim();
        let ip: IpAddr = match host {
            "" | "*" | "+" | "0.0.0.0" => IpAddr::from([0, 0, 0, 0]),
            "localhost" => IpAddr::from([127, 0, 0, 1]),
            other => other
                .trim_matches(|c| c == '[' || c == ']')
                .parse()
                .map_err(|_| BasisError::permanent(ErrorCode::InvalidArgument, format!("'{other}' is not an IP address or 'localhost'")))?,
        };
        Ok(SocketAddr::new(ip, port))
    }

    /// Ensure a leading slash, remove a trailing slash (except root).
    pub fn normalize_path(p: &str) -> String {
        let p = p.trim();
        if p.is_empty() {
            return "/".to_string();
        }
        let mut p = if p.starts_with('/') { p.to_string() } else { format!("/{p}") };
        while p.len() > 1 && p.ends_with('/') {
            p.pop();
        }
        p
    }

    /// IPv6 literals need bracket notation in a URL.
    fn format_host(host: &str) -> String {
        match host.parse::<IpAddr>() {
            Ok(IpAddr::V6(_)) => format!("[{host}]"),
            _ => host.to_string(),
        }
    }

    async fn handle_request(State(state): State<Arc<HealthState>>, method: Method, uri: Uri) -> Response<Body> {
        let Ok(_permit) = state.semaphore.try_acquire() else {
            return Self::empty(StatusCode::SERVICE_UNAVAILABLE);
        };
        if method != Method::GET {
            return Self::empty(StatusCode::METHOD_NOT_ALLOWED);
        }
        if Self::normalize_path(uri.path()) != state.path_normalized {
            return Self::empty(StatusCode::NOT_FOUND);
        }
        let ready = NetworkServer::server().is_some();
        let json = Self::build_health_json(ready, &state.start_time_utc);
        let mut response = Response::new(Body::from(json));
        *response.status_mut() = if ready { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
        Self::harden(&mut response);
        response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
        response
    }

    fn empty(status: StatusCode) -> Response<Body> {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = status;
        Self::harden(&mut response);
        response
    }

    fn harden(response: &mut Response<Body>) {
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store, max-age=0"));
        response.headers_mut().insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    }

    /// The health document. Numeric fields are numbers; `bsr` is included only when configured.
    pub fn build_health_json(ready: bool, start_time_utc: &str) -> String {
        let configuration = NetworkServer::configuration_or_default();
        let now_utc = utc_now_iso8601();
        let bsr = if configuration.health_include_bsr_profiling { format!(",\"bsr\":{}", Self::build_bsr_json()) } else { String::new() };
        // Always on, unlike the BSR block: memory behaviour is the one thing that was otherwise
        // invisible here.
        let memory = format!(",\"gc\":{}", Self::build_memory_json());
        let version = BasisNetworkVersion::server_version();
        let ready_text = if ready { "true" } else { "false" };
        // Where each transport is reachable, so a tool that only has the health URL (the
        // benchmark harness, a launcher) can find the iroh listener as well as the legacy port.
        let listeners = Self::build_listeners_json();
        // What the host's UDP stack actually does — segmentation offload, and the socket buffers
        // the kernel granted against what iroh asked for. Both decide how a run on this host
        // should be read, and a benchmark run somewhere else has no other way to capture them.
        let host_udp = format!(
            ",\"hostUdp\":{},\"udpReceiveBufferDrops\":{}",
            HostUdpCapabilities::get().json(),
            crate::diagnostics::BasisNetworkUdpDropMonitor::total_receive_buffer_drops()
        );

        if configuration.enable_statistics
            && let Some(server) = NetworkServer::server()
        {
            let stats = server.statistics();
            let transport = BasisTransportConfigStore::get::<IrohTransportConfig>(BasisNetworkStackRegistry::IROH_ID);
            return format!(
                "{{\"listening\":true,\"ready\":{ready_text},\"visitors\":{},\"capacity\":{},\"sent\":{},\"recv\":{},\"packetsSent\":{},\"packetsRecv\":{},\"droppedUnreliable\":{},\"droppedVoice\":{},\"queuePerPeer\":{},\"voiceQueuePerPeer\":{},\"currentTime\":\"{now_utc}\",\"startTime\":\"{start_time_utc}\",\"version\":\"{version}\"{listeners}{host_udp}{memory}{bsr}}}",
                server.connected_peers_count(),
                configuration.peer_limit,
                stats.bytes_sent,
                stats.bytes_received,
                stats.packets_sent,
                stats.packets_received,
                server.unreliable_dropped(),
                server.priority_unreliable_dropped(),
                transport.max_datagram_queue_per_peer,
                transport.max_priority_datagram_queue_per_peer
            );
        }
        format!(
            "{{\"listening\":true,\"ready\":{ready_text},\"currentTime\":\"{now_utc}\",\"startTime\":\"{start_time_utc}\",\"version\":\"{version}\"{listeners}{host_udp}{memory}{bsr}}}"
        )
    }

    /// `"stack"`, `"legacyPort"` and `"iroh"` for whatever the server listens on. Empty before
    /// the transport is up.
    fn build_listeners_json() -> String {
        let Some(server) = NetworkServer::server() else {
            return String::new();
        };
        let any = server.as_any();
        if let Some(mixed) = any.downcast_ref::<MixedNetManager>() {
            return format!(",\"stack\":\"mixed\",\"legacyPort\":{},\"iroh\":\"{}\"", mixed.legacy_port(), mixed.connection_string());
        }
        if let Some(iroh) = any.downcast_ref::<IrohNetManager>() {
            return format!(",\"stack\":\"iroh\",\"iroh\":\"{}\"", iroh.connection_string());
        }
        if let Some(lnl) = any.downcast_ref::<LnlNetManager>() {
            return format!(",\"stack\":\"litenetlib\",\"legacyPort\":{}", lnl.local_port());
        }
        String::new()
    }

    /// Process memory. The C# reported GC generations; a Rust process has no collector, so the
    /// generation counters are 0 and `heapMb` is the resident set. Reclaim passes stand in for
    /// forced collections.
    pub fn build_memory_json() -> String {
        // `committedMb` is what tools built against the C# document read as "the process's
        // memory"; for a process without a managed heap that is its resident set, the same
        // figure as `heapMb`. A collector's pause time and fragmentation are 0 by construction.
        let resident_mb = json_num(working_set_bytes() as f64 / 1_048_576.0, 1);
        format!(
            "{{\"gen0\":0,\"gen1\":0,\"gen2\":{},\"heapMb\":{resident_mb},\"committedMb\":{resident_mb},\"fragmentedMb\":0,\"pauseTimePercent\":0,\"allocatedMb\":0,\"reclaimedMb\":{},\"serverGc\":false,\"latencyMode\":\"None\",\"runtime\":\"rust\"}}",
            crate::diagnostics::BasisServerMemoryReclaim::passes(),
            json_num(crate::diagnostics::BasisServerMemoryReclaim::reclaimed_bytes() as f64 / 1_048_576.0, 1)
        )
    }

    pub fn build_bsr_json() -> String {
        let interval = BasisServerReductionSystemEvents::interval_ms().max(1);
        let mut sb = String::with_capacity(768);
        sb.push_str(&format!(
            "{{\"load\":{{\"tickMs\":{},\"overrunRatio\":{},\"intervalMs\":{},\"hz\":{},\"shedTier\":{},\"shedTierName\":\"{}\",\"sliceCount\":{},\"sendWorkers\":{},\"sendWorkerCap\":{},\"sendBudgetPercent\":{},\"sendDuty\":{},\"pairsPerWorkerMs\":{}}}",
            json_num(BasisServerReductionSystemEvents::tick_ms_ema(), 3),
            json_num(BasisServerReductionSystemEvents::tick_overrun_ratio(), 4),
            interval,
            1000 / interval,
            BasisServerReductionSystemEvents::load_shed_tier(),
            BasisServerReductionSystemEvents::load_shed_tier_label(),
            BasisServerReductionSystemEvents::slice_count(),
            BasisServerReductionSystemEvents::send_workers(),
            BasisServerReductionSystemEvents::send_worker_ceiling(),
            BasisServerReductionSystemEvents::send_phase_budget_percent(),
            json_num(BasisServerReductionSystemEvents::send_budget_duty(), 4),
            json_num(BasisServerReductionSystemEvents::pairs_per_worker_ms(), 2)
        ));
        let Some(s) = BSRProfiler::latest() else {
            sb.push_str(",\"window\":null}");
            return sb;
        };
        let ticks = s.ticks as f64;
        let per_tick = |ms: f64| json_num(if ticks > 0.0 { ms / ticks } else { 0.0 }, 4);
        sb.push_str(&format!(
            ",\"window\":{{\"capturedTime\":\"{}\",\"ticks\":{},\"messages\":{},\"sends\":{},\"preSerialized\":{},\"preSerializedSkipped\":{},\"msPerTick\":{{\"drain\":{},\"process\":{},\"distance\":{},\"update\":{},\"trigger\":{},\"total\":{}}}",
            s.captured_utc,
            s.ticks,
            s.messages,
            s.sends,
            s.pre_serializations,
            s.pre_serializations_skipped,
            per_tick(s.drain_ms),
            per_tick(s.process_ms),
            per_tick(s.distance_ms),
            per_tick(s.update_ms),
            per_tick(s.trigger_ms),
            per_tick(s.total_ms)
        ));
        let ratio = |num: i64, den: i64| json_num(if den > 0 { num as f64 / den as f64 } else { 0.0 }, 4);
        let lz4_raw = s.bundle_raw_bytes - s.bundle_zstd_raw_bytes;
        let lz4_emitted = s.bundles_emitted - s.bundle_zstd_emitted;
        sb.push_str(&format!(
            ",\"bundles\":{{\"emitted\":{},\"messages\":{},\"tailUncompressed\":{},\"fallbacks\":{},\"retries\":{},\"rawBytes\":{},\"compressedBytes\":{},\"savedBytes\":{},\"ratio\":{},\"perTick\":{},\"avgMessages\":{},\"deflateMsPerTick\":{},\"avgDeflateUs\":{},\"zstd\":{{\"dictGeneration\":{},\"emitted\":{},\"shareOfBundles\":{},\"rawBytes\":{},\"compressedBytes\":{},\"ratio\":{},\"msPerTick\":{},\"avgUs\":{},\"lz4Ratio\":{},\"lz4AvgUs\":{}}}}}}}}}",
            s.bundles_emitted,
            s.bundle_messages,
            s.bundle_tail_uncompressed,
            s.bundle_fallbacks,
            s.bundle_retries,
            s.bundle_raw_bytes,
            s.bundle_compressed_bytes,
            s.bundle_raw_bytes - s.bundle_compressed_bytes,
            ratio(s.bundle_compressed_bytes, s.bundle_raw_bytes),
            per_tick(s.bundles_emitted as f64),
            json_num(if s.bundles_emitted > 0 { s.bundle_messages as f64 / s.bundles_emitted as f64 } else { 0.0 }, 2),
            per_tick(s.bundle_deflate_ms),
            json_num(if s.bundles_emitted > 0 { s.bundle_deflate_ms * 1000.0 / s.bundles_emitted as f64 } else { 0.0 }, 2),
            BasisAvatarBundleZstd::dictionary_generation(),
            s.bundle_zstd_emitted,
            ratio(s.bundle_zstd_emitted, s.bundles_emitted),
            s.bundle_zstd_raw_bytes,
            s.bundle_zstd_compressed_bytes,
            ratio(s.bundle_zstd_compressed_bytes, s.bundle_zstd_raw_bytes),
            per_tick(s.bundle_zstd_ms),
            json_num(if s.bundle_zstd_emitted > 0 { s.bundle_zstd_ms * 1000.0 / s.bundle_zstd_emitted as f64 } else { 0.0 }, 2),
            ratio(s.bundle_compressed_bytes - s.bundle_zstd_compressed_bytes, lz4_raw),
            json_num(if lz4_emitted > 0 { (s.bundle_deflate_ms - s.bundle_zstd_ms) * 1000.0 / lz4_emitted as f64 } else { 0.0 }, 2)
        ));
        sb
    }

    /// The address the listener actually bound — with port 0 configured, the port the OS chose.
    pub fn bound_addr(&self) -> std::net::SocketAddr {
        self.bound_addr
    }

    pub fn stop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            // Give the graceful shutdown a moment, as the C# waited 250 ms on its loop.
            let _ = IrohRuntime::block_on(async move {
                let _ = tokio::time::timeout(std::time::Duration::from_millis(250), task).await;
            });
        }
    }
}

impl Drop for BasisNetworkHealthCheck {
    fn drop(&mut self) {
        self.stop();
    }
}
