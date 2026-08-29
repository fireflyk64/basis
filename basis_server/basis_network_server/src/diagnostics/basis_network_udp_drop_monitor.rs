//! Port of `Diagnostics/BasisNetworkUdpDropMonitor.cs`: a Linux-only background sampler that
//! polls `/proc/net/snmp` and warns when the kernel drops inbound UDP datagrams.
//!
//! Two failure modes show up here:
//!   1. RcvbufErrors increasing => the receive side can't drain the socket buffer fast enough.
//!   2. InErrors > RcvbufErrors => checksum/decode-level corruption, unrelated to saturation.
//!
//! On non-Linux platforms `start` is a no-op.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use basis_network_core::BNL;
use parking_lot::Mutex;

struct Baseline {
    rcvbuf_errors: i64,
    in_errors: i64,
    in_csum_errors: i64,
}

static RUNNING: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);
static BASELINE: Mutex<Option<Baseline>> = Mutex::new(None);
static RECV_BUFFER_DROPS_TOTAL: AtomicI64 = AtomicI64::new(0);

pub struct BasisNetworkUdpDropMonitor;

impl BasisNetworkUdpDropMonitor {
    const SNMP_PATH: &'static str = "/proc/net/snmp";
    const SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

    /// Running total of receive-buffer drops since start. Cumulative rather than take-and-reset
    /// so callers can compare rates across windows.
    pub fn total_receive_buffer_drops() -> i64 {
        RECV_BUFFER_DROPS_TOTAL.load(Ordering::Relaxed)
    }

    pub fn start() {
        if !cfg!(target_os = "linux") {
            return;
        }
        if STARTED.swap(true, Ordering::AcqRel) {
            return;
        }
        if !std::path::Path::new(Self::SNMP_PATH).exists() {
            BNL::log_warning(format!("[UdpDropMonitor] {} not readable; monitoring disabled", Self::SNMP_PATH));
            STARTED.store(false, Ordering::Release);
            return;
        }
        RUNNING.store(true, Ordering::Release);
        *BASELINE.lock() = None;
        let spawned = std::thread::Builder::new().name("UdpDropMonitor".to_string()).spawn(Self::run);
        match spawned {
            Ok(_) => BNL::log("[UdpDropMonitor] Started; sampling /proc/net/snmp every 10s"),
            Err(e) => {
                RUNNING.store(false, Ordering::Release);
                STARTED.store(false, Ordering::Release);
                BNL::log_error(format!("[UdpDropMonitor] could not start: {e}"));
            }
        }
    }

    pub fn stop() {
        RUNNING.store(false, Ordering::Release);
        STARTED.store(false, Ordering::Release);
    }

    fn run() {
        while RUNNING.load(Ordering::Acquire) {
            Self::sample();
            std::thread::sleep(Self::SAMPLE_INTERVAL);
        }
    }

    fn sample() {
        let Ok(text) = std::fs::read_to_string(Self::SNMP_PATH) else {
            return;
        };
        let Some((rcvbuf_errors, in_errors, in_csum_errors)) = Self::parse_snmp_udp(&text) else {
            return;
        };
        Self::apply_sample(rcvbuf_errors, in_errors, in_csum_errors);
    }

    /// Feeds one counter reading. The first establishes the baseline; only deltas after that
    /// mean anything. Returns the receive-buffer drops attributed to this sample.
    pub fn apply_sample(rcvbuf_errors: i64, in_errors: i64, in_csum_errors: i64) -> i64 {
        let mut baseline = BASELINE.lock();
        let mut dropped_buf = 0;
        if let Some(previous) = baseline.as_ref() {
            dropped_buf = rcvbuf_errors - previous.rcvbuf_errors;
            let delta_in = in_errors - previous.in_errors;
            let delta_csum = in_csum_errors - previous.in_csum_errors;
            if dropped_buf > 0 {
                // Publish it, not just log it: this is the only direct evidence that the receive
                // side is the bottleneck.
                RECV_BUFFER_DROPS_TOTAL.fetch_add(dropped_buf, Ordering::Relaxed);
                BNL::log_warning(format!(
                    "[UdpDropMonitor] Kernel dropped {dropped_buf} UDP packets in last {}s (RcvbufErrors). Receive thread is saturated -- raise the transport's receive concurrency or grow sysctl net.core.rmem_max.",
                    Self::SAMPLE_INTERVAL.as_secs()
                ));
            }
            // InErrors - RcvbufErrors isolates non-buffer drops (checksum, length, etc.) so a bad
            // NIC/cable shows up distinctly from a saturated app.
            let other_drops = delta_in - dropped_buf.max(0);
            if other_drops > 0 {
                let detail = if delta_csum > 0 { format!(" (InCsumErrors +{delta_csum})") } else { String::new() };
                BNL::log_warning(format!(
                    "[UdpDropMonitor] {other_drops} additional UDP InErrors in last {}s{detail} -- not recv-buffer related; check NIC/link health.",
                    Self::SAMPLE_INTERVAL.as_secs()
                ));
            }
        }
        *baseline = Some(Baseline { rcvbuf_errors, in_errors, in_csum_errors });
        dropped_buf.max(0)
    }

    /// `/proc/net/snmp` emits two consecutive lines starting with "Udp:" — the first names the
    /// columns, the second has the values. The column set is kernel-version-dependent so columns
    /// are looked up by name. Returns `(RcvbufErrors, InErrors, InCsumErrors)`.
    pub fn parse_snmp_udp(text: &str) -> Option<(i64, i64, i64)> {
        let lines: Vec<&str> = text.lines().collect();
        let (header, data) = lines.windows(2).find(|w| w[0].starts_with("Udp:") && w[1].starts_with("Udp:")).map(|w| (w[0], w[1]))?;
        let headers: Vec<&str> = header.split_whitespace().collect();
        let values: Vec<&str> = data.split_whitespace().collect();
        if headers.len() != values.len() {
            return None;
        }
        let mut rcvbuf_errors = 0;
        let mut in_errors = 0;
        let mut in_csum_errors = 0;
        for (name, value) in headers.iter().zip(values.iter()).skip(1) {
            let parsed = value.parse::<i64>().unwrap_or(0);
            match *name {
                "RcvbufErrors" => rcvbuf_errors = parsed,
                "InErrors" => in_errors = parsed,
                "InCsumErrors" => in_csum_errors = parsed,
                _ => {}
            }
        }
        Some((rcvbuf_errors, in_errors, in_csum_errors))
    }

    /// Clears the baseline and totals. Tests.
    pub fn reset_for_tests() {
        *BASELINE.lock() = None;
        RECV_BUFFER_DROPS_TOTAL.store(0, Ordering::Relaxed);
    }
}
