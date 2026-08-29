//! A process-wide recycling pool for the transport's per-packet buffers.
//!
//! Every datagram-shaped allocation in this codebase has a planable maximum size: the LiteNetLib
//! MTU ladder tops out at [`POOLED_CAPACITY`] bytes ([`NetConstants::MAX_PACKET_SIZE`] plus
//! headroom, and `RECEIVE_BUFFER_BYTES` equals it exactly), and iroh datagrams obey the same
//! path MTU. So instead of a malloc and a free per received datagram, per merged entry and per
//! send, buffers of one fixed capacity circulate: a rent takes one from the pool (or allocates
//! the first time), the [`PooledBytes`] guard returns it on drop, and a buffer converted into
//! [`Bytes`] for delivery comes back when the application drops its last reference.
//!
//! Requests larger than [`POOLED_CAPACITY`] (a reassembled fragment set, a large iroh stream
//! frame) fall back to a plain allocation transparently — the guard then frees instead of
//! recycling — so no caller has to know the threshold.
//!
//! # Bounds
//!
//! The pool is a reservoir, not an owner: it holds at most [`SHARD_COUNT`] ×
//! [`BUFFERS_PER_SHARD`] buffers (16 MB), enforced by the fixed-capacity queues themselves —
//! a recycle into a full shard frees the buffer. It never grows past that and starts empty.
//!
//! # Contention
//!
//! Rents and recycles come from the socket receive tasks, the logic thread, the rayon peer
//! workers and the application threads at once, so the pool has two levels. Each thread keeps
//! up to [`LOCAL_CACHE_MAX`] buffers in a thread-local stack — the steady-state rent and
//! recycle touch no atomics at all — and spills to a reservoir of [`SHARD_COUNT`] lock-free
//! queues ([`ArrayQueue`]) that carries buffers between threads (a receive task rents what an
//! application thread dropped). A dying thread drains its local stack back to the reservoir.
//!
//! # Why a recycled buffer can never still be in use
//!
//! Reuse-while-referenced would hand one customer another's bytes, so the guarantee here has to
//! be the one allocation gave: proof by ownership, not by discipline.
//!
//! * A buffer enters the pool in exactly one place — [`PooledBytes`]'s `Drop` (and the drain of
//!   a dying thread's cache, which owns its `Vec`s). When Rust runs `drop`, it has already
//!   proven no other reference to the buffer exists; that is the same proof that made freeing
//!   safe before pooling. The recycle *is* the drop, routed to a queue instead of the allocator.
//! * Buffers move: `recycle` takes the `Vec` by value, a rent moves it out to exactly one new
//!   owner. Safe Rust cannot construct a second owner of a moved `Vec`'s heap block, and this
//!   module is `#![forbid(unsafe_code)]`, compiler-enforced, so no future edit can weaken that
//!   with a raw pointer or `set_len`.
//! * Delivery shares a buffer through [`Bytes`]' reference count and drops the owner once,
//!   after the last clone — the same refcount the pre-pool code already trusted when it built
//!   `Bytes::from(vec)` for the reader.
//! * The residual risk class is not aliasing but *stale contents*: an API that returned a
//!   buffer still holding a previous packet's bytes would leak them if a caller failed to
//!   overwrite everything. No such API exists: every rent either zeroes what it returns
//!   ([`rent_zeroed`](PacketBufferPool::rent_zeroed),
//!   [`rent_frame`](PacketBufferPool::rent_frame)'s prefix) or copies the caller's bytes over
//!   the full length ([`rent_copy`](PacketBufferPool::rent_copy)) — closed by construction, and
//!   held down by the dirty-recycle tests below.
//!
//! What is trusted: rustc and `Vec` (as before), `bytes`' refcount (as before), and
//! `crossbeam`'s `ArrayQueue`, whose push/pop also move values under Rust's types.
//!
//! [`NetConstants::MAX_PACKET_SIZE`]: crate::transport::lnl_network_impl::NetConstants::MAX_PACKET_SIZE

#![forbid(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use bytes::Bytes;
use crossbeam_queue::ArrayQueue;

/// Capacity of every pooled buffer. Covers the largest UDP datagram either transport reads or
/// writes (the MTU ladder's 1432-byte ceiling plus header, and the 2048-byte receive buffer).
pub const POOLED_CAPACITY: usize = 2048;
/// Lock-free queues the reservoir is split across to keep rent/recycle uncontended.
pub const SHARD_COUNT: usize = 8;
/// Buffers one shard holds; with [`SHARD_COUNT`] and [`POOLED_CAPACITY`] this caps the
/// reservoir at 16 MB.
pub const BUFFERS_PER_SHARD: usize = 1024;
/// Buffers one thread may keep in its local stack (64 KB per thread): the no-atomics fast path.
pub const LOCAL_CACHE_MAX: usize = 32;

/// Counters describing what the pool has done since the process started. Monotonic; read them
/// as deltas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PacketPoolStats {
    /// Rents served from a thread-local stack (no allocation, no atomics). Folded in from the
    /// per-thread counters every [`LocalCache::FLUSH_EVERY`]-ish operations, so this trails
    /// live activity slightly.
    pub reused_local: u64,
    /// Rents served from the shared reservoir (no allocation).
    pub reused: u64,
    /// Rents that allocated because every shard was empty.
    pub allocated: u64,
    /// Rents larger than [`POOLED_CAPACITY`], served by a plain allocation.
    pub oversize: u64,
    /// Buffers kept on a drop — into the local stack or the reservoir. (A dying thread's
    /// drain moves its buffers into the reservoir and counts them a second time.)
    pub recycled: u64,
    /// Buffers freed because their shard was full (the reservoir bound at work).
    pub dropped_full: u64,
}

pub(crate) struct BufferPoolInner {
    shards: Vec<ArrayQueue<Vec<u8>>>,
    reused_local: AtomicU64,
    reused: AtomicU64,
    allocated: AtomicU64,
    oversize: AtomicU64,
    recycled: AtomicU64,
    dropped_full: AtomicU64,
}

impl BufferPoolInner {
    pub(crate) fn new() -> Self {
        Self {
            shards: (0..SHARD_COUNT).map(|_| ArrayQueue::new(BUFFERS_PER_SHARD)).collect(),
            reused_local: AtomicU64::new(0),
            reused: AtomicU64::new(0),
            allocated: AtomicU64::new(0),
            oversize: AtomicU64::new(0),
            recycled: AtomicU64::new(0),
            dropped_full: AtomicU64::new(0),
        }
    }

    /// Takes from this thread's local stack — the fast path, no atomics. `None` when the
    /// stack is empty, this pool is not the process pool (tests), or the thread is shutting
    /// down; the caller then goes to the reservoir.
    fn take_local(&'static self) -> Option<Vec<u8>> {
        if !std::ptr::eq(self, &*GLOBAL) {
            return None;
        }
        LOCAL_CACHE
            .try_with(|cache| {
                let mut cache = cache.try_borrow_mut().ok()?;
                let taken = cache.buffers.pop();
                if taken.is_some() {
                    // Counted thread-locally and folded into the shared stats periodically, so
                    // two busy cores do not fight over one counter cache line per packet.
                    cache.reused += 1;
                    if cache.reused >= LocalCache::FLUSH_EVERY {
                        GLOBAL.reused_local.fetch_add(cache.reused, Ordering::Relaxed);
                        cache.reused = 0;
                    }
                }
                taken
            })
            .ok()
            .flatten()
    }

    /// Puts a buffer on this thread's local stack; hands it back when the stack is full (or on
    /// a test pool / dying thread) so the caller spills it to the reservoir.
    fn store_local(&'static self, buffer: Vec<u8>) -> Option<Vec<u8>> {
        if !std::ptr::eq(self, &*GLOBAL) {
            return Some(buffer);
        }
        let mut carried = Some(buffer);
        let outcome = LOCAL_CACHE.try_with(|cache| {
            let Ok(mut cache) = cache.try_borrow_mut() else {
                return;
            };
            if cache.buffers.len() < LOCAL_CACHE_MAX
                && let Some(buffer) = carried.take()
            {
                cache.buffers.push(buffer);
                cache.kept += 1;
                if cache.kept >= LocalCache::FLUSH_EVERY {
                    GLOBAL.recycled.fetch_add(cache.kept, Ordering::Relaxed);
                    cache.kept = 0;
                }
            }
        });
        // On Err the thread-local is already destroyed (thread exit) and the closure never ran:
        // `carried` still holds the buffer and the caller spills it to the reservoir.
        let _ = outcome;
        carried
    }

    fn home_shard(&self) -> usize {
        thread_local! {
            static HOME: Cell<usize> = const { Cell::new(usize::MAX) };
        }
        static NEXT_HOME: AtomicUsize = AtomicUsize::new(0);
        HOME.with(|home| {
            let mut index = home.get();
            if index == usize::MAX {
                index = NEXT_HOME.fetch_add(1, Ordering::Relaxed) % SHARD_COUNT;
                home.set(index);
            }
            index
        })
    }

    /// A buffer from the local stack or the reservoir — contents are whatever the previous
    /// user left, length preserved — or `None` when both are empty.
    fn take_buffer(&'static self) -> Option<Vec<u8>> {
        if let Some(buffer) = self.take_local() {
            return Some(buffer);
        }
        let home = self.home_shard();
        for offset in 0..self.shards.len() {
            let index = (home + offset) % self.shards.len();
            if let Some(shard) = self.shards.get(index)
                && let Some(buffer) = shard.pop()
            {
                self.reused.fetch_add(1, Ordering::Relaxed);
                return Some(buffer);
            }
        }
        None
    }

    fn fresh(&self) -> Vec<u8> {
        self.allocated.fetch_add(1, Ordering::Relaxed);
        Vec::with_capacity(POOLED_CAPACITY)
    }

    fn recycle(&'static self, buffer: Vec<u8>) {
        // Only buffers still at the pool's exact capacity go back; anything that was grown (it
        // cannot be through [`PooledBytes`], but stay safe) is freed so the reservoir can never
        // hold more than its stated bytes.
        if buffer.capacity() != POOLED_CAPACITY {
            return;
        }
        let Some(buffer) = self.store_local(buffer) else {
            return; // kept in the local stack; counted there and flushed periodically
        };
        self.recycle_to_shard(buffer);
    }

    /// Puts a buffer into the sharded reservoir directly (the spill path, and the drain a
    /// dying thread runs).
    fn recycle_to_shard(&self, buffer: Vec<u8>) {
        let home = self.home_shard();
        match self.shards.get(home) {
            Some(shard) => match shard.push(buffer) {
                Ok(()) => {
                    self.recycled.fetch_add(1, Ordering::Relaxed);
                }
                Err(_full) => {
                    self.dropped_full.fetch_add(1, Ordering::Relaxed);
                }
            },
            None => {
                self.dropped_full.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// A pooled copy of `source` (or a plain allocation past [`POOLED_CAPACITY`]). No byte is
    /// written twice: the recycled buffer is cleared (a length change, not a wipe) and the
    /// source copied in.
    pub(crate) fn rent_copy(&'static self, source: &[u8]) -> PooledBytes {
        if source.len() > POOLED_CAPACITY {
            self.oversize.fetch_add(1, Ordering::Relaxed);
            return PooledBytes { data: source.to_vec(), pool: None };
        }
        let mut data = self.take_buffer().unwrap_or_else(|| self.fresh());
        data.clear();
        data.extend_from_slice(source);
        PooledBytes { data, pool: Some(self) }
    }

    /// A pooled buffer of `len` zero bytes — the [`NetPacket::with_size`] contract, so header
    /// fields assembled with or-masks start from zero and padding never carries a previous
    /// packet's bytes onto the wire.
    ///
    /// [`NetPacket::with_size`]: crate::transport::lnl_network_impl::NetPacket::with_size
    pub(crate) fn rent_zeroed(&'static self, len: usize) -> PooledBytes {
        if len > POOLED_CAPACITY {
            self.oversize.fetch_add(1, Ordering::Relaxed);
            return PooledBytes { data: vec![0; len], pool: None };
        }
        let mut data = self.take_buffer().unwrap_or_else(|| self.fresh());
        data.clear();
        data.resize(len, 0);
        PooledBytes { data, pool: Some(self) }
    }

    /// A pooled buffer laid out as `prefix` zero bytes followed by `payload` — the shape of
    /// every outgoing packet (a zeroed header the caller stamps, then the user bytes). Only the
    /// prefix is memset; the payload is copied once.
    pub(crate) fn rent_frame(&'static self, prefix: usize, payload: &[u8]) -> PooledBytes {
        let total = prefix.saturating_add(payload.len());
        if total > POOLED_CAPACITY {
            self.oversize.fetch_add(1, Ordering::Relaxed);
            let mut data = Vec::with_capacity(total);
            data.resize(prefix, 0);
            data.extend_from_slice(payload);
            return PooledBytes { data, pool: None };
        }
        let mut data = self.take_buffer().unwrap_or_else(|| self.fresh());
        data.clear();
        data.resize(prefix, 0);
        data.extend_from_slice(payload);
        PooledBytes { data, pool: Some(self) }
    }

    pub(crate) fn stats(&self) -> PacketPoolStats {
        PacketPoolStats {
            reused_local: self.reused_local.load(Ordering::Relaxed),
            reused: self.reused.load(Ordering::Relaxed),
            allocated: self.allocated.load(Ordering::Relaxed),
            oversize: self.oversize.load(Ordering::Relaxed),
            recycled: self.recycled.load(Ordering::Relaxed),
            dropped_full: self.dropped_full.load(Ordering::Relaxed),
        }
    }

    /// Buffers currently resting in the reservoir (approximate under concurrency).
    pub(crate) fn pooled_buffers(&self) -> usize {
        self.shards.iter().map(ArrayQueue::len).sum()
    }
}

static GLOBAL: LazyLock<BufferPoolInner> = LazyLock::new(BufferPoolInner::new);

/// This thread's stack of the process pool's buffers, with the thread's not-yet-flushed share
/// of the pool statistics. Dropped when the thread exits, which drains every held buffer back
/// to the shared reservoir (so short-lived threads leak nothing) and folds the counters in.
struct LocalCache {
    buffers: Vec<Vec<u8>>,
    /// Local-stack rents not yet folded into [`PacketPoolStats::reused_local`].
    reused: u64,
    /// Local-stack keeps not yet folded into [`PacketPoolStats::recycled`].
    kept: u64,
}

impl LocalCache {
    /// How much thread-local activity may accumulate before it is folded into the shared
    /// counters; `stats()` therefore trails the busiest threads by at most this much.
    const FLUSH_EVERY: u64 = 1024;
}

impl Drop for LocalCache {
    fn drop(&mut self) {
        for buffer in self.buffers.drain(..) {
            GLOBAL.recycle_to_shard(buffer);
        }
        GLOBAL.reused_local.fetch_add(self.reused, Ordering::Relaxed);
        GLOBAL.recycled.fetch_add(self.kept, Ordering::Relaxed);
    }
}

thread_local! {
    static LOCAL_CACHE: RefCell<LocalCache> = RefCell::new(LocalCache { buffers: Vec::with_capacity(LOCAL_CACHE_MAX), reused: 0, kept: 0 });
}

/// The process-wide packet buffer pool. See the module documentation.
pub struct PacketBufferPool;

impl PacketBufferPool {
    /// A pooled copy of `source`. See [`BufferPoolInner::rent_copy`].
    pub fn rent_copy(source: &[u8]) -> PooledBytes {
        GLOBAL.rent_copy(source)
    }

    /// `len` zero bytes. See [`BufferPoolInner::rent_zeroed`].
    pub fn rent_zeroed(len: usize) -> PooledBytes {
        GLOBAL.rent_zeroed(len)
    }

    /// `prefix` zero bytes then `payload`. See [`BufferPoolInner::rent_frame`].
    pub fn rent_frame(prefix: usize, payload: &[u8]) -> PooledBytes {
        GLOBAL.rent_frame(prefix, payload)
    }

    /// Lifetime counters, for the health document and the pooling benchmarks.
    pub fn stats() -> PacketPoolStats {
        GLOBAL.stats()
    }

    /// Buffers currently resting in the reservoir.
    pub fn pooled_buffers() -> usize {
        GLOBAL.pooled_buffers()
    }
}

/// An owned byte buffer that goes back to its pool when dropped (or was too large to pool, and
/// is simply freed). Dereferences to `[u8]`; converting it [`Into<Bytes>`] keeps the recycling:
/// the buffer returns to the pool when the last `Bytes` clone is dropped.
pub struct PooledBytes {
    data: Vec<u8>,
    pool: Option<&'static BufferPoolInner>,
}

impl PooledBytes {
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Shortens the buffer to `len` bytes (never grows it).
    pub fn truncate(&mut self, len: usize) {
        self.data.truncate(len);
    }

    /// Whether this buffer will return to a pool on drop (false for oversize fallbacks and
    /// buffers adopted from a plain `Vec`).
    pub fn is_pooled(&self) -> bool {
        self.pool.is_some()
    }

    /// The bytes, unpooled: the buffer is removed from the recycling cycle and behaves as a
    /// plain allocation from here on.
    pub fn into_vec(mut self) -> Vec<u8> {
        self.pool = None;
        std::mem::take(&mut self.data)
    }
}

impl Drop for PooledBytes {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.take() {
            pool.recycle(std::mem::take(&mut self.data));
        }
    }
}

impl std::ops::Deref for PooledBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.data
    }
}

impl std::ops::DerefMut for PooledBytes {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl AsRef<[u8]> for PooledBytes {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

impl std::fmt::Debug for PooledBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledBytes").field("len", &self.data.len()).field("pooled", &self.is_pooled()).finish()
    }
}

impl Clone for PooledBytes {
    /// A pooled copy (falling back exactly as a rent does).
    fn clone(&self) -> Self {
        match self.pool {
            Some(pool) => pool.rent_copy(&self.data),
            None => PacketBufferPool::rent_copy(&self.data),
        }
    }
}

impl PartialEq for PooledBytes {
    fn eq(&self, other: &Self) -> bool {
        self.data == other.data
    }
}

impl Eq for PooledBytes {}

/// Adopts a plain `Vec` (unpooled; dropping it frees as usual). Keeps every existing
/// `Vec`-built call site compiling and lets tests construct exact buffers.
impl From<Vec<u8>> for PooledBytes {
    fn from(data: Vec<u8>) -> Self {
        Self { data, pool: None }
    }
}

/// Zero-copy: the `Bytes` borrows the pooled buffer and the buffer returns to the pool when the
/// last clone is dropped. This is how a received packet reaches the application without taking
/// the buffer out of circulation.
impl From<PooledBytes> for Bytes {
    fn from(buffer: PooledBytes) -> Self {
        if buffer.pool.is_some() { Bytes::from_owner(buffer) } else { Bytes::from(buffer.into_vec()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private pool per test: assertions on counters and reservoir contents cannot race the
    /// rest of the test binary, which shares the global pool.
    fn pool() -> &'static BufferPoolInner {
        Box::leak(Box::new(BufferPoolInner::new()))
    }

    #[test]
    fn a_dropped_buffer_is_reused_by_the_next_rent() {
        let pool = pool();
        let first = pool.rent_copy(b"hello");
        assert!(first.is_pooled());
        drop(first);
        assert_eq!(pool.pooled_buffers(), 1);
        let second = pool.rent_copy(b"world");
        assert_eq!(&second[..], b"world");
        let stats = pool.stats();
        assert_eq!((stats.allocated, stats.recycled, stats.reused), (1, 1, 1));
        assert_eq!(pool.pooled_buffers(), 0);
    }

    #[test]
    fn rent_zeroed_is_zero_even_after_a_dirty_previous_user() {
        let pool = pool();
        // Leave a buffer full of a "previous peer's" bytes in the reservoir.
        drop(pool.rent_copy(&[0xAB; POOLED_CAPACITY]));
        let clean = pool.rent_zeroed(64);
        assert_eq!(pool.stats().reused, 1, "the dirty buffer really was reused");
        assert!(clean.iter().all(|b| *b == 0), "with_size packets must never leak recycled bytes");
    }

    #[test]
    fn rent_frame_zeroes_the_prefix_and_copies_the_payload() {
        let pool = pool();
        drop(pool.rent_copy(&[0xCD; POOLED_CAPACITY]));
        let frame = pool.rent_frame(10, b"payload");
        assert_eq!(&frame[..10], &[0u8; 10]);
        assert_eq!(&frame[10..], b"payload");
        assert_eq!(frame.len(), 17);
    }

    #[test]
    fn every_rent_is_zeroed_or_fully_copied_never_stale() {
        // The disclosure-by-construction check: whatever a previous user left behind, no rent
        // API can return a byte it did not zero or copy itself.
        let pool = pool();
        for _ in 0..4 {
            drop(pool.rent_copy(&[0xEE; POOLED_CAPACITY]));
            let zeroed = pool.rent_zeroed(512);
            assert!(zeroed.iter().all(|b| *b == 0));
            drop(zeroed);
            let frame = pool.rent_frame(12, &[0x11; 300]);
            assert!(frame[..12].iter().all(|b| *b == 0));
            assert!(frame[12..].iter().all(|b| *b == 0x11));
            drop(frame);
            let copied = pool.rent_copy(&[0x22; 64]);
            assert!(copied.iter().all(|b| *b == 0x22));
        }
    }

    #[test]
    fn oversize_requests_fall_back_to_plain_allocations() {
        let pool = pool();
        let big = pool.rent_zeroed(POOLED_CAPACITY + 1);
        assert!(!big.is_pooled());
        assert_eq!(big.len(), POOLED_CAPACITY + 1);
        drop(big);
        let big = pool.rent_copy(&vec![9u8; POOLED_CAPACITY * 2]);
        assert!(!big.is_pooled());
        drop(big);
        let big = pool.rent_frame(POOLED_CAPACITY, &[1, 2, 3]);
        assert!(!big.is_pooled());
        assert_eq!(pool.stats().oversize, 3);
        assert_eq!(pool.pooled_buffers(), 0, "oversize buffers never enter the reservoir");
    }

    #[test]
    fn the_reservoir_is_bounded_and_overflow_is_freed() {
        let pool = pool();
        let cap = SHARD_COUNT * BUFFERS_PER_SHARD;
        // One thread recycles only into its home shard, so from a single thread the reachable
        // bound is one shard's worth.
        let held: Vec<PooledBytes> = (0..BUFFERS_PER_SHARD + 50).map(|_| pool.rent_zeroed(8)).collect();
        drop(held);
        assert_eq!(pool.pooled_buffers(), BUFFERS_PER_SHARD);
        assert!(pool.pooled_buffers() <= cap);
        let stats = pool.stats();
        assert_eq!(stats.dropped_full, 50);
        assert_eq!(stats.recycled, BUFFERS_PER_SHARD as u64);
    }

    #[test]
    fn into_vec_removes_a_buffer_from_the_cycle() {
        let pool = pool();
        let rented = pool.rent_copy(b"escape");
        let vec = rented.into_vec();
        assert_eq!(vec, b"escape");
        drop(vec);
        assert_eq!(pool.pooled_buffers(), 0);
        assert_eq!(pool.stats().recycled, 0);
    }

    #[test]
    fn bytes_conversion_recycles_when_the_last_clone_drops() {
        let pool = pool();
        let rented = pool.rent_copy(b"shared delivery");
        let shared: Bytes = rented.into();
        let second = shared.clone();
        assert_eq!(&second[..], b"shared delivery");
        drop(shared);
        assert_eq!(pool.pooled_buffers(), 0, "a live clone must keep the buffer out");
        drop(second);
        assert_eq!(pool.pooled_buffers(), 1, "the last clone returns the buffer");
        assert_eq!(pool.stats().recycled, 1);
    }

    #[test]
    fn adopted_vecs_and_clones_behave() {
        let pool = pool();
        let adopted = PooledBytes::from(vec![1, 2, 3]);
        assert!(!adopted.is_pooled());
        let cloned = pool.rent_copy(b"abc").clone();
        assert_eq!(&cloned[..], b"abc");
        assert_eq!(adopted, PooledBytes::from(vec![1, 2, 3]));
        let shared: Bytes = adopted.into();
        assert_eq!(&shared[..], &[1, 2, 3]);
    }

    #[test]
    fn the_process_pool_reuses_through_the_thread_local_stack() {
        // Pointer identity proves the fast path: on one thread, a dropped buffer is the next
        // rent, no matter what other tests do to the process pool concurrently.
        let outcome = std::thread::spawn(|| {
            let first = PacketBufferPool::rent_copy(b"warm");
            let pointer = first.as_ptr();
            drop(first);
            let second = PacketBufferPool::rent_copy(b"warm again");
            pointer == second.as_ptr()
        })
        .join();
        assert_eq!(outcome.ok(), Some(true));
    }

    #[test]
    fn a_dying_thread_drains_its_local_stack_to_the_reservoir() {
        let before = PacketBufferPool::stats();
        let held = std::thread::spawn(|| {
            let held: Vec<_> = (0..5).map(|_| PacketBufferPool::rent_zeroed(16)).collect();
            drop(held);
        })
        .join();
        assert!(held.is_ok());
        // The five buffers entered the thread's local stack and then, on thread exit, the
        // reservoir; both arrivals count, and other tests only ever add.
        let after = PacketBufferPool::stats();
        assert!(after.recycled >= before.recycled + 10, "local stores then the drain: {} -> {}", before.recycled, after.recycled);
    }

    #[test]
    fn concurrent_rent_and_recycle_stay_consistent() {
        let pool = pool();
        let threads: Vec<_> = (0..4)
            .map(|t| {
                std::thread::spawn(move || {
                    for i in 0..5_000usize {
                        let payload = [(t as u8) ^ (i as u8); 96];
                        let buffer = pool.rent_copy(&payload);
                        // Widen any aliasing window before verifying: if a recycled buffer
                        // could ever be handed to two owners, another thread's tag would
                        // appear under ours.
                        std::thread::yield_now();
                        assert_eq!(&buffer[..], &payload[..]);
                        if i % 3 == 0 {
                            let shared: Bytes = buffer.into();
                            std::thread::yield_now();
                            assert_eq!(&shared[..], &payload[..]);
                        }
                    }
                })
            })
            .collect();
        for thread in threads {
            if thread.join().is_err() {
                unreachable!("pool churn thread panicked");
            }
        }
        let stats = pool.stats();
        assert_eq!(stats.reused + stats.allocated, 4 * 5_000);
        assert!(pool.pooled_buffers() <= SHARD_COUNT * BUFFERS_PER_SHARD);
    }
}
