use super::LogRotationKind;
use tracing::Level;

pub(super) const fn log_level() -> Level {
    Level::INFO
}

pub(super) const fn default_rotation() -> LogRotationKind {
    LogRotationKind::Hourly
}
