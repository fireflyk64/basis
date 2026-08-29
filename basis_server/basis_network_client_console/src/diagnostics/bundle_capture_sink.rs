//! Port of `BundleCaptureSink.cs`: harvests real avatar-bundle bodies off the wire into a capture
//! file, which the bundle dictionary trainer turns into the Zstd dictionary.
//!
//! Captured here and not in the server: the sniffer already holds the decompressed grouped body,
//! byte-for-byte what the server compressed, so training material comes with no hook in the
//! server's send path. Capture is decimated (`every_nth`) rather than "first N bundles", so the
//! sample is spread across the whole run instead of the join burst.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use basis_network_core::compression::BasisAvatarBundleCodec;
use parking_lot::Mutex;

/// File magic + format version. Bump the digit if the record layout changes.
const MAGIC: &[u8; 8] = b"BSNDCAP1";
/// Record flag: this body contained only DeltaAvatarChannel groups.
pub const FLAG_DELTA_ONLY: u8 = 1;

struct Capture {
    file: BufWriter<File>,
    every_nth: i64,
    max_samples: i32,
    written: i32,
    written_delta: i32,
    raw_bytes: i64,
    wire_bytes: i64,
}

static GATE: Mutex<Option<Capture>> = Mutex::new(None);
static SEEN: AtomicI64 = AtomicI64::new(0);
static ENABLED: AtomicBool = AtomicBool::new(false);

pub struct BundleCaptureSink;

impl BundleCaptureSink {
    /// True once `configure` has opened a capture file.
    pub fn enabled() -> bool {
        ENABLED.load(Ordering::Relaxed)
    }

    /// Opens `path` for capture. `max_samples` bounds the file so an overnight run cannot fill
    /// the disk; `every_nth` decimates.
    pub fn configure(path: &str, max_samples: i32, every_nth: i32) -> std::io::Result<()> {
        let mut gate = GATE.lock();
        if gate.is_some() {
            return Ok(());
        }
        if let Some(dir) = std::path::Path::new(path).parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = BufWriter::with_capacity(1 << 16, File::create(path)?);
        file.write_all(MAGIC)?;
        *gate = Some(Capture { file, every_nth: every_nth.max(1) as i64, max_samples: max_samples.max(1), written: 0, written_delta: 0, raw_bytes: 0, wire_bytes: 0 });
        ENABLED.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Records one decoded bundle body. `compressed_len` and `codec` are stored only in the
    /// summary — the file holds raw bodies, so a capture taken with one codec can train a
    /// dictionary used by another.
    pub fn capture(grouped: &[u8], compressed_len: usize, _codec: u8) {
        if !Self::enabled() || grouped.is_empty() || grouped.len() > u16::MAX as usize {
            return;
        }
        // Decimate before taking the lock so the common case is one atomic increment.
        let every = GATE.lock().as_ref().map(|c| c.every_nth).unwrap_or(1);
        if (SEEN.fetch_add(1, Ordering::Relaxed) + 1) % every != 0 {
            return;
        }
        let Some(delta_only) = BasisAvatarBundleCodec::try_classify(grouped) else {
            return;
        };
        let mut gate = GATE.lock();
        let Some(capture) = gate.as_mut() else { return };
        if capture.written >= capture.max_samples {
            return;
        }
        let length = grouped.len();
        let header = [if delta_only { FLAG_DELTA_ONLY } else { 0 }, (length & 0xFF) as u8, ((length >> 8) & 0xFF) as u8];
        if capture.file.write_all(&header).is_err() || capture.file.write_all(grouped).is_err() {
            return;
        }
        capture.written += 1;
        if delta_only {
            capture.written_delta += 1;
        }
        capture.raw_bytes += length as i64;
        capture.wire_bytes += compressed_len as i64;
    }

    /// Closes the capture file and returns a one-line summary, or None if capture was off.
    pub fn finish() -> Option<String> {
        let mut gate = GATE.lock();
        let mut capture = gate.take()?;
        let _ = capture.file.flush();
        ENABLED.store(false, Ordering::Relaxed);
        let keyframe = capture.written - capture.written_delta;
        let ratio = if capture.raw_bytes > 0 { capture.wire_bytes as f64 / capture.raw_bytes as f64 } else { 0.0 };
        Some(format!(
            "[BundleCapture] {} samples ({keyframe} keyframe/full, {} delta-only) from {} bundles, {} raw bytes, observed wire ratio {ratio:.4}.",
            capture.written,
            capture.written_delta,
            SEEN.load(Ordering::Relaxed),
            capture.raw_bytes
        ))
    }
}
