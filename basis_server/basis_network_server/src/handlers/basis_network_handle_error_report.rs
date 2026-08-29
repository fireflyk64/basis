//! Port of `Handlers/BasisNetworkHandleErrorReport.cs`: receives client error/exception reports.
//!
//! Wire (client→server): `[eventType:1][severity:1][lenPrefixed PermissionCompression blob of (system, message, stack)]`
//!
//! The reporting client never sends its own identity — the authoritative UUID, display name and
//! platform are attached here from the peer's connect metadata. Only the first occurrence of each
//! unique error per user (this server session) is written, to `CrashReports/<uuid>.jsonl`. Gated
//! by `BasisCrashReportStateManager`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use basis_network_core::SerializableBasis::PermissionCompression;
use basis_network_core::configuration::Configuration;
use basis_network_core::{BNL, NetPacketReader, NetPeerRef};
use dashmap::DashMap;
use parking_lot::Mutex;

use crate::NetworkServer;
use crate::networking::BasisSavedState;
use crate::security::BasisCrashReportStateManager;
use crate::util::{json_escape, utc_now_iso8601};

// Dedup state is bounded on every axis: entries are 8-byte hashes, each user stops recording
// after MAX_SEEN_PER_USER distinct errors, and the whole table is wiped if the bucket count
// reaches MAX_TRACKED_USERS (reports landing under "unknown" belong to no disconnect, so that
// bucket would otherwise live forever).
static SEEN_PER_USER: LazyLock<DashMap<String, Arc<Mutex<HashSet<i64>>>>> = LazyLock::new(DashMap::new);
static FILE_LOCK: Mutex<()> = Mutex::new(());

pub struct BasisNetworkHandleErrorReport;

impl BasisNetworkHandleErrorReport {
    const MAX_MESSAGE_CHARS: usize = 2000;
    const MAX_STACK_CHARS: usize = 12000;
    const MAX_SEEN_PER_USER: usize = 256;
    const MAX_TRACKED_USERS: usize = 4096;

    pub fn remove_user(uuid: &str) {
        if uuid.is_empty() {
            return;
        }
        SEEN_PER_USER.remove(uuid);
    }

    pub fn clear_all_seen() {
        SEEN_PER_USER.clear();
    }

    pub fn crash_report_directory() -> PathBuf {
        Configuration::base_directory().join("CrashReports")
    }

    pub fn handle_event(mut reader: NetPacketReader, peer: &NetPeerRef, _event_type: u8) {
        if !BasisCrashReportStateManager::enabled() {
            return;
        }
        let Ok(severity) = reader.get_byte() else {
            return;
        };
        let Ok(compressed) = reader.get_bytes_with_length() else {
            return;
        };
        let parts = PermissionCompression::decompress_extras(&compressed, 3);
        let system = parts.first().cloned().unwrap_or_default();
        let message = parts.get(1).cloned().unwrap_or_default();
        let stack = parts.get(2).cloned().unwrap_or_default();

        if !NetworkServer::configuration_or_default().has_file_support {
            return;
        }

        let mut uuid = "unknown".to_string();
        let mut display_name = String::new();
        let mut platform = String::new();
        if let Some(meta) = BasisSavedState::get_last_player_meta_data(peer) {
            if !meta.player_uuid.is_empty() {
                uuid = meta.player_uuid;
            }
            display_name = meta.player_display_name;
            platform = meta.player_platform;
        }

        // Truncate before hashing so the dedup key and the written report agree.
        let message: String = message.chars().take(Self::MAX_MESSAGE_CHARS).collect();
        let stack: String = stack.chars().take(Self::MAX_STACK_CHARS).collect();

        if SEEN_PER_USER.len() >= Self::MAX_TRACKED_USERS && !SEEN_PER_USER.contains_key(&uuid) {
            SEEN_PER_USER.clear();
        }

        let hash = Self::compute_hash(severity, &system, &message, &stack);
        let seen = SEEN_PER_USER.entry(uuid.clone()).or_insert_with(|| Arc::new(Mutex::new(HashSet::new()))).clone();
        {
            let mut seen = seen.lock();
            if seen.len() >= Self::MAX_SEEN_PER_USER || !seen.insert(hash) {
                return;
            }
        }

        if let Err(e) = Self::write_report(&uuid, &display_name, &platform, severity, &system, &message, &stack) {
            BNL::log_error(format!("Failed to handle error report: {e}"));
        }
    }

    /// FNV-1a 64 over the severity, system, message and the first stack line. A collision only
    /// suppresses a duplicate report line; it never loses data. Mixes UTF-16 code units so the
    /// value matches the C# implementation.
    pub fn compute_hash(severity: u8, system: &str, message: &str, stack: &str) -> i64 {
        let first_stack_line = stack.split('\n').next().unwrap_or("");
        let mut hash: u64 = 14_695_981_039_346_656_037;
        hash = Self::fnv_mix_byte(hash, severity);
        hash = Self::fnv_mix_str(hash, system);
        hash = Self::fnv_mix_str(hash, message);
        hash = Self::fnv_mix_str(hash, first_stack_line);
        hash as i64
    }

    fn fnv_mix_byte(hash: u64, value: u8) -> u64 {
        (hash ^ u64::from(value)).wrapping_mul(1_099_511_628_211)
    }

    fn fnv_mix_str(mut hash: u64, value: &str) -> u64 {
        hash = (hash ^ 0x1F).wrapping_mul(1_099_511_628_211);
        for unit in value.encode_utf16() {
            hash = (hash ^ u64::from(unit as u8)).wrapping_mul(1_099_511_628_211);
            hash = (hash ^ u64::from((unit >> 8) as u8)).wrapping_mul(1_099_511_628_211);
        }
        hash
    }

    /// One JSON line for the report.
    pub fn format_report(uuid: &str, display_name: &str, platform: &str, severity: u8, system: &str, message: &str, stack: &str) -> String {
        let severity_name = match severity {
            1 => "exception",
            2 => "crash",
            _ => "error",
        };
        format!(
            "{{\"timeUtc\":\"{}\",\"uuid\":\"{}\",\"displayName\":\"{}\",\"platform\":\"{}\",\"severity\":\"{severity_name}\",\"system\":\"{}\",\"message\":\"{}\",\"stack\":\"{}\"}}",
            utc_now_iso8601(),
            json_escape(uuid),
            json_escape(display_name),
            json_escape(platform),
            json_escape(system),
            json_escape(message),
            json_escape(stack)
        )
    }

    fn write_report(uuid: &str, display_name: &str, platform: &str, severity: u8, system: &str, message: &str, stack: &str) -> std::io::Result<()> {
        use std::io::Write;
        let dir = Self::crash_report_directory();
        let file = dir.join(format!("{}.jsonl", Self::sanitize_file_name(uuid)));
        let line = Self::format_report(uuid, display_name, platform, severity, system, message, stack);
        let _guard = FILE_LOCK.lock();
        std::fs::create_dir_all(&dir)?;
        let mut handle = std::fs::OpenOptions::new().create(true).append(true).open(file)?;
        writeln!(handle, "{line}")
    }

    pub fn sanitize_file_name(value: &str) -> String {
        if value.is_empty() {
            return "unknown".to_string();
        }
        value
            .chars()
            .map(|c| if c.is_control() || matches!(c, '"' | '<' | '>' | '|' | ':' | '*' | '?' | '\\' | '/') { '_' } else { c })
            .collect()
    }
}
