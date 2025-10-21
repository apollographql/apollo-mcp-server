use super::LogRotationKind;
use crate::runtime::logging::format_style::FormatStyle;
use tracing::Level;

pub(super) const fn log_level() -> Level {
    Level::INFO
}

pub(super) const fn default_rotation() -> LogRotationKind {
    LogRotationKind::Hourly
}

pub(super) const fn default_format() -> FormatStyle {
    FormatStyle::Full
}
