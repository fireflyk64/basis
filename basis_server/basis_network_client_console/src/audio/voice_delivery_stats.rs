//! Port of `VoiceDeliveryStats.cs`: measures what a receiver actually HEARS, which is the only
//! honest way to judge the voice path. Every simulated voice frame carries a per-sender sequence
//! byte; tracking it per (receiver, sender) pair turns the stream into a loss measurement.
//! Sequence is a single byte at 50 frames/s, so it wraps every ~5.1 s — all arithmetic is done
//! in byte space for that reason.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use parking_lot::Mutex;

/// A delta above this is read as reorder/duplicate rather than a very large gap. Server to client
/// is unreliable, so late arrivals are expected and must not be counted as loss.
const REORDER_THRESHOLD: i32 = 128;

static ENABLED: AtomicBool = AtomicBool::new(false);
static RECEIVED: AtomicI64 = AtomicI64::new(0);
static LOST: AtomicI64 = AtomicI64::new(0);
static REORDERED: AtomicI64 = AtomicI64::new(0);
static STREAMS: AtomicI64 = AtomicI64::new(0);
/// Last sequence seen per (receiver, sender).
static LAST_SEQ: Mutex<Option<HashMap<u64, u8>>> = Mutex::new(None);

pub struct VoiceDeliveryStats;

impl VoiceDeliveryStats {
    pub fn enabled() -> bool {
        ENABLED.load(Ordering::Relaxed)
    }

    pub fn set_enabled(value: bool) {
        ENABLED.store(value, Ordering::Relaxed);
    }

    pub fn received() -> i64 {
        RECEIVED.load(Ordering::Relaxed)
    }

    pub fn lost() -> i64 {
        LOST.load(Ordering::Relaxed)
    }

    pub fn reordered() -> i64 {
        REORDERED.load(Ordering::Relaxed)
    }

    pub fn streams() -> i64 {
        STREAMS.load(Ordering::Relaxed)
    }

    pub fn reset() {
        let mut map = LAST_SEQ.lock();
        if let Some(map) = map.as_mut() {
            map.clear();
        }
        RECEIVED.store(0, Ordering::Relaxed);
        LOST.store(0, Ordering::Relaxed);
        REORDERED.store(0, Ordering::Relaxed);
        STREAMS.store(0, Ordering::Relaxed);
    }

    /// Records one received voice frame. `sender_id` and `sequence` come straight off the wire.
    pub fn note(receiver_index: usize, sender_id: i32, sequence: u8) {
        if !Self::enabled() {
            return;
        }
        RECEIVED.fetch_add(1, Ordering::Relaxed);
        let key = ((receiver_index as u64) << 32) | (sender_id as u32 as u64);
        let mut guard = LAST_SEQ.lock();
        let map = guard.get_or_insert_with(HashMap::new);
        let Some(last) = map.get(&key).copied() else {
            // First frame of a stream establishes the baseline. Counting the distance from zero
            // here would charge every talker's first packet as a burst of loss.
            map.insert(key, sequence);
            STREAMS.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let delta = sequence.wrapping_sub(last) as i32;
        if delta == 0 || delta > REORDER_THRESHOLD {
            REORDERED.fetch_add(1, Ordering::Relaxed);
            return; // do not move the baseline backwards
        }
        if delta > 1 {
            LOST.fetch_add((delta - 1) as i64, Ordering::Relaxed);
        }
        map.insert(key, sequence);
    }

    /// Delivered share, 0..1 — the number that answers "is voice breaking up".
    pub fn delivered_fraction() -> f64 {
        let (recv, lost) = (Self::received(), Self::lost());
        let produced = recv + lost;
        if produced > 0 { recv as f64 / produced as f64 } else { 0.0 }
    }

    pub fn describe() -> String {
        format!("[VOICE] delivered {:.2}% | received={} lost={} reordered={} streams={}", Self::delivered_fraction() * 100.0, Self::received(), Self::lost(), Self::reordered(), Self::streams())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_gaps_wraps_and_reorders() {
        VoiceDeliveryStats::reset();
        VoiceDeliveryStats::set_enabled(true);
        VoiceDeliveryStats::note(0, 7, 250);
        VoiceDeliveryStats::note(0, 7, 251);
        VoiceDeliveryStats::note(0, 7, 254); // 252, 253 lost
        VoiceDeliveryStats::note(0, 7, 1); // wraps: 255, 0 lost
        VoiceDeliveryStats::note(0, 7, 0); // late arrival: reordered, baseline stays
        VoiceDeliveryStats::note(0, 7, 1); // duplicate
        assert_eq!(VoiceDeliveryStats::streams(), 1);
        assert_eq!(VoiceDeliveryStats::lost(), 4);
        assert_eq!(VoiceDeliveryStats::reordered(), 2);
        assert_eq!(VoiceDeliveryStats::received(), 6);
        assert!((VoiceDeliveryStats::delivered_fraction() - 0.6).abs() < 1e-9);
        VoiceDeliveryStats::set_enabled(false);
        VoiceDeliveryStats::reset();
    }
}
