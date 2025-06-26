use std::collections::HashMap;

use schemars::JsonSchema;

pub(super) fn header_map(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    // A header map is just a hash map of string to string with extra validation
    HashMap::<String, String>::json_schema(generator)
}

pub(super) fn level(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
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
