//! Port of `MicrophoneCapture.cs`.
//!
//! The C# captured a real Windows recording device through winmm/waveIn so a load test could
//! carry audio a listener can judge. This build runs on the server host, where there is no
//! waveIn; the API is kept so the driver loop and voice sender read exactly as they did, and
//! `start` reports honestly that capture is unavailable and falls back to synthetic voice.

use std::sync::atomic::{AtomicI64, Ordering};

use basis_network_core::BNL;

static FRAMES_CAPTURED: AtomicI64 = AtomicI64::new(0);
static FRAMES_SPEECH: AtomicI64 = AtomicI64::new(0);

pub struct MicrophoneCapture;

impl MicrophoneCapture {
    pub fn active() -> bool {
        false
    }

    pub fn device_name() -> String {
        String::new()
    }

    pub fn frames_captured() -> i64 {
        FRAMES_CAPTURED.load(Ordering::Relaxed)
    }

    pub fn frames_speech() -> i64 {
        FRAMES_SPEECH.load(Ordering::Relaxed)
    }

    /// Loudest sample seen since the last call, then resets.
    pub fn take_peak() -> f32 {
        0.0
    }

    pub fn list_devices() -> Vec<String> {
        Vec::new()
    }

    pub fn start(_device_match: &str, _frame_ms: i32, _bitrate: i32) -> bool {
        BNL::log_error("[Mic] System microphone capture requires Windows (winmm). Falling back to synthetic voice.");
        false
    }

    /// Reads the next captured frame after `cursor`, advancing it. Never has one here.
    pub fn try_read(_cursor: &mut i64) -> Option<(Vec<u8>, bool)> {
        None
    }

    pub fn newest_frame_index() -> i64 {
        0
    }

    pub fn stop() {}
}
