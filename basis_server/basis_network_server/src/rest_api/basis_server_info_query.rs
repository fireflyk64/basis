//! Port of `RestApi/BasisServerInfoQuery.cs`: unconnected "server info" probes — the UDP
//! equivalent of a server-list ping.
//!
//! Wire format (little-endian):
//!   Query:    `[u32 ServerInfoQueryMagic][u16 protoVersion][u16 nonce][padding ≥ ServerInfoMinRequestBytes total]`
//!   Response: `[u32 ServerInfoResponseMagic][u16 protoVersion][u16 nonce][u16 online][u16 max][string name][string motd]`
//!
//! DDoS protections: a minimum request size (amplification factor < 1), a global token bucket
//! on responses per second, and a bounded per-IP throttle.

use std::net::{IpAddr, SocketAddr};
use std::sync::LazyLock;
use std::time::Instant;

use basis_network_core::transport::basis_network_shell::SubscriptionId;
use basis_network_core::{BNL, BasisNetworkCommons, NetDataReader, NetDataWriter};
use dashmap::DashMap;
use parking_lot::Mutex;

use crate::NetworkServer;

struct GlobalBucket {
    tokens: f64,
    last_refill: Option<Instant>,
}

static LAST_SEEN: LazyLock<DashMap<IpAddr, i64>> = LazyLock::new(DashMap::new);
static BUCKET: Mutex<GlobalBucket> = Mutex::new(GlobalBucket { tokens: BasisServerInfoQuery::GLOBAL_BUCKET_CAPACITY, last_refill: None });
static CLOCK: LazyLock<Instant> = LazyLock::new(Instant::now);
static SUBSCRIPTION: Mutex<Option<SubscriptionId>> = Mutex::new(None);

pub struct BasisServerInfoQuery;

impl BasisServerInfoQuery {
    /// One response per IP per window.
    pub const MIN_INTERVAL_MS: i64 = 500;
    /// Cap memory under spoofed-IP floods: past this size the map is wiped.
    const MAX_TRACKED_IPS: usize = 4096;
    /// Caps responses/sec across every source.
    const GLOBAL_REFILL_TOKENS_PER_SECOND: f64 = 100.0;
    const GLOBAL_BUCKET_CAPACITY: f64 = 200.0;

    pub fn subscribe() {
        let Some(listener) = NetworkServer::listener() else {
            return;
        };
        Self::unsubscribe();
        let id = listener.network_receive_unconnected_event.subscribe(std::sync::Arc::new(Self::handle_query));
        *SUBSCRIPTION.lock() = Some(id);
    }

    pub fn unsubscribe() {
        if let Some(id) = SUBSCRIPTION.lock().take()
            && let Some(listener) = NetworkServer::listener()
        {
            listener.network_receive_unconnected_event.unsubscribe(id);
        }
    }

    fn handle_query(remote_end_point: SocketAddr, reader: NetDataReader) {
        let Some(response) = Self::build_response(remote_end_point, reader) else {
            return;
        };
        if let Some(server) = NetworkServer::server()
            && !server.send_unconnected_message(&response, remote_end_point)
        {
            BNL::log_warning("ServerInfoQuery failed: the transport refused the unconnected reply");
        }
        NetworkServer::return_writer(response);
    }

    /// The reply for one probe, or `None` when it is dropped (undersized, wrong magic, or rate
    /// limited). The returned writer is rented and must be returned by the caller.
    pub fn build_response(remote_end_point: SocketAddr, mut reader: NetDataReader) -> Option<NetDataWriter> {
        // Layer 1 — minimum request size. Drop tiny packets before anything else; they're the
        // cheapest amplification ammo.
        let total_bytes = reader.available_bytes();
        if total_bytes < BasisNetworkCommons::SERVER_INFO_MIN_REQUEST_BYTES || total_bytes < 8 {
            return None;
        }
        let magic = reader.get_uint().ok()?;
        if magic != BasisNetworkCommons::SERVER_INFO_QUERY_MAGIC {
            return None;
        }
        let _proto_version = reader.get_ushort().ok()?;
        let nonce = reader.get_ushort().ok()?;

        // Layer 2 — per-IP cooldown.
        if !Self::should_respond_per_ip(remote_end_point.ip()) {
            return None;
        }
        // Layer 3 — global response-rate cap.
        if !Self::try_consume_global_token() {
            return None;
        }

        let cfg = NetworkServer::configuration();
        let online = NetworkServer::authenticated_peers().len();
        let max = cfg.as_ref().map(|c| c.peer_limit).unwrap_or(0);
        let server_name = cfg.as_ref().map(|c| c.server_name.clone()).unwrap_or_default();
        let motd = cfg.as_ref().map(|c| c.server_motd.clone()).unwrap_or_default();

        let mut writer = NetworkServer::rent_writer();
        writer.put_uint(BasisNetworkCommons::SERVER_INFO_RESPONSE_MAGIC);
        writer.put_ushort(BasisNetworkCommons::SERVER_INFO_PROTOCOL_VERSION);
        writer.put_ushort(nonce);
        writer.put_ushort(u16::try_from(online).unwrap_or(u16::MAX));
        writer.put_ushort(u16::try_from(max.max(0)).unwrap_or(u16::MAX));
        let ok = writer.put_string_max(&server_name, BasisNetworkCommons::SERVER_INFO_NAME_MAX_LENGTH).is_ok()
            && writer.put_string_max(&motd, BasisNetworkCommons::SERVER_INFO_MOTD_MAX_LENGTH).is_ok();
        if !ok {
            NetworkServer::return_writer(writer);
            return None;
        }
        Some(writer)
    }

    fn now_ms() -> i64 {
        CLOCK.elapsed().as_millis() as i64
    }

    pub fn should_respond_per_ip(address: IpAddr) -> bool {
        // If a flood of unique source IPs is filling the map, wipe it. Crude but bounded.
        if LAST_SEEN.len() > Self::MAX_TRACKED_IPS {
            LAST_SEEN.clear();
        }
        let now_ms = Self::now_ms();
        let previous = *LAST_SEEN.entry(address).or_insert(0);
        if previous != 0 && now_ms - previous < Self::MIN_INTERVAL_MS {
            return false;
        }
        LAST_SEEN.insert(address, now_ms.max(1));
        true
    }

    pub fn try_consume_global_token() -> bool {
        let mut bucket = BUCKET.lock();
        let now = Instant::now();
        let last = bucket.last_refill.get_or_insert(now);
        let seconds_elapsed = now.duration_since(*last).as_secs_f64();
        if seconds_elapsed > 0.0 {
            bucket.tokens = Self::GLOBAL_BUCKET_CAPACITY.min(bucket.tokens + seconds_elapsed * Self::GLOBAL_REFILL_TOKENS_PER_SECOND);
            bucket.last_refill = Some(now);
        }
        if bucket.tokens < 1.0 {
            return false;
        }
        bucket.tokens -= 1.0;
        true
    }

    pub fn reset_for_tests() {
        LAST_SEEN.clear();
        *BUCKET.lock() = GlobalBucket { tokens: Self::GLOBAL_BUCKET_CAPACITY, last_refill: None };
    }
}
