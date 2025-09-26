### feat: adding ability to omit attributes for traces and metrics - @alocay PR #358

Adding ability to configure which attributes are omitted from telemetry traces and metrics.

1. Using a Rust build script (`build.rs`) to auto-generate telemetry attribute code based on the data found in `telemetry.toml`.
2. Utilizing an enum for attributes so typos in the config file raise an error.
3. Omitting trace attributes by filtering it out in a custom exporter.
4. Omitting metric attributes by indicating which attributes are allowed via a view.
5. Created `telemetry_attributes.rs` to map `TelemetryAttribute` enum to a OTEL `Key`.

The `telemetry.toml` file includes attributes (both for metrics and traces) as well as list of metrics gathered. An example would look like the following:
```
[attributes.apollo.mcp]
my_attribute = "Some attribute info"

[metrics.apollo.mcp]
some.count = "Some metric count info"
```
This would generate a file that looks like the following:
```
/// All TelemetryAttribute values
pub const ALL_ATTRS: &[TelemetryAttribute; 1usize] = &[
    TelemetryAttribute::MyAttribute
];
#[derive(Debug, ::serde::Deserialize, ::schemars::JsonSchema,, Clone, Eq, PartialEq, Hash, Copy)]
pub enum TelemetryAttribute {
    ///Some attribute info
    #[serde(alias = "my_attribute")]
    MyAttribute,
}
impl TelemetryAttribute {
    /// Supported telemetry attribute (tags) values
    pub const fn as_str(&self) -> &'static str {
        match self {
            TelemetryAttribute::MyAttribute => "apollo.mcp.my_attribute",
        }
    }
}
#[derive(Debug, ::serde::Deserialize, ::schemars::JsonSchema,, Clone, Eq, PartialEq, Hash, Copy)]
pub enum TelemetryMetric {
    ///Some metric count info
    #[serde(alias = "some.count")]
    SomeCount,
}
impl TelemetryMetric {
    /// Converts TelemetryMetric to &str
    pub const fn as_str(&self) -> &'static str {
        match self {
            TelemetryMetric::SomeCount => "apollo.mcp.some.count",
        }
    }
}
```
An example configuration that omits `tool_name` attribute for metrics and `request_id` for tracing would look like the following:
```
telemetry:
  exporters:
    metrics:
      otlp:
        endpoint: "http://localhost:4317"
        protocol: "grpc"
      omitted_attributes:
        - tool_name
    tracing:
      otlp:
        endpoint: "http://localhost:4317"
        protocol: "grpc"
      omitted_attributes:
        - request_id
```
