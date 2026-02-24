use parking_lot::Mutex;
use rhai::Shared;

/// With the `sync` feature, `rhai::Shared` is `Arc`, so this is `Arc<Mutex<T>>`.
pub(crate) type SharedMut<T> = Shared<Mutex<T>>;

pub(crate) trait WithMut<T> {
    /// Run a closure with a mutable reference to the inner value.
    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;
}

impl<T> WithMut<T> for SharedMut<T> {
    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.lock();
        f(&mut guard)
    }
}
