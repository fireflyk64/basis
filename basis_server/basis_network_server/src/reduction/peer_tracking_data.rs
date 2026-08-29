//! Port of `Reduction/PeerTrackingData.cs`: per-(receiver, sender) send bookkeeping.

/// Indexed by sender id inside each receiver's tracking table. Reset to default on peer removal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerTrackingData {
    pub last_sent_time: i64,
    pub last_seen_generation: i64,
    /// Delta baseline tracking: the sender-keyframe generation this receiver was last sent a
    /// keyframe for. A delta is only sent when this matches the sender's current keyframe;
    /// otherwise the receiver is (re)sent a keyframe first.
    pub baseline_keyframe_gen: i64,
    /// Cached by the slow distance loop, read by the fast send loop. In tick units (µs).
    pub cached_interval_ticks: i32,
    pub cached_quality_index: u8,
    pub cached_interval_byte: u8,
    pub baseline_quality: u8,
}
