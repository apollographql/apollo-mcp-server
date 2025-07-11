use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::Level;
use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

enum LogDestination {
    File(Arc<Mutex<NonBlocking>>),
    Stderr,
}

struct LogWriter {
    destination: LogDestination,
}

impl<'a> MakeWriter<'a> for LogWriter {
    type Writer = Box<dyn std::io::Write + Send + Sync>;

    fn make_writer(&'a self) -> Self::Writer {
        match &self.destination {
            LogDestination::File(tracing_appender) => match tracing_appender.lock() {
                Ok(tracing_appender) => Box::new(tracing_appender.clone()),
                Err(_) => Box::new(std::io::stderr()),
            },
            LogDestination::Stderr => Box::new(std::io::stderr()),
        }
    }
}

fn build_log_writer(
    log_path: String,
) -> (
    impl for<'a> MakeWriter<'a> + Send + Sync + 'static,
    bool,
    Option<WorkerGuard>,
) {
    let fallback_writer = LogWriter {
        destination: LogDestination::Stderr,
    };

    let log_dir = Path::new(log_path.as_str());

    if let Err(e) = fs::create_dir_all(log_dir) {
        eprintln!("Failed to create log directory: {}", e);
        return (fallback_writer, true, None);
    }

    match RollingFileAppender::builder()
        .rotation(Rotation::NEVER)
        .filename_prefix("apollo_mcp_server")
        .filename_suffix("log")
        .build(log_path)
    {
        Ok(file_appender) => {
            let (non_blocking_writer, guard) = tracing_appender::non_blocking(file_appender);
            let writer = LogWriter {
                destination: LogDestination::File(Arc::new(Mutex::new(non_blocking_writer))),
            };

            (writer, false, Some(guard))
        }
        Err(_) => (fallback_writer, true, None),
    }
}

pub fn setup_logging(log_path: String, log_level: Level) -> Option<WorkerGuard> {
    let (log_writer, has_ansi, guard) = build_log_writer(log_path);
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(log_level.into()))
        .with_writer(log_writer)
        .with_ansi(has_ansi)
        .with_target(false)
        .init();

    guard
}
