use schemars::JsonSchema;

pub(crate) fn level(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    /// Log level
    #[derive(JsonSchema)]
    #[schemars(rename_all = "lowercase")]
    // This is just an intermediate type to auto create schema information for,
    // so it is OK if it is never used
    #[allow(dead_code)]
    enum Level {
        Trace,
        Debug,
        Info,
        Warn,
        Error,
    }

    Level::json_schema(generator)
}
