use schemars::JsonSchema;
use serde::Deserialize;
use tracing_appender::rolling::Rotation;

#[derive(Debug, Deserialize, JsonSchema, Clone)]
pub enum LogRotationKind {
    #[serde(alias = "minutely", alias = "MINUTELY")]
    Minutely,
    #[serde(alias = "hourly", alias = "HOURLY")]
    Hourly,
    #[serde(alias = "daily", alias = "DAILY")]
    Daily,
    #[serde(alias = "never", alias = "NEVER")]
    Never,
}

impl From<LogRotationKind> for Rotation {
    fn from(value: LogRotationKind) -> Self {
        match value {
            LogRotationKind::Minutely => Rotation::MINUTELY,
            LogRotationKind::Hourly => Rotation::HOURLY,
            LogRotationKind::Daily => Rotation::DAILY,
            LogRotationKind::Never => Rotation::NEVER,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LogRotationKind;
    use rstest::rstest;
    use tracing_appender::rolling::Rotation;

    #[rstest]
    #[case(LogRotationKind::Minutely, Rotation::MINUTELY)]
    #[case(LogRotationKind::Hourly, Rotation::HOURLY)]
    #[case(LogRotationKind::Daily, Rotation::DAILY)]
    #[case(LogRotationKind::Never, Rotation::NEVER)]
    fn it_maps_to_rotation_correctly(
        #[case] log_rotation_kind: LogRotationKind,
        #[case] expected: Rotation,
    ) {
        let actual: Rotation = log_rotation_kind.into();
        assert_eq!(expected, actual);
    }
}
