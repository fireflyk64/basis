//! Port of `Networking/BasisImageBandwidthGovernor.cs`: server-side rate control for image and
//! animated-image traffic, in both directions.
//!
//! The client already paces its own image uploads against a budget the server advertises. That
//! is the right place for the *decision* — only the sharer knows how its fan-out splits between
//! relayed and direct peers — but the wrong place for the *guarantee*, because a modified client
//! simply ignores it. [`try_consume_egress`](BasisImageBandwidthGovernor::try_consume_egress) is
//! the server-side floor under it.
//!
//! The download direction has no client half at all: cache replay is the server's own send to an
//! arriving player who never requested it, so pacing it is the only control that exists.
//!
//! Dropping is the right response to overrun: image chunks are ReliableOrdered, so a dropped
//! relay ends that transfer and the recipients clean it up on their inbound timeout. Queueing the
//! overrun instead would grow exactly the backlog this exists to prevent.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use basis_network_core::{BNL, NetPeerRef};
use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};

use crate::NetworkServer;

/// A cached payload plus the owner it must be stamped with on the way out.
#[derive(Clone, Debug)]
pub struct PendingPayload {
    pub owner_id: u16,
    pub payload: Arc<[u8]>,
}

impl PendingPayload {
    pub fn new(owner_id: u16, payload: impl Into<Arc<[u8]>>) -> Self {
        Self { owner_id, payload: payload.into() }
    }
}

struct EgressBucket {
    tokens: f64,
    last_refill: Instant,
}

/// One arriving peer's outstanding replay, drained by the pump at the configured rate.
struct ReplayJob {
    peer: NetPeerRef,
    payloads: Vec<PendingPayload>,
    cursor: usize,
    tokens: f64,
    last_refill: Instant,
    /// Set when the pump has decided this job is finished, so an append racing the removal
    /// starts a fresh job instead of writing into one nobody will drain.
    retired: bool,
}

pub type SendPayloadFn = Arc<dyn Fn(&NetPeerRef, u16, &[u8]) + Send + Sync>;

static EGRESS: LazyLock<DashMap<u16, Mutex<EgressBucket>>> = LazyLock::new(DashMap::new);
static DROPPED_MESSAGES: AtomicI64 = AtomicI64::new(0);
static DROPPED_BYTES: AtomicI64 = AtomicI64::new(0);
static REPLAYS: LazyLock<DashMap<i32, Arc<Mutex<ReplayJob>>>> = LazyLock::new(DashMap::new);
static PUMP_RUNNING: AtomicBool = AtomicBool::new(false);
static PUMP_START_LOCK: Mutex<()> = Mutex::new(());
/// Whether enqueuing starts the background pump. Tests turn this off and drive
/// [`pump_once_for_tests`](BasisImageBandwidthGovernor::pump_once_for_tests) themselves.
static AUTO_PUMP: AtomicBool = AtomicBool::new(true);
/// Sends one payload to one peer. Supplied by the cache, which owns the wire format.
static SEND_PAYLOAD: RwLock<Option<SendPayloadFn>> = RwLock::new(None);

pub struct BasisImageBandwidthGovernor;

impl BasisImageBandwidthGovernor {
    /// Megabits per second to bytes per second.
    const MEGABITS_TO_BYTES: f64 = 125_000.0;
    /// Seconds of budget a bucket may bank while idle. Image traffic is bursty — a share is a
    /// wall of chunks and then nothing — so a bucket with no burst allowance would clip the
    /// front of every transfer. Two seconds covers a chunk train across a tick boundary without
    /// letting banked credit fund a sustained flood.
    const BURST_SECONDS: f64 = 2.0;
    /// How often the replay pump wakes.
    const PUMP_INTERVAL_MS: u64 = 25;

    /// Image relays refused because the sender was over its enforced budget.
    pub fn dropped_messages() -> i64 {
        DROPPED_MESSAGES.load(Ordering::Relaxed)
    }

    /// Bytes of server egress those refusals avoided, counting fan-out.
    pub fn dropped_bytes() -> i64 {
        DROPPED_BYTES.load(Ordering::Relaxed)
    }

    pub fn set_auto_pump(enabled: bool) {
        AUTO_PUMP.store(enabled, Ordering::Release);
    }

    pub fn set_send_payload(send: Option<SendPayloadFn>) {
        *SEND_PAYLOAD.write() = send;
    }

    fn enforced_egress_bytes_per_second() -> f64 {
        let Some(configuration) = NetworkServer::configuration() else {
            return 0.0;
        };
        let megabits = configuration.image_share_egress_megabits_per_second;
        if megabits <= 0 {
            return 0.0; // nothing advertised means nothing to enforce
        }
        // Enforcing below what was advertised is never correct.
        let percent = configuration.image_share_egress_enforcement_percent.max(100);
        f64::from(megabits) * Self::MEGABITS_TO_BYTES * (f64::from(percent) / 100.0)
    }

    /// Charges one image relay against its sender's budget. Returns false when the sender is
    /// over, in which case the caller must not relay the message. `bytes` is wire bytes
    /// multiplied by the number of peers this will be sent to.
    pub fn try_consume_egress(sender_id: u16, bytes: i64) -> bool {
        let rate_per_second = Self::enforced_egress_bytes_per_second();
        if rate_per_second <= 0.0 || bytes <= 0 {
            return true; // disabled
        }
        let bucket = EGRESS
            .entry(sender_id)
            .or_insert_with(|| Mutex::new(EgressBucket { tokens: rate_per_second * Self::BURST_SECONDS, last_refill: Instant::now() }));
        let mut bucket = bucket.lock();
        let now = Instant::now();
        let elapsed = now.duration_since(bucket.last_refill).as_secs_f64();
        if elapsed > 0.0 {
            bucket.last_refill = now;
            let ceiling = rate_per_second * Self::BURST_SECONDS;
            bucket.tokens = ceiling.min(bucket.tokens + rate_per_second * elapsed);
        }
        // Gate on having something rather than on the whole charge fitting, and let the bucket
        // go negative. A single chunk multiplied by a wide fan-out can exceed the entire burst
        // capacity, and a bucket that demanded the charge fit would deadlock that sender forever.
        if bucket.tokens <= 0.0 {
            DROPPED_MESSAGES.fetch_add(1, Ordering::Relaxed);
            DROPPED_BYTES.fetch_add(bytes, Ordering::Relaxed);
            return false;
        }
        bucket.tokens -= bytes as f64;
        true
    }

    fn replay_bytes_per_second() -> f64 {
        let Some(configuration) = NetworkServer::configuration() else {
            return 0.0;
        };
        let megabits = configuration.image_share_download_megabits_per_second;
        if megabits <= 0 { 0.0 } else { f64::from(megabits) * Self::MEGABITS_TO_BYTES }
    }

    /// Queues a peer's cache replay to be delivered at the configured rate. Returns false when
    /// pacing is disabled, in which case the caller should send inline as it always did.
    pub fn enqueue_replay(peer: &NetPeerRef, payloads: Vec<PendingPayload>) -> bool {
        if payloads.is_empty() {
            return false;
        }
        let rate_per_second = Self::replay_bytes_per_second();
        if rate_per_second <= 0.0 {
            return false;
        }
        // Append rather than replace. A peer picks up images repeatedly — once on join and again
        // each time they walk into range of another card — and overwriting the job would throw
        // away whatever of the previous batch had not gone out yet.
        if let Some(existing) = REPLAYS.get(&peer.id()).map(|j| j.clone()) {
            let mut job = existing.lock();
            if !job.retired {
                job.payloads.extend(payloads);
                job.peer = peer.clone();
                drop(job);
                Self::ensure_pump();
                return true;
            }
        }
        let job = ReplayJob {
            peer: peer.clone(),
            payloads,
            cursor: 0,
            tokens: rate_per_second * Self::BURST_SECONDS,
            last_refill: Instant::now(),
            retired: false,
        };
        REPLAYS.insert(peer.id(), Arc::new(Mutex::new(job)));
        Self::ensure_pump();
        true
    }

    fn ensure_pump() {
        if !AUTO_PUMP.load(Ordering::Acquire) || PUMP_RUNNING.load(Ordering::Acquire) {
            return;
        }
        let _guard = PUMP_START_LOCK.lock();
        if PUMP_RUNNING.swap(true, Ordering::AcqRel) {
            return;
        }
        let spawned = std::thread::Builder::new().name("BasisImageReplayPump".to_string()).spawn(Self::pump_loop);
        if let Err(e) = spawned {
            PUMP_RUNNING.store(false, Ordering::Release);
            BNL::log_error(format!("[ImageReplay] could not start the pump thread: {e}"));
        }
    }

    fn pump_loop() {
        while PUMP_RUNNING.load(Ordering::Acquire) {
            // A replay is a nicety — a joiner that misses one re-requests nothing and simply does
            // not see that image. A panicking send callback must never take the pump down.
            if std::panic::catch_unwind(Self::pump_once).is_err() {
                BNL::log_error("[ImageReplay] pump error: a send callback panicked");
            }
            std::thread::sleep(Duration::from_millis(Self::PUMP_INTERVAL_MS));
        }
    }

    fn pump_once() {
        if REPLAYS.is_empty() {
            return;
        }
        let rate_per_second = Self::replay_bytes_per_second();
        let send = SEND_PAYLOAD.read().clone();
        let jobs: Vec<(i32, Arc<Mutex<ReplayJob>>)> = REPLAYS.iter().map(|e| (*e.key(), e.value().clone())).collect();
        for (key, job) in jobs {
            // A rate turned off under us drops the remaining work rather than holding the payload
            // list alive. Departure is handled by remove_peer on the disconnect path; a send that
            // races a disconnect is harmless because try_send is what it says it is.
            let Some(send) = send.as_ref() else {
                REPLAYS.remove(&key);
                continue;
            };
            if rate_per_second <= 0.0 {
                REPLAYS.remove(&key);
                continue;
            }
            let mut guard = job.lock();
            let now = Instant::now();
            let elapsed = now.duration_since(guard.last_refill).as_secs_f64();
            if elapsed > 0.0 {
                guard.last_refill = now;
                let ceiling = rate_per_second * Self::BURST_SECONDS;
                guard.tokens = ceiling.min(guard.tokens + rate_per_second * elapsed);
            }
            while guard.tokens > 0.0 {
                if guard.cursor >= guard.payloads.len() {
                    break;
                }
                let pending = guard.payloads[guard.cursor].clone();
                guard.cursor += 1;
                let size = pending.payload.len();
                if size == 0 {
                    continue;
                }
                guard.tokens -= size as f64;
                let peer = guard.peer.clone();
                // Send outside the job lock so a slow transport cannot hold up an append.
                drop(guard);
                send(&peer, pending.owner_id, &pending.payload);
                guard = job.lock();
            }
            let retired = guard.cursor >= guard.payloads.len();
            guard.retired = retired;
            drop(guard);
            if retired {
                REPLAYS.remove(&key);
            }
        }
    }

    /// Forgets a peer's buckets and any queued replay. Called when they disconnect.
    pub fn remove_peer(peer_id: i32) {
        REPLAYS.remove(&peer_id);
        if let Ok(id) = u16::try_from(peer_id) {
            EGRESS.remove(&id);
        }
    }

    /// Drops all state. Called on server stop and from tests.
    pub fn reset() {
        PUMP_RUNNING.store(false, Ordering::Release);
        AUTO_PUMP.store(true, Ordering::Release);
        REPLAYS.clear();
        EGRESS.clear();
        DROPPED_MESSAGES.store(0, Ordering::Relaxed);
        DROPPED_BYTES.store(0, Ordering::Relaxed);
    }

    /// Test seam: runs one pump pass without the background thread.
    pub fn pump_once_for_tests() {
        Self::pump_once();
    }

    /// Test seam: whether a peer still has replay work outstanding.
    pub fn has_pending_replay(peer_id: i32) -> bool {
        REPLAYS.contains_key(&peer_id)
    }
}
