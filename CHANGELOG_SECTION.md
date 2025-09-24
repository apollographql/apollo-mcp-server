# [0.9.0] - 2025-09-24

## 🚀 Features

### Add CORS support - @DaleSeo PR #362

This PR implements comprehensive CORS support for Apollo MCP Server to enable web-based MCP clients to connect without CORS errors. The implementation and configuration draw heavily from the Router's approach. Similar to other features like health checks and telemetry, CORS is supported only for the StreamableHttp transport, making it a top-level configuration.

### feat: Configuration for disabling authorization token passthrough - @swcollard PR #336

A new optional new MCP Server configuration parameter, `transport.auth.disable_auth_token_passthrough`, which is `false` by default, that when true, will no longer pass through validated Auth tokens to the GraphQL API.

### Implement metrics for mcp tool and operation counts and durations - @swcollard PR #297

This PR adds metrics to count and measure request duration to events throughout the MCP server

* apollo.mcp.operation.duration
* apollo.mcp.operation.count
* apollo.mcp.tool.duration
* apollo.mcp.tool.count
* apollo.mcp.initialize.count
* apollo.mcp.list_tools.count
* apollo.mcp.get_info.count

### feat: adding config option for trace sampling - @alocay PR #366

Adding configuration option to sample traces. Can use the following options:
1. Ratio based samples (ratio >= 1 is always sample)
2. Always on
3. Always off

Defaults to always on if not provided.

### Prototype OpenTelemetry Traces in MCP Server - @swcollard PR #274

Pulls in new crates and SDKs for prototyping instrumenting the Apollo MCP Server with Open Telemetry Traces.

* Adds new rust crates to support OTel
* Annotates excecute and call_tool functions with trace macro
* Adds Axum and Tower middleware's for OTel tracing
* Refactors Logging so that all the tracing_subscribers are set together in a single module.

### Telemetry: Trace operations and auth - @swcollard PR #375

* Adds traces for the MCP server generating Tools from Operations and performing authorization
* Includes the HTTP status code to the top level HTTP trace

### feat: Enhance tool descriptions - @DaleSeo PR #350

This PR enhances the descriptions of the introspect and search tools to offer clearer guidance for AI models on efficient GraphQL schema exploration patterns.

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

## 🐛 Fixes

### fix: Include the cargo feature and TraceContextPropagator to send otel headers downstream - @swcollard PR #307

Inside the reqwest middleware, if the global text_map_propagator is not set, it will no op and not send the traceparent and tracestate headers to the Router. Adding this is needed to correlate traces from the mcp server to the router or other downstream APIs

### Update SDL handling in sdl_to_api_schema function - @lennyburdette PR #365

Loads supergraph schemas using a function that supports various features, including Apollo Connectors. When supergraph loading failed, it would load it as a standard GraphQL schema, which reveals Federation query planning directives in when using the `search` and `introspection` tools.

### Minify: Add support for deprecated directive - @esilverm PR #367

Includes any existing `@deprecated` directives in the schema in the minified output of builtin tools. Now operations generated via these tools should take into account deprecated fields when being generated.

## 📃 Configuration

### fix: Disable statefulness to fix initialize race condition - @swcollard PR #351

We've been seeing errors with state and session handling in the MCP Server. Whether that is requests being sent before the initialized notification is processed. Or running a fleet of MCP Server pods behind a round robin load balancer. A new configuration option under the streamable_http transport `stateful_mode`, allows disabling session handling which appears to fix the race condition issue.

### Add basic config file options to otel telemetry - @swcollard PR #330

Adds new Configuration options for setting up configuration beyond the standard OTEL environment variables needed before.

* Renames trace->telemetry
* Adds OTLP options for metrics and tracing to choose grpc or http upload protocols and setting the endpoints
* This configuration is all optional, so by default nothing will be logged

## 🛠 Maintenance

### Fix version on mcp server tester - @alocay PR #374

Add a specific version when calling the mcp-server-tester for e2e tests. The current latest (1.4.1) as an issue so to avoid problems now and in the future updating the test script to invoke the testing tool via specific version.

### Configure Codecov with coverage targets - @DaleSeo PR #337

This PR adds `codecov.yml` to set up Codecov with specific coverage targets and quality standards. It helps define clear expectations for code quality. It also includes some documentation about code coverage in `CONTRIBUTING.md` and adds the Codecov badge to `README.md`.

### Implement Test Coverage Measurement and Reporting - @DaleSeo PR #335

This PR adds the bare minimum for code coverage reporting using [cargo-llvm-cov](https://crates.io/crates/cargo-llvm-cov) and integrates with [Codecov](https://www.codecov.io/). It adds a new `coverage` job to the CI workflow that generates and uploads coverage reporting in parallel with existing tests. The setup mirrors that of Router, except it uses `nextest` instead of the built-in test runner and CircleCI instead of GitHub Actions.

### test: add tests for server event and SupergraphSdlQuery - @DaleSeo PR #347

This PR adds tests for some uncovered parts of the codebase to check the Codecov integration.

### chore: update RMCP dependency ([328](https://github.com/apollographql/apollo-mcp-server/issues/328))

Update the RMCP dependency to the latest version, pulling in newer specification changes.

### ci: Pin stable rust version ([Issue #287](https://github.com/apollographql/apollo-mcp-server/issues/287))

Pins the stable version of Rust to the current latest version to ensure backwards compatibility with future versions.

