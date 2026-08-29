use parking_lot::Mutex;

/// Object pool to avoid allocation during runtime.
pub struct BasisObjectPool<T> {
    create_func: Box<dyn Fn() -> T + Send + Sync>,
    pool: Mutex<Vec<T>>,
}

impl<T> BasisObjectPool<T> {
    pub fn new(create_func: impl Fn() -> T + Send + Sync + 'static) -> Self {
        Self {
            create_func: Box::new(create_func),
            pool: Mutex::new(Vec::new()),
        }
    }

    pub fn get(&self) -> T {
        self.pool.lock().pop().unwrap_or_else(|| (self.create_func)())
    }

    pub fn return_item(&self, item: T) {
        self.pool.lock().push(item);
    }
}
