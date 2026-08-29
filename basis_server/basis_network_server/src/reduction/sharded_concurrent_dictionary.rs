//! Port of `Reduction/ShardedConcurrentDictionary.cs`: an int-keyed map split across
//! power-of-two shards, with a drain operation the tick uses to take everything queued.

use std::collections::HashMap;

use parking_lot::Mutex;

pub struct ShardedConcurrentDictionary<V> {
    shards: Vec<Mutex<HashMap<i32, V>>>,
    mask: usize,
}

impl<V> Default for ShardedConcurrentDictionary<V> {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        Self::with_shards(cores.max(1).next_power_of_two())
    }
}

impl<V> ShardedConcurrentDictionary<V> {
    /// `shard_count` is rounded up to a power of two (the C# refused non-powers; rounding keeps
    /// the caller's intent without a failure path).
    pub fn with_shards(shard_count: usize) -> Self {
        let shard_count = shard_count.max(1).next_power_of_two();
        Self { shards: (0..shard_count).map(|_| Mutex::new(HashMap::new())).collect(), mask: shard_count - 1 }
    }

    /// 32-bit integer hash mix (Murmur3-style). Player ids are dense small ints; without
    /// scrambling, ids 0..N-1 would all hash to shard 0 under low-bit masking.
    #[inline]
    fn scramble(key: i32) -> u32 {
        let mut x = key as u32;
        x ^= x >> 16;
        x = x.wrapping_mul(0x7feb_352d);
        x ^= x >> 15;
        x = x.wrapping_mul(0x846c_a68b);
        x ^= x >> 16;
        x
    }

    #[inline]
    fn shard_of(&self, key: i32) -> &Mutex<HashMap<i32, V>> {
        &self.shards[(Self::scramble(key) as usize) & self.mask]
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    pub fn get_cloned(&self, key: i32) -> Option<V>
    where
        V: Clone,
    {
        self.shard_of(key).lock().get(&key).cloned()
    }

    pub fn contains_key(&self, key: i32) -> bool {
        self.shard_of(key).lock().contains_key(&key)
    }

    pub fn insert(&self, key: i32, value: V) -> Option<V> {
        self.shard_of(key).lock().insert(key, value)
    }

    pub fn remove(&self, key: i32) -> Option<V> {
        self.shard_of(key).lock().remove(&key)
    }

    pub fn clear(&self) {
        for shard in &self.shards {
            shard.lock().clear();
        }
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lock().len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.lock().is_empty())
    }

    /// Takes everything currently held into `destination`, one shard lock at a time. Anything
    /// written mid-drain lands on the next call.
    pub fn drain_into(&self, destination: &mut Vec<V>) {
        for shard in &self.shards {
            let mut shard = shard.lock();
            if shard.is_empty() {
                continue;
            }
            destination.extend(shard.drain().map(|(_, v)| v));
        }
    }

    /// A snapshot of every `(key, value)` pair.
    pub fn entries(&self) -> Vec<(i32, V)>
    where
        V: Clone,
    {
        let mut out = Vec::new();
        for shard in &self.shards {
            let shard = shard.lock();
            out.extend(shard.iter().map(|(k, v)| (*k, v.clone())));
        }
        out
    }

    /// Runs `f` on every value under the shard lock.
    pub fn for_each(&self, mut f: impl FnMut(i32, &V)) {
        for shard in &self.shards {
            let shard = shard.lock();
            for (k, v) in shard.iter() {
                f(*k, v);
            }
        }
    }
}
