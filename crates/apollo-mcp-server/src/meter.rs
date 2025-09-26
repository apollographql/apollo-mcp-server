use opentelemetry::{global, metrics::Meter};
use std::sync::LazyLock;

pub static METER: LazyLock<Meter> = LazyLock::new(|| global::meter(env!("CARGO_PKG_NAME")));
