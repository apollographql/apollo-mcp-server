use opentelemetry::{global, metrics::Meter};
use std::sync::OnceLock;

static METER: OnceLock<Meter> = OnceLock::new();

pub fn get_meter() -> &'static Meter {
    METER.get_or_init(|| global::meter(env!("CARGO_PKG_NAME")))
}
