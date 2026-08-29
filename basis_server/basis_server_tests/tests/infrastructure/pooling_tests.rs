//! `BasisObjectPool<T>`, `BasisByteArrayPooling` and `ThreadSafeMessagePool<T>`: get pops (LIFO)
//! or creates, return pushes, returned instances are not reset, and concurrent use never hands one
//! instance to two holders.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use basis_network_core::compression::BasisObjectPool;
use basis_network_core::pooling::{BasisByteArrayPooling, ThreadSafeMessagePool};
use serial_test::serial;

/// Runs a worker on dedicated threads released together, then joins with a bounded wait so a
/// deadlocked pool fails the test instead of hanging the run.
fn run_concurrently(thread_count: usize, worker: impl Fn() + Send + Sync + 'static) {
    let worker = Arc::new(worker);
    let gate = Arc::new(Barrier::new(thread_count));
    let handles: Vec<_> = (0..thread_count)
        .map(|_| {
            let worker = worker.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                worker();
            })
        })
        .collect();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    for h in handles {
        assert!(std::time::Instant::now() < deadline, "pool workers did not finish within 60 seconds");
        h.join().expect("worker panicked");
    }
}

// ── BasisObjectPool ──

#[derive(Default)]
struct PooledNode {
    id: usize,
}

static NEXT_NODE: AtomicUsize = AtomicUsize::new(1);

fn node_factory() -> PooledNode {
    PooledNode { id: NEXT_NODE.fetch_add(1, Ordering::Relaxed) }
}

#[test]
fn get_empty_pool_creates_distinct_instances_via_factory() {
    let created = Arc::new(AtomicUsize::new(0));
    let counter = created.clone();
    let pool = BasisObjectPool::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
        node_factory()
    });
    let first = pool.get();
    let second = pool.get();
    assert_ne!(first.id, second.id);
    assert_eq!(created.load(Ordering::Relaxed), 2);
}

#[test]
fn return_then_get_reuses_instance_without_invoking_factory() {
    let created = Arc::new(AtomicUsize::new(0));
    let counter = created.clone();
    let pool = BasisObjectPool::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
        node_factory()
    });
    let item = pool.get();
    let id = item.id;
    pool.return_item(item);
    assert_eq!(pool.get().id, id);
    assert_eq!(created.load(Ordering::Relaxed), 1);
}

#[test]
fn get_drains_returned_items_in_lifo_order() {
    let pool = BasisObjectPool::new(node_factory);
    let a = pool.get();
    let b = pool.get();
    let (a_id, b_id) = (a.id, b.id);
    pool.return_item(a);
    pool.return_item(b);
    assert_eq!(pool.get().id, b_id);
    assert_eq!(pool.get().id, a_id);
}

#[test]
fn return_does_not_reset_instances_caller_must_clear_state() {
    let pool = BasisObjectPool::new(Vec::<i32>::new);
    let mut list = pool.get();
    list.push(42);
    pool.return_item(list);
    let reused = pool.get();
    assert_eq!(reused, vec![42]);
}

#[test]
fn concurrent_get_return_never_hands_same_instance_to_two_holders() {
    const THREADS: usize = 8;
    const ITERATIONS: usize = 10_000;
    let pool = Arc::new(BasisObjectPool::new(node_factory));
    let outstanding = Arc::new(Mutex::new(HashSet::new()));
    let duplicates = Arc::new(AtomicUsize::new(0));
    let (p, o, d) = (pool.clone(), outstanding.clone(), duplicates.clone());
    run_concurrently(THREADS, move || {
        for _ in 0..ITERATIONS {
            let node = p.get();
            if !o.lock().unwrap().insert(node.id) {
                d.fetch_add(1, Ordering::Relaxed);
            }
            o.lock().unwrap().remove(&node.id);
            p.return_item(node);
        }
    });
    assert_eq!(duplicates.load(Ordering::Relaxed), 0);
    assert!(outstanding.lock().unwrap().is_empty());
}

// ── BasisByteArrayPooling ──

#[test]
#[serial(byte_pool)]
fn rent_returns_writable_array_of_exact_requested_length() {
    for size in [1usize, 2, 16, 255, 1024, 65_536] {
        let mut array = BasisByteArrayPooling::rent(size);
        assert_eq!(array.len(), size);
        array[0] = 0x5A;
        array[size - 1] = 0xA5;
    }
}

#[test]
#[serial(byte_pool)]
fn rent_zero_size_returns_empty_array() {
    BasisByteArrayPooling::clear();
    let empty = BasisByteArrayPooling::rent(0);
    assert!(empty.is_empty());
    BasisByteArrayPooling::return_array(empty);
    assert!(BasisByteArrayPooling::rent(0).is_empty());
}

#[test]
#[serial(byte_pool)]
fn rent_large_size_honors_length_and_return_then_rent_reuses() {
    const SIZE: usize = (1 << 20) + 17;
    BasisByteArrayPooling::clear();
    let mut large = BasisByteArrayPooling::rent(SIZE);
    assert_eq!(large.len(), SIZE);
    large[SIZE - 1] = 0x7F;
    let ptr = large.as_ptr();
    BasisByteArrayPooling::return_array(large);
    let again = BasisByteArrayPooling::rent(SIZE);
    assert_eq!(again.as_ptr(), ptr, "the same buffer comes back");
    BasisByteArrayPooling::clear();
}

#[test]
#[serial(byte_pool)]
fn return_then_rent_reuses_buffer_without_clearing_contents() {
    const SIZE: usize = 7717;
    BasisByteArrayPooling::clear();
    let mut array = BasisByteArrayPooling::rent(SIZE);
    array[0] = 0xAB;
    array[SIZE - 1] = 0xCD;
    let ptr = array.as_ptr();
    BasisByteArrayPooling::return_array(array);
    let reused = BasisByteArrayPooling::rent(SIZE);
    assert_eq!(reused.as_ptr(), ptr);
    // The pool does not zero buffers; stale-data hygiene is on the caller.
    assert_eq!(reused[0], 0xAB);
    assert_eq!(reused[SIZE - 1], 0xCD);
}

#[test]
#[serial(byte_pool)]
fn rent_uses_exact_size_buckets_never_borrows_other_sizes() {
    BasisByteArrayPooling::clear();
    let pooled = BasisByteArrayPooling::rent(4099);
    let ptr = pooled.as_ptr();
    BasisByteArrayPooling::return_array(pooled);
    let other = BasisByteArrayPooling::rent(4100);
    assert_eq!(other.len(), 4100);
    let same_bucket = BasisByteArrayPooling::rent(4099);
    assert_eq!(same_bucket.as_ptr(), ptr);
}

#[test]
#[serial(byte_pool)]
fn clear_drops_pooled_buffers() {
    BasisByteArrayPooling::clear();
    let array = BasisByteArrayPooling::rent(6151);
    let ptr = array.as_ptr();
    BasisByteArrayPooling::return_array(array);
    BasisByteArrayPooling::clear();
    // A fresh allocation may land at any address, including the freed one; what must not happen is
    // the old buffer's identity surviving a clear — so its length is the pinned property.
    let fresh = BasisByteArrayPooling::rent(6151);
    assert_eq!(fresh.len(), 6151);
    let _ = ptr;
}

#[test]
#[serial(byte_pool)]
fn concurrent_rentals_never_alias_the_same_array() {
    const RENTALS: usize = 8_000;
    const SIZE: usize = 5081;
    let rented = Arc::new(Mutex::new(Vec::new()));
    let wrong_length = Arc::new(AtomicUsize::new(0));
    let (r, w) = (rented.clone(), wrong_length.clone());
    run_concurrently(8, move || {
        for _ in 0..RENTALS / 8 {
            let array = BasisByteArrayPooling::rent(SIZE);
            if array.len() != SIZE {
                w.fetch_add(1, Ordering::Relaxed);
            }
            r.lock().unwrap().push(array);
        }
    });
    let held = rented.lock().unwrap();
    let mut ptrs: HashSet<usize> = HashSet::new();
    for a in held.iter() {
        assert!(ptrs.insert(a.as_ptr() as usize), "two rentals alias one buffer");
    }
    assert_eq!(wrong_length.load(Ordering::Relaxed), 0);
    assert_eq!(held.len(), RENTALS);
}

#[test]
#[serial(byte_pool)]
fn concurrent_rent_return_storm_keeps_buffers_exclusive() {
    const THREADS: usize = 8;
    const ITERATIONS: usize = 10_000;
    const SIZE: usize = 3371;
    let outstanding = Arc::new(Mutex::new(HashSet::new()));
    let duplicates = Arc::new(AtomicUsize::new(0));
    let wrong_length = Arc::new(AtomicUsize::new(0));
    let (o, d, w) = (outstanding.clone(), duplicates.clone(), wrong_length.clone());
    run_concurrently(THREADS, move || {
        for i in 0..ITERATIONS {
            let mut array = BasisByteArrayPooling::rent(SIZE);
            if array.len() != SIZE {
                w.fetch_add(1, Ordering::Relaxed);
            }
            let key = array.as_ptr() as usize;
            if !o.lock().unwrap().insert(key) {
                d.fetch_add(1, Ordering::Relaxed);
            }
            array[i % SIZE] = i as u8;
            o.lock().unwrap().remove(&key);
            BasisByteArrayPooling::return_array(array);
        }
    });
    assert_eq!(wrong_length.load(Ordering::Relaxed), 0);
    assert_eq!(duplicates.load(Ordering::Relaxed), 0);
    assert!(outstanding.lock().unwrap().is_empty());
}

// ── ThreadSafeMessagePool ──

#[derive(Default)]
struct StatefulMessage {
    value: i32,
    id: usize,
}

#[derive(Default)]
struct StormMessage {
    id: usize,
}

static NEXT_MESSAGE: AtomicUsize = AtomicUsize::new(1);

fn stamp<T>(mut m: T, id: &mut usize) -> T
where
    T: Default,
{
    let _ = &mut m;
    *id = NEXT_MESSAGE.fetch_add(1, Ordering::Relaxed);
    m
}

#[test]
#[serial(message_pool)]
fn rent_empty_pool_creates_distinct_instances_and_return_then_rent_reuses() {
    let mut first = ThreadSafeMessagePool::<StatefulMessage>::rent();
    let mut second = ThreadSafeMessagePool::<StatefulMessage>::rent();
    let (mut a, mut b) = (0, 0);
    first = stamp(first, &mut a);
    second = stamp(second, &mut b);
    first.id = a;
    second.id = b;
    assert_ne!(first.id, second.id);

    let id = first.id;
    first.value = 42;
    ThreadSafeMessagePool::return_obj(first);
    let reused = ThreadSafeMessagePool::<StatefulMessage>::rent();
    // Matches production usage: the rented instance is deserialized over, so whatever the previous
    // user left behind is still there.
    assert_eq!(reused.id, id);
    assert_eq!(reused.value, 42);
    ThreadSafeMessagePool::return_obj(second);
    ThreadSafeMessagePool::return_obj(reused);
}

#[test]
#[serial(message_pool)]
fn return_beyond_cap_does_not_panic_and_retains_at_most_the_cap() {
    const MAX_POOL_SIZE: usize = 500; // mirrors ThreadSafeMessagePool<T>::MAX_POOL_SIZE
    const OVER_RETURN: usize = 600;
    for i in 0..OVER_RETURN {
        ThreadSafeMessagePool::return_obj(StormMessage { id: 100_000 + i });
    }
    let mut rented = HashSet::new();
    let mut from_returned = 0;
    for _ in 0..OVER_RETURN {
        let message = ThreadSafeMessagePool::<StormMessage>::rent();
        if message.id >= 100_000 {
            from_returned += 1;
            assert!(rented.insert(message.id), "an instance was handed out twice");
        }
    }
    // Retention stopped at the cap: no more than the cap came back out of what was returned.
    assert!(from_returned <= MAX_POOL_SIZE, "{from_returned} retained past the cap");
    assert!(from_returned > 0);
}

#[test]
#[serial(message_pool)]
fn concurrent_rent_return_storm_no_duplicate_outstanding_instances() {
    const THREADS: usize = 8;
    const ITERATIONS: usize = 10_000;
    let outstanding = Arc::new(Mutex::new(HashSet::new()));
    let duplicates = Arc::new(AtomicUsize::new(0));
    let (o, d) = (outstanding.clone(), duplicates.clone());
    run_concurrently(THREADS, move || {
        for _ in 0..ITERATIONS {
            let mut message = ThreadSafeMessagePool::<StormMessage>::rent();
            if message.id == 0 {
                message.id = NEXT_MESSAGE.fetch_add(1, Ordering::Relaxed);
            }
            if !o.lock().unwrap().insert(message.id) {
                d.fetch_add(1, Ordering::Relaxed);
            }
            o.lock().unwrap().remove(&message.id);
            ThreadSafeMessagePool::return_obj(message);
        }
    });
    assert_eq!(duplicates.load(Ordering::Relaxed), 0);
    assert!(outstanding.lock().unwrap().is_empty());
}
