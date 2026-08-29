//! Port of `Reduction/PlayerState.cs`, split by who writes what.
//!
//! The C# class was mutated lock-free from three phases of the tick. Here the sender-side work
//! (`SenderWork`) is locked once per inbound frame, the receiver-side bookkeeping
//! (`ReceiverData`) once per receiver per phase, and what the send loop reads about *other*
//! players is published as an immutable [`SenderFrame`] that a receiver reads without a lock.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use basis_network_core::SerializableBasis::LocalAvatarSyncMessage;
use basis_network_core::mathematics::Vector3;
use basis_network_core::NetPeerRef;
use parking_lot::{Mutex, RwLock};

use super::{PeerTrackingData, PendingAvatarSend};

/// What the send loop needs to know about a sender, frozen at the moment it was published.
#[derive(Debug, Default)]
pub struct SenderFrame {
    /// Generation this frame became (the value `data_generation` holds once published).
    pub generation: i64,
    pub keyframe_gen: i64,
    pub current_is_keyframe: bool,
    pub small_id: bool,
    pub bypass_reduction: bool,
    /// Pre-serialized keyframe per quality. Layout: `[PlayerID:1|2][interval:1][sequence:1][array:N][additional...]`.
    pub serialized_keyframe: [Option<Arc<[u8]>>; 4],
    /// Whether each quality's keyframe carries an additional-data section — the send loop picks
    /// the matching (odd/even) channel per quality.
    pub serialized_has_additional: [bool; 4],
    /// Pre-serialized delta per quality (DeltaAvatarChannel wire), rebuilt each delta tick.
    pub serialized_delta: [Option<Arc<[u8]>>; 4],
}

/// Sender-side state written while an inbound frame is processed.
#[derive(Default)]
pub struct SenderWork {
    /// Cached per-quality payloads. `avatar_high` owns its own buffer.
    pub avatar_high: LocalAvatarSyncMessage,
    pub avatar_medium: LocalAvatarSyncMessage,
    pub avatar_low: LocalAvatarSyncMessage,
    pub avatar_very_low: LocalAvatarSyncMessage,
    /// Actual payload size stored in `avatar_high.array` (used for the muscle-change comparison).
    pub high_array_actual_size: usize,
    /// Inbound sequence tracking for unreliable client→server packets.
    pub last_inbound_sequence: u8,
    pub has_received_first: bool,
    /// Outbound sequence stamped into pre-serialized data (increments per new avatar update).
    pub outbound_sequence: u8,
    pub has_additional_data: bool,
    /// Snapshot of each quality's payload bytes at the last keyframe — the baseline deltas diff against.
    pub keyframe_payload: [Vec<u8>; 4],
    pub keyframe_payload_length: [usize; 4],
    pub keyframe_gen: i64,
    pub keyframe_sequence: u8,
    pub last_keyframe_time_ticks: i64,
    /// Adaptive keyframe stretch: a streak of small High deltas doubles the periodic keyframe
    /// interval step by step, up to the configured maximum.
    pub keyframe_stretch_shift: i32,
    pub small_delta_streak: i32,
    pub current_is_keyframe: bool,
    pub delta_probe_scratch: Vec<u8>,
    /// Scratch the next frame's serialized buffers are built in before being frozen.
    pub serialized_keyframe: [Vec<u8>; 4],
    pub serialized_delta: [Vec<u8>; 4],
    /// The frozen keyframes (kept across delta ticks so a lagging receiver can rebaseline).
    pub keyframe_arcs: [Option<Arc<[u8]>>; 4],
    pub keyframe_has_additional: [bool; 4],
    /// The frozen deltas of the current generation.
    pub delta_arcs: [Option<Arc<[u8]>>; 4],
}

/// Receiver-side state: what this player has been sent, and the per-tick send batch.
#[derive(Default)]
pub struct ReceiverData {
    /// Indexed by sender player id.
    pub peer_tracking: Vec<PeerTrackingData>,
    pub pending_sends: Vec<PendingAvatarSend>,
    pub pending_peak: usize,
    pub pending_peak_ticks: usize,
    pub bundle_raw_scratch: Vec<u8>,
    pub bundle_compressed_scratch: Vec<u8>,
    pub pending_sort_scratch: Vec<PendingAvatarSend>,
    /// EMA of compressed/raw ratio observed for this receiver's LZ4 bundles. 0 = unseeded.
    pub last_bundle_ratio: f32,
    /// Same, for the Zstd path — kept separate because the two codecs sit far apart.
    pub last_bundle_zstd_ratio: f32,
    /// Share of the MTU budget the first compress attempt aims to fill. 0 = unseeded.
    pub bundle_fill_margin: f32,
    /// Flushes remaining before this receiver re-probes whether bundling is worth the CPU.
    pub bundle_skip_countdown: i32,
}

pub struct PlayerState {
    pub id: i32,
    pub peer: RwLock<NetPeerRef>,
    pub is_active: AtomicBool,
    /// Admin-set: bypass the distance reduction system and fan High data to every receiver.
    pub bypass_reduction: AtomicBool,
    position: [AtomicU32; 3],
    /// Incremented each time this player receives new avatar data; receivers compare against
    /// their last-seen generation to know if there is new data.
    pub data_generation: AtomicI64,
    /// Lazy pre-serialization: sticky bitmask of which quality levels had receivers.
    /// Bit 0 = VeryLow, Bit 1 = Low, Bit 2 = Medium, Bit 3 = High.
    pub used_qualities: AtomicI32,
    /// True when the player id fits in a byte (≤255).
    pub small_id: AtomicBool,
    pub frame: ArcSwap<SenderFrame>,
    pub sender: Mutex<SenderWork>,
    pub receiver: Mutex<ReceiverData>,
}

impl PlayerState {
    pub fn new(id: i32, peer: NetPeerRef, position: Vector3, initial_tracking_capacity: usize) -> Self {
        let state = Self {
            id,
            peer: RwLock::new(peer),
            is_active: AtomicBool::new(true),
            bypass_reduction: AtomicBool::new(false),
            position: [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)],
            data_generation: AtomicI64::new(0),
            used_qualities: AtomicI32::new(0),
            small_id: AtomicBool::new(id <= i32::from(u8::MAX)),
            frame: ArcSwap::from_pointee(SenderFrame::default()),
            sender: Mutex::new(SenderWork::default()),
            receiver: Mutex::new(ReceiverData { peer_tracking: vec![PeerTrackingData::default(); initial_tracking_capacity], ..Default::default() }),
        };
        state.set_position(position);
        state
    }

    pub fn peer(&self) -> NetPeerRef {
        self.peer.read().clone()
    }

    pub fn is_active(&self) -> bool {
        self.is_active.load(Ordering::Acquire)
    }

    pub fn bypass_reduction(&self) -> bool {
        self.bypass_reduction.load(Ordering::Relaxed)
    }

    pub fn small_id(&self) -> bool {
        self.small_id.load(Ordering::Relaxed)
    }

    pub fn position(&self) -> Vector3 {
        Vector3 {
            x: f32::from_bits(self.position[0].load(Ordering::Relaxed)),
            y: f32::from_bits(self.position[1].load(Ordering::Relaxed)),
            z: f32::from_bits(self.position[2].load(Ordering::Relaxed)),
        }
    }

    pub fn set_position(&self, position: Vector3) {
        self.position[0].store(position.x.to_bits(), Ordering::Relaxed);
        self.position[1].store(position.y.to_bits(), Ordering::Relaxed);
        self.position[2].store(position.z.to_bits(), Ordering::Relaxed);
    }

    pub fn data_generation(&self) -> i64 {
        self.data_generation.load(Ordering::Acquire)
    }

    /// Sticky quality bits: set, never cleared by the send loop.
    #[inline]
    pub fn mark_quality_used(&self, qi: usize) {
        let bit = 1i32 << qi;
        if self.used_qualities.load(Ordering::Relaxed) & bit != 0 {
            return;
        }
        self.used_qualities.fetch_or(bit, Ordering::Relaxed);
    }

    /// Grows the tracking table so `sender_id` is addressable.
    pub fn ensure_tracking(receiver: &mut ReceiverData, sender_id: usize) {
        if sender_id >= receiver.peer_tracking.len() {
            let new_len = (receiver.peer_tracking.len() * 2).max(sender_id + 1);
            receiver.peer_tracking.resize(new_len, PeerTrackingData::default());
        }
    }
}
