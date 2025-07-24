use std::collections::HashMap;

use schemars::JsonSchema;

pub(super) fn header_map(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    // A header map is just a hash map of string to string with extra validation
    HashMap::<String, String>::json_schema(generator)
}
