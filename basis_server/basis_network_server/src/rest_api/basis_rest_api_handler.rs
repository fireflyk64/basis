//! Port of `RestApi/BasisRestApiHandler.cs`: bearer-token authenticated HTTP front for the routes.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, Response, StatusCode, Uri, header};
use basis_error::{BasisError, BasisResult, ErrorCode, ResultExt, io_fault_kind};
use basis_network_core::BNL;
use basis_network_core::configuration::Configuration;
use basis_network_core::transport::iroh_network_impl::IrohRuntime;
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::core::basis_server_control::{BasisServerControl, IServerControl};
use crate::diagnostics::basis_network_health_check::BasisNetworkHealthCheck;
use crate::rest_api::{ApiResponse, BasisRestApiRoutes};

struct ApiState {
    routes: BasisRestApiRoutes,
    /// SHA-256 of the configured key; empty when no key is configured (every request refused).
    key_hash: Vec<u8>,
    semaphore: Semaphore,
    cancellation: CancellationToken,
}

pub struct BasisRestApiHandler {
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    cancellation: CancellationToken,
    pub bound_addr: SocketAddr,
}

impl BasisRestApiHandler {
    const MAX_CONCURRENT_REQUESTS: usize = 32;

    pub fn new(config: &Configuration, control: Option<Arc<dyn IServerControl>>) -> BasisResult<Self> {
        let key_hash = if config.api_key.is_empty() { Vec::new() } else { Self::hash_bytes(config.api_key.as_bytes()) };
        if config.api_key.is_empty() {
            BNL::log_warning("[REST API] No ApiKey configured — all requests will be rejected. Set ApiKey in config to enable the REST API.");
        }
        let addr = BasisNetworkHealthCheck::bind_address(&config.api_host, config.api_port)?;
        let listener = IrohRuntime::block_on(async move { tokio::net::TcpListener::bind(addr).await })?
            .map_err(|e| BasisError::wrap(io_fault_kind(e.kind()), ErrorCode::Io, e))
            .with_context(|| format!("binding the REST API listener on {addr}"))?;
        let bound_addr = listener.local_addr().unwrap_or(addr);
        let cancellation = CancellationToken::new();
        let state = Arc::new(ApiState {
            routes: BasisRestApiRoutes::new(control.unwrap_or_else(BasisServerControl::shared)),
            key_hash,
            semaphore: Semaphore::new(Self::MAX_CONCURRENT_REQUESTS),
            cancellation: cancellation.clone(),
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
                BNL::log_warning(format!("REST API listener stopped unexpectedly: {e}"));
            }
        })?;
        BNL::log(format!("REST API started at http://{}:{}/api/", config.api_host, bound_addr.port()));
        Ok(Self { shutdown: Some(shutdown), task: Some(task), cancellation, bound_addr })
    }

    pub fn hash_bytes(data: &[u8]) -> Vec<u8> {
        Sha256::digest(data).to_vec()
    }

    /// Constant-time comparison of two digests.
    pub fn fixed_time_equals(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut diff = 0u8;
        for (x, y) in a.iter().zip(b) {
            diff |= x ^ y;
        }
        diff == 0
    }

    /// True when the Authorization header carries the configured bearer key.
    pub fn authenticate(key_hash: &[u8], authorization: Option<&str>) -> bool {
        if key_hash.is_empty() {
            return false;
        }
        let Some(auth) = authorization else {
            return false;
        };
        if auth.len() < 7 || !auth[..7].eq_ignore_ascii_case("Bearer ") {
            return false;
        }
        let token_hash = Self::hash_bytes(&auth.as_bytes()[7..]);
        Self::fixed_time_equals(key_hash, &token_hash)
    }

    async fn handle_request(State(state): State<Arc<ApiState>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response<Body> {
        let Ok(_permit) = state.semaphore.try_acquire() else {
            return Self::finish(ApiResponse::empty(503), false);
        };
        let authorization = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok());
        if !Self::authenticate(&state.key_hash, authorization) {
            return Self::finish(ApiResponse::empty(401), true);
        }
        let path = uri.path().trim_end_matches('/');
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 || segments[0] != "api" {
            return Self::finish(ApiResponse::empty(404), false);
        }
        let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            state.routes.dispatch(method.as_str(), &segments, &body, state.cancellation.clone())
        }))
        .unwrap_or_else(|_| BasisRestApiRoutes::internal_error());
        Self::finish(response, false)
    }

    fn finish(api: ApiResponse, unauthorized: bool) -> Response<Body> {
        let has_body = !api.body.is_empty();
        let mut response = Response::new(Body::from(api.body));
        *response.status_mut() = StatusCode::from_u16(api.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let headers = response.headers_mut();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store, max-age=0"));
        headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
        if has_body {
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json; charset=utf-8"));
        }
        if unauthorized {
            headers.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"basis-server\""));
        }
        if let Some(allow) = api.allow.as_deref().and_then(|a| HeaderValue::from_str(a).ok()) {
            headers.insert(header::ALLOW, allow);
        }
        response
    }

    pub fn stop(&mut self) {
        self.cancellation.cancel();
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = IrohRuntime::block_on(async move {
                let _ = tokio::time::timeout(std::time::Duration::from_millis(250), task).await;
            });
        }
    }
}

impl Drop for BasisRestApiHandler {
    fn drop(&mut self) {
        self.stop();
    }
}
