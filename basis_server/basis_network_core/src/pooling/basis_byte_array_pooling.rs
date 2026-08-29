use std::collections::{HashMap, VecDeque};

use parking_lot::Mutex;

static POOL: Mutex<Option<HashMap<usize, VecDeque<Vec<u8>>>>> = Mutex::new(None);

/// Size-keyed byte array pool. `rent` hands back a zeroed vector of exactly `size` bytes.
pub struct BasisByteArrayPooling;

impl BasisByteArrayPooling {
    pub fn rent(size: usize) -> Vec<u8> {
        let mut guard = POOL.lock();
        let pool = guard.get_or_insert_with(HashMap::new);
        if let Some(queue) = pool.get_mut(&size)
            && let Some(array) = queue.pop_front()
        {
            return array;
        }
        // If not available, create a new array
        vec![0u8; size]
    }

    pub fn return_array(array: Vec<u8>) {
        let mut guard = POOL.lock();
        let pool = guard.get_or_insert_with(HashMap::new);
        pool.entry(array.len()).or_default().push_back(array);
    }

    /// Optional: Clear all pooled arrays
    pub fn clear() {
        if let Some(pool) = POOL.lock().as_mut() {
            pool.clear();
        }
    }
}
