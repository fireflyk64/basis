use std::any::{Any, TypeId};
use std::collections::HashMap;

use parking_lot::Mutex;

static POOLS: Mutex<Option<HashMap<TypeId, Vec<Box<dyn Any + Send>>>>> = Mutex::new(None);

/// One pool per message type, the counterpart of the C# generic static class.
pub struct ThreadSafeMessagePool<T>(std::marker::PhantomData<T>);

impl<T: Default + Send + 'static> ThreadSafeMessagePool<T> {
    const MAX_POOL_SIZE: usize = 500;

    pub fn rent() -> T {
        let mut guard = POOLS.lock();
        let pools = guard.get_or_insert_with(HashMap::new);
        if let Some(pool) = pools.get_mut(&TypeId::of::<T>())
            && let Some(boxed) = pool.pop()
            && let Ok(value) = boxed.downcast::<T>()
        {
            return *value;
        }
        T::default()
    }

    pub fn return_obj(obj: T) {
        let mut guard = POOLS.lock();
        let pools = guard.get_or_insert_with(HashMap::new);
        let pool = pools.entry(TypeId::of::<T>()).or_default();
        if pool.len() < Self::MAX_POOL_SIZE {
            pool.push(Box::new(obj));
        }
    }
}
