//! In-memory log writer shared by the event format tests.

use std::io;
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;

/// A thread-safe in-memory writer for capturing log output.
#[derive(Clone)]
pub(super) struct TestWriter(Arc<Mutex<Vec<u8>>>);

impl TestWriter {
    pub(super) fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }

    pub(super) fn contents(&self) -> String {
        let buf = self.0.lock().expect("lock poisoned");
        String::from_utf8_lossy(&buf).to_string()
    }
}

impl io::Write for TestWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("lock poisoned").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for TestWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
