//! Port of `Security/BasisServerLogBundleService.cs`.
//!
//! Builds a single compressed bundle of the server's logs/ and CrashReports/ folders on demand
//! and streams it back to the requesting admin over the admin channel.
//!
//! The files are packed into one length-prefixed container, LZ4-compressed (the same codec
//! already used for avatar bundles), and split into ordered chunks so a large transfer never
//! depends on one oversized datagram. The admin channel is ReliableOrdered, so the client
//! reassembles them in send order. The whole build + send runs off the network thread, and each
//! chunk uses a fresh writer (no shared pool) to stay thread-safe.
//!
//! Container (before compression):
//!   `[int fileCount]` then per file: `[string relativePath][int byteLength][bytes]`
//!
//! Wire (server→client), all on AdminChannel, ReliableOrdered:
//!   LogBundleBegin : `[string serverNameSafe][string fileName][bool isCompressed][int payloadBytes][int rawBytes][int totalChunks]`
//!   LogBundleChunk : `[int chunkIndex][lenPrefixed bytes]` (repeated totalChunks times)
//!   LogBundleEnd   : `[bool ok][string message]`
//!
//! Gated upstream by `PermNodes::ADMIN_LOGS` in `BasisPlayerModeration`.

use std::path::{Path, PathBuf};

use basis_network_core::SerializableBasis::{AdminRequest, AdminRequestMode};
use basis_network_core::configuration::Configuration;
use basis_network_core::{BNL, BasisNetworkCommons, DeliveryMethod, NetDataWriter, NetPeerRef};

use crate::NetworkServer;
use crate::handlers::BasisNetworkHandleErrorReport;
use crate::security::BasisPlayerModeration;

pub struct BasisServerLogBundleService;

impl BasisServerLogBundleService {
    /// Bytes per streamed chunk. Comfortably below any single-message limit while keeping the
    /// chunk count low.
    const CHUNK_SIZE: usize = 32 * 1024;

    /// Hard ceiling on the assembled (raw) container. Logs that big almost certainly mean
    /// something is wrong; refuse rather than flood the link.
    const MAX_RAW_BYTES: usize = 256 * 1024 * 1024;

    pub fn send_all_logs_to_peer(peer: &NetPeerRef) {
        if !NetworkServer::configuration_or_default().has_file_support {
            BasisPlayerModeration::send_back_message(peer, "File support is disabled on this server; there are no logs to pull.");
            return;
        }
        // Build and stream off the network thread — packing can touch many files.
        let peer = peer.clone();
        Self::spawn("BasisLogBundleBuild", move || Self::build_and_send(&peer));
    }

    pub fn delete_all_logs_for_peer(peer: &NetPeerRef) {
        if !NetworkServer::configuration_or_default().has_file_support {
            BasisPlayerModeration::send_back_message(peer, "File support is disabled on this server; there are no logs to delete.");
            return;
        }
        // Deletion touches many files; keep it off the network thread.
        let peer = peer.clone();
        Self::spawn("BasisLogBundleDelete", move || Self::delete_all(&peer));
    }

    fn spawn(name: &str, work: impl FnOnce() + Send + 'static) {
        if let Err(e) = std::thread::Builder::new().name(name.to_string()).spawn(work) {
            BNL::log_error(format!("Could not start the {name} thread: {e}"));
        }
    }

    fn logs_dir() -> PathBuf {
        Configuration::base_directory().join(Configuration::LOGS_FOLDER_NAME)
    }

    fn crash_dir() -> PathBuf {
        Configuration::base_directory().join("CrashReports")
    }

    fn delete_all(peer: &NetPeerRef) {
        let deleted = Self::delete_directory_files(&Self::logs_dir()) + Self::delete_directory_files(&Self::crash_dir());

        // The error-report writer dedupes identical reports per user for this server session;
        // forget that history so fresh occurrences are recorded again.
        BasisNetworkHandleErrorReport::clear_all_seen();

        BasisPlayerModeration::send_back_message(peer, &format!("Deleted {deleted} log/crash file(s) from logs/ and CrashReports/."));
        BNL::log(format!("Admin (peer {}) deleted {deleted} server log/crash file(s).", peer.id()));
    }

    fn delete_directory_files(source_dir: &Path) -> usize {
        if !source_dir.is_dir() {
            return 0;
        }
        let mut deleted = 0;
        for file in Self::enumerate_files(source_dir) {
            match std::fs::remove_file(&file) {
                Ok(()) => deleted += 1,
                Err(e) => {
                    // The current day's log file is still open for append and can't be removed while running.
                    BNL::log_warning(format!("Could not delete log file '{}' (in use?): {e}", file.display()));
                }
            }
        }
        deleted
    }

    fn enumerate_files(dir: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(current) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }

    fn build_and_send(peer: &NetPeerRef) {
        let (raw, file_count) = Self::build_container(&Self::logs_dir(), &Self::crash_dir());
        if raw.is_empty() || file_count == 0 {
            BasisPlayerModeration::send_back_message(peer, "No log files were found to send.");
            return;
        }
        if raw.len() > Self::MAX_RAW_BYTES {
            BasisPlayerModeration::send_back_message(
                peer,
                &format!(
                    "Log bundle is too large to send ({} MB, limit {} MB).",
                    raw.len() / (1024 * 1024),
                    Self::MAX_RAW_BYTES / (1024 * 1024)
                ),
            );
            return;
        }

        let (payload, is_compressed) = Self::compress(raw.clone());
        let server_name_safe = Self::sanitize_name(&NetworkServer::configuration_or_default().server_name);
        let total_chunks = payload.len().div_ceil(Self::CHUNK_SIZE);

        if !Self::send_begin(peer, &server_name_safe, "logs", is_compressed, payload.len(), raw.len(), total_chunks) {
            Self::send_end(peer, false, "Server failed to build the log bundle. See server log.");
            return;
        }
        for (index, slice) in payload.chunks(Self::CHUNK_SIZE).enumerate() {
            if !Self::send_chunk(peer, index, slice) {
                Self::send_end(peer, false, "Server failed to build the log bundle. See server log.");
                return;
            }
        }
        Self::send_end(peer, true, &format!("Sent {file_count} log file(s), {} KB compressed.", payload.len() / 1024));
        BNL::log(format!(
            "Streamed log bundle to peer {}: {file_count} files, {} KB raw / {} KB sent.",
            peer.id(),
            raw.len() / 1024,
            payload.len() / 1024
        ));
    }

    /// The container plus how many files it holds; an empty container for none.
    pub fn build_container(logs_dir: &Path, crash_dir: &Path) -> (Vec<u8>, usize) {
        let mut out = Vec::new();
        // Reserve the count slot; backfill once we know how many files were readable.
        out.extend_from_slice(&0i32.to_le_bytes());
        let mut count = 0usize;
        count += Self::add_directory(&mut out, logs_dir, "logs");
        count += Self::add_directory(&mut out, crash_dir, "CrashReports");
        let count_bytes = i32::try_from(count).unwrap_or(i32::MAX).to_le_bytes();
        out[..4].copy_from_slice(&count_bytes);
        if count == 0 {
            return (Vec::new(), 0);
        }
        (out, count)
    }

    fn add_directory(out: &mut Vec<u8>, source_dir: &Path, entry_prefix: &str) -> usize {
        if !source_dir.is_dir() {
            return 0;
        }
        let mut added = 0;
        for file in Self::enumerate_files(source_dir) {
            let relative = file.strip_prefix(source_dir).map(|p| p.to_path_buf()).unwrap_or_else(|_| {
                file.file_name().map(PathBuf::from).unwrap_or_default()
            });
            let relative = relative.to_string_lossy().replace('\\', "/");
            let entry_name = format!("{entry_prefix}/{relative}");
            match std::fs::read(&file) {
                Ok(bytes) => {
                    Self::write_dotnet_string(out, &entry_name);
                    out.extend_from_slice(&i32::try_from(bytes.len()).unwrap_or(i32::MAX).to_le_bytes());
                    out.extend_from_slice(&bytes);
                    added += 1;
                }
                Err(e) => BNL::log_warning(format!("Skipped log file '{entry_name}' while bundling: {e}")),
            }
        }
        added
    }

    /// `BinaryWriter.Write(string)`: a 7-bit-encoded byte length followed by the UTF-8 bytes.
    fn write_dotnet_string(out: &mut Vec<u8>, value: &str) {
        let mut length = value.len();
        while length >= 0x80 {
            out.push((length as u8) | 0x80);
            length >>= 7;
        }
        out.push(length as u8);
        out.extend_from_slice(value.as_bytes());
    }

    /// LZ4 block compression when it actually shrinks the payload; otherwise raw.
    pub fn compress(raw: Vec<u8>) -> (Vec<u8>, bool) {
        let compressed = lz4_flex::block::compress(&raw);
        if !compressed.is_empty() && compressed.len() < raw.len() { (compressed, true) } else { (raw, false) }
    }

    fn send_begin(peer: &NetPeerRef, server_name_safe: &str, file_name: &str, is_compressed: bool, payload_bytes: usize, raw_bytes: usize, total_chunks: usize) -> bool {
        let mut writer = NetDataWriter::new();
        let written = AdminRequest::default().serialize(&mut writer, AdminRequestMode::LogBundleBegin).and_then(|_| {
            writer.put_string(server_name_safe)?;
            writer.put_string(file_name)?;
            writer.put_bool(is_compressed);
            writer.put_int(i32::try_from(payload_bytes).unwrap_or(i32::MAX));
            writer.put_int(i32::try_from(raw_bytes).unwrap_or(i32::MAX));
            writer.put_int(i32::try_from(total_chunks).unwrap_or(i32::MAX));
            Ok(())
        });
        match written {
            Ok(()) => {
                NetworkServer::try_send(peer, &writer, BasisNetworkCommons::ADMIN_CHANNEL, DeliveryMethod::ReliableOrdered);
                true
            }
            Err(e) => {
                BNL::log_error(format!("Failed to build/send log bundle: {e}"));
                false
            }
        }
    }

    fn send_chunk(peer: &NetPeerRef, chunk_index: usize, data: &[u8]) -> bool {
        let mut writer = NetDataWriter::new();
        let written = AdminRequest::default().serialize(&mut writer, AdminRequestMode::LogBundleChunk).and_then(|_| {
            writer.put_int(i32::try_from(chunk_index).unwrap_or(i32::MAX));
            writer.put_bytes_with_length(data)
        });
        match written {
            Ok(()) => {
                NetworkServer::try_send(peer, &writer, BasisNetworkCommons::ADMIN_CHANNEL, DeliveryMethod::ReliableOrdered);
                true
            }
            Err(e) => {
                BNL::log_error(format!("Failed to build/send log bundle: {e}"));
                false
            }
        }
    }

    fn send_end(peer: &NetPeerRef, ok: bool, message: &str) {
        let mut writer = NetDataWriter::new();
        let written = AdminRequest::default().serialize(&mut writer, AdminRequestMode::LogBundleEnd).and_then(|_| {
            writer.put_bool(ok);
            writer.put_string(message)
        });
        if written.is_ok() {
            NetworkServer::try_send(peer, &writer, BasisNetworkCommons::ADMIN_CHANNEL, DeliveryMethod::ReliableOrdered);
        }
    }

    /// A file-name-safe rendering of the server name.
    pub fn sanitize_name(value: &str) -> String {
        if value.trim().is_empty() {
            return "server".to_string();
        }
        let invalid = |c: char| c.is_control() || matches!(c, '"' | '<' | '>' | '|' | ':' | '*' | '?' | '\\' | '/');
        let cleaned: String = value.chars().map(|c| if invalid(c) { '_' } else { c }).collect();
        let cleaned = cleaned.trim().replace(' ', "_");
        if cleaned.is_empty() { "server".to_string() } else { cleaned }
    }
}
