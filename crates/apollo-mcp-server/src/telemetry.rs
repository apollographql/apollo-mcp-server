use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct InMemoryTelemetry {
    errored: Arc<AtomicUsize>,
}

impl Default for InMemoryTelemetry {
    fn default() -> Self {
        Self {
            errored: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl InMemoryTelemetry {
    pub fn new() -> Self {
        Self::default()
    }
}

pub trait Telemetry: Send + Sync {
    fn errors(&self) -> usize;
    fn set_error_count(&self, errors: usize);
    fn record_error(&self);
}

impl Telemetry for InMemoryTelemetry {
    fn errors(&self) -> usize {
        self.errored.load(Ordering::Relaxed)
    }

    fn set_error_count(&self, errors: usize) {
        self.errored.store(errors, Ordering::Relaxed)
    }

    fn record_error(&self) {
        self.errored.fetch_add(1, Ordering::Relaxed);
    }
}
