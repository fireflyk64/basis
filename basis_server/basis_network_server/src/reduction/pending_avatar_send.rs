//! Port of `Reduction/PendingAvatarSend.cs`: one deferred send in a receiver's per-tick batch.

use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct PendingAvatarSend {
    /// The shared, pre-serialized frame. Patched per receiver at `interval_offset` on the way out.
    pub source: Arc<[u8]>,
    pub length: usize,
    pub channel: u8,
    pub interval: u8,
    /// 1 for byte-id, 2 for ushort-id (delta frames: 2 / 3).
    pub interval_offset: u8,
}
