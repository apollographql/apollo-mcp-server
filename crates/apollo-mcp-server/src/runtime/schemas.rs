use std::collections::HashMap;

use schemars::JsonSchema;

pub(super) fn header_map(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    // A header map is just a hash map of string to string with extra validation
    HashMap::<String, String>::json_schema(generator)
}

/// Makes `protocol` optional on an OTLP exporter's gRPC variant, matching
/// `telemetry::parsers::grpc_by_default`, which fills it in.
pub(super) fn grpc_by_default(schema: &mut schemars::Schema) {
    let Some(variants) = schema.get_mut("oneOf").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for variant in variants {
        if variant.pointer("/properties/protocol/const") != Some(&"grpc".into()) {
            continue;
        }
        if let Some(required) = variant.get_mut("required").and_then(|r| r.as_array_mut()) {
            required.retain(|key| key != "protocol");
        }
        if let Some(protocol) = variant
            .pointer_mut("/properties/protocol")
            .and_then(|p| p.as_object_mut())
        {
            protocol.insert("default".into(), "grpc".into());
        }
    }
}
