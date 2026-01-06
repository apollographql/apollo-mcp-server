# Changelog

All notable changes to this project will be documented in this file.

This project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 1.3.0 (2025-12-10)

### 🚀 Features

#### Set destructiveHint in tool annotations - @DaleSeo PR #510

This PR explicitly sets the `destructiveHint` annotation for MCP tools based on the GraphQL operation type. We now set `destructiveHint: false` for queries and `destructiveHint: true` for mutations to avoid relying on client-side spec compliance.

We currently only sets `readOnlyHint` based on whether the operation is a query and omits `destructiveHint`. Per [the MCP spec](https://modelcontextprotocol.io/legacy/concepts/tools#available-tool-annotations), `destructiveHint` defaults to true when omitted. The spec also says `destructiveHint` should be ignored when `readOnlyHint` is true, but OpenAI doesn't appear to implement this correctly.

### 🐛 Fixes

#### Fix broken non nullable return types in minified schema - @esilverm PR #514

Fix handling of minified return types to support non-null lists and other non-null cases to improve operation construction accuracy.

### 🛠 Maintenance

#### Clean up sanitize module - @DaleSeo PR #503

Just cleaning up unused code

#### Abstract away operation execution logic from the server's running state - @DaleSeo PR #517

I abstracted the operation execution logic from the server's running state, following the pattern used in apps. This change helped me write tests and identify a subtle bug where the execute tool wasn't propagating the OTel context.

## 1.2.1 (2025-11-18)

### 🚀 Features

#### feat: adding a debug print out for the entire parsed configuration - @alocay PR #496

Adding a debug print out to display the entire parsed configuration at the start of the server.

Example output:

```
2025-11-18T16:12:29.253985Z  INFO Apollo MCP Server v1.2.0 // (c) Apollo Graph, Inc. // Licensed under MIT
2025-11-18T16:12:29.254074Z DEBUG Configuration: Config {
    cors: CorsConfig {
        enabled: true,
        origins: [],
        match_origins: [],
        allow_any_origin: true,
        allow_credentials: false,
        allow_methods: [
            "GET",
            "POST",
            "DELETE",
        ],
        allow_headers: [
            "content-type",
            "mcp-protocol-version",
            "mcp-session-id",
            "traceparent",
            "tracestate",
        ],
        ...
```

### 🐛 Fixes

#### Fix fragment field validation in schema tree shaking - @DaleSeo PR #471

Fixed "field not found" errors that occurred when loading operations containing GraphQL fragments (inline fragments or fragment spreads) on union types or interfaces. The schema tree shaking algorithm now correctly handles fragments by evaluating them against their specific type conditions.

#### Implement deduplication of operations - @DaleSeo PR #491

Fixed an issue where specifying both a directory and an explicit file path within that directory in the `operations.paths` configuration would create duplicate tools.
The server now automatically deduplicates operations based on their canonical file paths, ensuring that only one tool is created per unique operation file, regardless of how the paths are specified in the configuration.

#### Index fields from interface implementing types - @DaleSeo PR #494

Fixed an issue where the search tool would not return results for fields that only exist on types implementing an interface.
Now when a query returns an interface type, the search tool correctly indexes and searches all fields from implementing types, making implementation-specific fields discoverable even when accessed through interface types.

## 1.2.0 (2025-11-13)

### 🚀 Features

#### Adding config option to specify metadata/header values for telemetry exporters - @alocay PR #460

Adding a `metadata` (for `grpc` protocol) and `headers` (for `http/protobuf` protocol) config option for the `telemetry.exporters.metrics.otlp` yaml config section.

```yaml
telemetry:
  exporters:
    metrics:
      otlp:
        endpoint: "http://127.0.0.1:4317"
        protocol: "grpc"
        metadata:
          the-key: some-value
    tracing:
      otlp:
        endpoint: "http://127.0.0.1:4317"
        protocol: "http/protobuf"
        headers:
          some-key: another-value
```

### 🐛 Fixes

#### Allow using builtin names for custom tools - @dylan-apollo PR #481

Previously, the names of builtin tools were reserved even if the tool was disabled.
These names are now available for custom tools _if_ the matching builtin tool is disabled via config:

- `introspect`
- `search`
- `explorer`
- `execute`
- `validate`

#### Improved performance of parallel tool calls - @dylan-apollo PR #475

Responsiveness of all tools is improved when many clients are connected.

## 1.1.1 (2025-10-21)

### 🐛 Fixes

#### fix docker image ignoring port setting - @DaleSeo PR #467

The Docker image had `APOLLO_MCP_TRANSPORT__PORT=8000` baked in as an environment variable in `flake.nix`. Since environment variables take precedence over config file settings (by design in our config loading logic), users are unable to override the port in their `config.yaml` when running the Docker container.

## 1.1.0 (2025-10-16)

### ❗ BREAKING ❗

#### Change default port from 5000 to 8000 - @DaleSeo PR #417

The default server port has been changed from `5000` to `8000` to avoid conflicts with common development tools and services that typically use port 5000 (such as macOS AirPlay, Flask development servers, and other local services).

**Migration**: If you were relying on the default port 5000, you can continue using it by explicitly setting the port in your configuration file or command line arguments.

- Before

```yaml
transport:
  type: streamable_http
```

- After

```yaml
transport:
  type: streamable_http
  port: 5000
```

### 🚀 Features

#### feat: Add configuration option for metric temporality - @swcollard PR #413

Creates a new configuration option for telemetry to set the Metric temporality to either Cumulative (default) or Delta.

- Cumulative - The metric value will be the overall value since the start of the measurement.
- Delta - The metric will be the difference in the measurement since the last time it was reported.

Some observability vendors require that one is used over the other so we want to support the configuration in the MCP Server.

#### Add support for forwarding headers from MCP clients to GraphQL APIs - @DaleSeo PR #428

Adds opt-in support for dynamic header forwarding, which enables metadata for A/B testing, feature flagging, geo information from CDNs, or internal instrumentation to be sent from MCP clients to downstream GraphQL APIs. It automatically blocks hop-by-hop headers according to the guidelines in [RFC 7230, section 6.1](https://datatracker.ietf.org/doc/html/rfc7230#section-6.1), and it only works with the Streamable HTTP transport.

You can configure using the `forward_headers` setting:

```yaml
forward_headers:
  - x-tenant-id
  - x-experiment-id
  - x-geo-country
```

Please note that this feature is not intended for passing through credentials as documented in the best practices page.

#### feat: Add mcp-session-id header to HTTP request trace attributes - @swcollard PR #421

Includes the value of the [Mcp-Session-Id](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports#session-management) HTTP header as an attribute of the trace for HTTP requests to the MCP Server

### 🐛 Fixes

#### Fix compatibility issue with VSCode/Copilot - @DaleSeo PR #447

This updates Apollo MCP Server’s tool schemas from [Draft 2020-12](https://json-schema.org/draft/2020-12) to [Draft‑07](https://json-schema.org/draft-07) which is more widely supported across different validators. VSCode/Copilot still validate against Draft‑07, so rejects Apollo MCP Server’s tools. Our JSON schemas don’t rely on newer features, so downgrading improves compatibility across MCP clients with no practical impact.

### 🛠 Maintenance

#### Update rmcp sdk to version 0.8.x - @swcollard PR #433

Bumping the Rust MCP SDK version used in this server up to 0.8.x

#### chore: Only initialize a single HTTP client for graphql requests - @swcollard PR #412

Currently the MCP Server spins up a new HTTP client every time it wants to make a request to the downstream graphql endpoint. This change creates a static reqwest client that gets initialized using LazyLock and reused on each graphql request.

This change is based on the suggestion from the reqwest [documentation](https://docs.rs/reqwest/latest/reqwest/struct.Client.html)

> "The Client holds a connection pool internally, so it is advised that you create one and reuse it."

## 1.0.0 (2025-10-01)

# Apollo MCP Server 1.0 Release Notes

Apollo MCP Server 1.0 marks the **General Availability (GA)** milestone, delivering a production-ready Model Context Protocol server that seamlessly bridges GraphQL APIs with AI applications. This release transforms how AI agents interact with GraphQL APIs through standardized MCP tools, enabling natural language access to your GraphQL operations.

### 🎯 GA Highlights

#### **Production-Ready MCP Protocol Implementation**

Apollo MCP Server 1.0 provides full compliance with the [MCP specification](https://modelcontextprotocol.io/specification/2025-06-18), enabling AI applications to discover and invoke GraphQL operations through standardized protocols. The server acts as a translation layer, converting GraphQL operations into MCP tools that AI models can execute through natural language requests.

**Key Benefits:**

- **Standardized AI Integration**: No more custom API bridges - use the industry-standard MCP protocol
- **Automatic Tool Discovery**: AI agents automatically discover available GraphQL operations as MCP tools
- **Type-Safe Execution**: All operations are validated against your GraphQL schema before execution
- **Enterprise-Ready**: Full OAuth 2.1 authentication and comprehensive observability

#### **🚀 Multi-Transport Architecture**

Flexible communication options for every deployment scenario:

- **stdio**: Perfect for local development and debugging with MCP Inspector
- **Streamable HTTP**: Production-grade transport with load balancer support and concurrent connections

All transports maintain full MCP protocol compliance while optimizing for specific use cases.

#### **🔧 Advanced GraphQL Integration**

**Custom Scalar Support**: Seamlessly handle specialized types like `DateTime`, `UUID`, and domain-specific scalars with automatic JSON Schema mapping.

**Mutation Controls**: Fine-grained security controls to prevent unintended data changes:

- `all`: Enable all mutations (default)
- `none`: Disable all mutations for read-only access
- `allowlist`: Only allow specific mutations

#### **📊 Flexible Schema & Operation Management**

**Dual Schema Sources:**

- **Local Files**: Direct schema control for development and offline scenarios
- **Apollo GraphOS**: Centralized schema management with automatic updates via uplink integration

**Multiple Operation Sources:**

- **Local Statement Files**: Hot-reloading `.graphql` files for rapid development
- **Persisted Query Manifests**: Security-focused pre-approved operation execution
- **GraphOS Operation Collections**: Centrally managed operations with automatic polling
- **GraphOS Persisted Queries**: Enterprise-grade operation management

#### **🤖 AI-Optimized Introspection Tools**

**Core Tools:**

- **`introspect`**: Comprehensive schema exploration with AI-friendly formatting
- **`execute`**: Safe dynamic operation execution with proper error handling
- **`validate`**: Operation validation without execution to prevent side effects
- **`search`**: Semantic schema search to efficiently find relevant types and fields

**AI Optimizations:**

- **Minified Output**: Configurable minification reduces context window usage by 30%+ while preserving essential information
- **Semantic Search**: Natural language schema exploration with ranked results

#### **⚙️ Configuration-Driven Architecture**

**YAML Configuration**: Replace complex command-line arguments with structured, version-controllable configuration files.

**Environment Variable Overrides**: Seamless environment-specific customization using the `APOLLO_MCP_` prefix convention.

**Comprehensive Validation**: Clear error messages and sensible defaults for rapid deployment.

#### **🔐 Enterprise Security & Observability**

**OAuth 2.1 Authentication**: Production-ready authentication supporting major identity providers:

- Auth0, WorkOS, Keycloak, Okta
- JWT token validation with audience and scope enforcement
- OIDC discovery for automatic provider configuration

**Health Monitoring**: Kubernetes-ready health checks with configurable liveness and readiness probes.

**OpenTelemetry Integration**: Comprehensive observability with traces, metrics, and events:

- Operation-level performance tracking
- Semantic conventions for HTTP servers when using the Streamable HTTP transport.
- OTLP export to any OpenTelemetry-compatible collector
- Integration with existing monitoring infrastructure

**CORS Support**: Enable browser-based MCP clients with comprehensive Cross-Origin Resource Sharing support following Apollo Router patterns.

### 🐛 Fixes

#### fix: remove verbose logging - @swcollard PR #401

The tracing-subscriber crate we are using to create logs does not have a configuration to exclude the span name and attributes from the log line. This led to rather verbose logs on app startup which would dump the full operation object into the logs before the actual log line.

This change strips the attributes from the top level spans so that we still have telemetry and tracing during this important work the server is doing, but they don't make it into the logs. The relevant details are provided in child spans after the operation has been parsed so we aren't losing any information other than a large json blob in the top level trace of generating Tools from GraphQL Operations.

### 🛠 Maintenance

#### deps: update rust to v1.90.0 - @DaleSeo PR #387

Updates the Rust version to v1.90.0

## 0.9.0 (2025-09-24)

### 🚀 Features

#### Prototype OpenTelemetry Traces in MCP Server - @swcollard PR #274

Pulls in new crates and SDKs for prototyping instrumenting the Apollo MCP Server with Open Telemetry Traces.

- Adds new rust crates to support OTel
- Annotates excecute and call_tool functions with trace macro
- Adds Axum and Tower middleware's for OTel tracing
- Refactors Logging so that all the tracing_subscribers are set together in a single module.

#### Add CORS support - @DaleSeo PR #362

This PR implements comprehensive CORS support for Apollo MCP Server to enable web-based MCP clients to connect without CORS errors. The implementation and configuration draw heavily from the Router's approach. Similar to other features like health checks and telemetry, CORS is supported only for the StreamableHttp transport, making it a top-level configuration.

#### Enhance tool descriptions - @DaleSeo PR #350

This PR enhances the descriptions of the introspect and search tools to offer clearer guidance for AI models on efficient GraphQL schema exploration patterns.

#### Telemetry: Trace operations and auth - @swcollard PR #375

- Adds traces for the MCP server generating Tools from Operations and performing authorization
- Includes the HTTP status code to the top level HTTP trace

#### Implement metrics for mcp tool and operation counts and durations - @swcollard PR #297

This PR adds metrics to count and measure request duration to events throughout the MCP server

- apollo.mcp.operation.duration
- apollo.mcp.operation.count
- apollo.mcp.tool.duration
- apollo.mcp.tool.count
- apollo.mcp.initialize.count
- apollo.mcp.list_tools.count
- apollo.mcp.get_info.count

#### Adding ability to omit attributes for traces and metrics - @alocay PR #358

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

#### Adding config option for trace sampling - @alocay PR #366

Adding configuration option to sample traces. Can use the following options:

1. Ratio based samples (ratio >= 1 is always sample)
2. Always on
3. Always off

Defaults to always on if not provided.

### 🐛 Fixes

#### Update SDL handling in sdl_to_api_schema function - @lennyburdette PR #365

Loads supergraph schemas using a function that supports various features, including Apollo Connectors. When supergraph loading failed, it would load it as a standard GraphQL schema, which reveals Federation query planning directives in when using the `search` and `introspection` tools.

#### Include the cargo feature and TraceContextPropagator to send otel headers downstream - @swcollard PR #307

Inside the reqwest middleware, if the global text_map_propagator is not set, it will no op and not send the traceparent and tracestate headers to the Router. Adding this is needed to correlate traces from the mcp server to the router or other downstream APIs

#### Add support for deprecated directive - @esilverm PR #367

Includes any existing `@deprecated` directives in the schema in the minified output of builtin tools. Now operations generated via these tools should take into account deprecated fields when being generated.

### 📃 Configuration

#### Add basic config file options to otel telemetry - @swcollard PR #330

Adds new Configuration options for setting up configuration beyond the standard OTEL environment variables needed before.

- Renames trace->telemetry
- Adds OTLP options for metrics and tracing to choose grpc or http upload protocols and setting the endpoints
- This configuration is all optional, so by default nothing will be logged

#### Disable statefulness to fix initialize race condition - @swcollard PR #351

We've been seeing errors with state and session handling in the MCP Server. Whether that is requests being sent before the initialized notification is processed. Or running a fleet of MCP Server pods behind a round robin load balancer. A new configuration option under the streamable_http transport `stateful_mode`, allows disabling session handling which appears to fix the race condition issue.

### 🛠 Maintenance

#### Add tests for server event and SupergraphSdlQuery - @DaleSeo PR #347

This PR adds tests for some uncovered parts of the codebase to check the Codecov integration.

#### Fix version on mcp server tester - @alocay PR #374

Add a specific version when calling the mcp-server-tester for e2e tests. The current latest (1.4.1) as an issue so to avoid problems now and in the future updating the test script to invoke the testing tool via specific version.

## 0.8.0 (2025-09-12)

### 🚀 Features

#### feat: Configuration for disabling authorization token passthrough - @swcollard PR #336

A new optional new MCP Server configuration parameter, `transport.auth.disable_auth_token_passthrough`, which is `false` by default, that when true, will no longer pass through validated Auth tokens to the GraphQL API.

### 🛠 Maintenance

#### Configure Codecov with coverage targets - @DaleSeo PR #337

This PR adds `codecov.yml` to set up Codecov with specific coverage targets and quality standards. It helps define clear expectations for code quality. It also includes some documentation about code coverage in `CONTRIBUTING.md` and adds the Codecov badge to `README.md`.

#### Implement Test Coverage Measurement and Reporting - @DaleSeo PR #335

This PR adds the bare minimum for code coverage reporting using [cargo-llvm-cov](https://crates.io/crates/cargo-llvm-cov) and integrates with [Codecov](https://www.codecov.io/). It adds a new `coverage` job to the CI workflow that generates and uploads coverage reporting in parallel with existing tests. The setup mirrors that of Router, except it uses `nextest` instead of the built-in test runner and CircleCI instead of GitHub Actions.

#### chore: update RMCP dependency ([328](https://github.com/apollographql/apollo-mcp-server/issues/328))

Update the RMCP dependency to the latest version, pulling in newer specification changes.

#### ci: Pin stable rust version ([Issue #287](https://github.com/apollographql/apollo-mcp-server/issues/287))

Pins the stable version of Rust to the current latest version to ensure backwards compatibility with future versions.

## 0.7.5 (2025-09-03)

### 🐛 Fixes

#### fix: Validate ExecutableDocument in validate tool - @swcollard PR #329

Contains fixes for https://github.com/apollographql/apollo-mcp-server/issues/327

The validate tool was parsing the operation passed in to it against the schema but it wasn't performing the validate function on the ExecutableDocument returned by the Parser. This led to cases where missing required arguments were not caught by the Tool.

This change also updates the input schema to the execute tool to make it more clear to the LLM that it needs to provide a valid JSON object

### 🛠 Maintenance

#### test: adding a basic manual e2e test for mcp server - @alocay PR #320

Adding some basic e2e tests using [mcp-server-tester](https://github.com/steviec/mcp-server-tester). Currently, the tool does not always exit (ctrl+c is sometimes needed) so this should be run manually.

#### How to run tests?

Added a script `run_tests.sh` (may need to run `chmod +x` to run it) to run tests. Basic usage found via `./run_tests.sh -h`. The script does the following:

1. Builds test/config yaml paths and verifies the files exist.
2. Checks if release `apollo-mcp-server` binary exists. If not, it builds the binary via `cargo build --release`.
3. Reads in the template file (used by `mcp-server-tester`) and replaces all `<test-dir>` placeholders with the test directory value. Generates this test server config file and places it in a temp location.
4. Invokes the `mcp-server-tester` via `npx`.
5. On script exit the generated config is cleaned up.

#### Example run:

To run the tests for `local-operations` simply run `./run_tests.sh local-operations`

#### Update snapshot format - @DaleSeo PR #313

Updates all inline snapshots in the codebase to ensure they are consistent with the latest insta format.

#### Hardcoded version strings in tests - @DaleSeo PR #305

The GraphQL tests have hardcoded version strings that we need to update manually each time we release a new version. Since this isn't included in the release checklist, it's easy to miss it and only notice the test failures later.

## 0.7.4 (2025-08-27)

### 🐛 Fixes

#### fix: Add missing token propagation for execute tool - @DaleSeo PR #298

The execute tool is not forwarding JWT authentication tokens to upstream GraphQL endpoints, causing authentication failures when using this tool with protected APIs. This PR adds missing token propagation for execute tool.

## 0.7.3 (2025-08-25)

### 🐛 Fixes

#### fix: generate openAI-compatible json schemas for list types - @DaleSeo PR #272

The MCP server is generating JSON schemas that don't match OpenAI's function calling specification. It puts `oneOf` at the array level instead of using `items` to define the JSON schemas for the GraphQL list types. While some other LLMs are more flexible about this, it technically violates the [JSON Schema specification](https://json-schema.org/understanding-json-schema/reference/array) that OpenAI strictly follows.

This PR updates the list type handling logic to move `oneOf` inside `items` for GraphQL list types.

## 0.7.2 (2025-08-19)

### 🚀 Features

#### Prevent server restarts while polling collections - @DaleSeo PR #261

Right now, the MCP server restarts whenever there's a connectivity issue while polling collections from GraphOS. This causes the entire server to restart instead of handling the error gracefully.

```
Error: Failed to create operation: Error loading collection: error sending request for url (https://graphql.api.apollographql.com/api/graphql)
Caused by:
    Error loading collection: error sending request for url (https://graphql.api.apollographql.com/api/graphql)
```

This PR prevents server restarts by distinguishing between transient errors and permanent errors.

### 🐛 Fixes

#### Keycloak OIDC discovery URL transformation - @DaleSeo PR #238

The MCP server currently replaces the entire path when building OIDC discovery URLs. This causes authentication failures for identity providers like Keycloak, which have path-based realms in the URL. This PR updates the URL transformation logic to preserve the existing path from the OAuth server URL.

#### fix: build error, let expressions unstable in while - @ThoreKoritzius #263

Fix unstable let expressions in while loop
Replaced the unstable while let = expr syntax with a stable alternative, ensuring the code compiles on stable Rust without requiring nightly features.

### 🛠 Maintenance

#### Address Security Vulnerabilities - @DaleSeo PR #264

This PR addresses the security vulnerabilities and dependency issues tracked in Dependency Dashboard #41 (https://osv.dev/vulnerability/RUSTSEC-2024-0388).

- Replaced the unmaintained `derivate` crate with the `educe` crate instead.
- Updated the `tantivy` crate.

## 0.7.1 (2025-08-13)

### 🚀 Features

#### feat: Pass `remote-mcp` mcp-session-id header along to GraphQL request - @damassi PR #236

This adds support for passing the `mcp-session-id` header through from `remote-mcp` via the MCP client config. This header [originates from the underlying `@modelcontextprotocol/sdk` library](https://github.com/modelcontextprotocol/typescript-sdk/blob/a1608a6513d18eb965266286904760f830de96fe/src/client/streamableHttp.ts#L182), invoked from `remote-mcp`.

With this change it is possible to correlate requests from MCP clients through to the final GraphQL server destination.

### 🐛 Fixes

#### fix: Valid token fails validation with multiple audiences - @DaleSeo PR #244

Valid tokens are failing validation with the following error when the JWT tokens contain an audience claim as an array.

```
JSON error: invalid type: sequence, expected a string at line 1 column 97
```

According to [RFC 7519 Section 4.1.3](https://datatracker.ietf.org/doc/html/rfc7519#section-4.1.3), the audience claim can be either a single string or an array of strings. However, our implementation assumes it will always be a string, which is causing this JSON parsing error.
This fix updates the `Claims` struct to use `Vec<String>` instead of `String` for the `aud` field, along with a custom deserializer to handle both string and array formats.

#### fix: Add custom deserializer to handle APOLLO_UPLINK_ENDPOINTS environment variable parsing - @swcollard PR #220

The APOLLO_UPLINK_ENDPOINTS environment variables has historically been a comma separated list of URL strings.
The move to yaml configuration allows us to more directly define the endpoints as a Vec.
This fix introduces a custom deserializer for the `apollo_uplink_endpoints` config field that can handle both the environment variable comma separated string, and the yaml-based list.

## 0.7.0 (2025-08-04)

### 🚀 Features

#### feat: add mcp auth - @nicholascioli PR #210

The MCP server can now be configured to act as an OAuth 2.1 resource server, following
guidelines from the official MCP specification on Authorization / Authentication (see
[the spec](https://modelcontextprotocol.io/specification/2025-06-18/basic/authorization)).

To configure this new feature, a new `auth` section has been added to the SSE and
Streamable HTTP transports. Below is an example configuration using Streamable HTTP:

```yaml
transport:
  type: streamable_http
  auth:
    # List of upstream delegated OAuth servers
    # Note: These need to support the OIDC metadata discovery endpoint
    servers:
      - https://auth.example.com

    # List of accepted audiences from upstream signed JWTs
    # See: https://www.ory.sh/docs/hydra/guides/audiences
    audiences:
      - mcp.example.audience

    # The externally available URL pointing to this MCP server. Can be `localhost`
    # when testing locally.
    # Note: Subpaths must be preserved here as well. So append `/mcp` if using
    # Streamable HTTP or `/sse` is using SSE.
    resource: https://hosted.mcp.server/mcp

    # Optional link to more documentation relating to this MCP server.
    resource_documentation: https://info.mcp.server

    # List of queryable OAuth scopes from the upstream OAuth servers
    scopes:
      - read
      - mcp
      - profile
```

### 🐛 Fixes

#### Setting input_schema properties to empty when operation has no args ([Issue #136](https://github.com/apollographql/apollo-mcp-server/issues/136)) ([PR #212](https://github.com/apollographql/apollo-mcp-server/pull/212))

To support certain scenarios where a client fails on an omitted `properties` field within `input_schema`, setting the field to an empty map (`{}`) instead. While a missing `properties` field is allowed this will unblock
certain users and allow them to use the MCP server.

## 0.6.1 (2025-07-29)

### 🐛 Fixes

#### Handle headers from config file - @tylerscoville PR #213

Fix an issue where the server crashes when headers are set in the config file

#### Handle environment variables when no config file is provided - @DaleSeo PR #211

Fix an issue where the server fails with the message "Missing environment variable: APOLLO_GRAPH_REF," even when the variables are properly set.

### 🚀 Features

#### Health Check Support - @DaleSeo PR #209

Health reporting functionality has been added to make the MCP server ready for production deployment with proper health monitoring and Kubernetes integration.

## 0.6.0 (2025-07-14)

### ❗ BREAKING ❗

#### Replace CLI flags with a configuration file - @nicholascioli PR #162

All command line arguments are now removed and replaced with equivalent configuration
options. The Apollo MCP server only accepts a single argument which is a path to a
configuration file. An empty file may be passed, as all options have sane defaults
that follow the previous argument defaults.

All options can be overridden by environment variables. They are of the following
form:

- Prefixed by `APOLLO_MCP_`
- Suffixed by the config equivalent path, with `__` marking nested options.

E.g. The environment variable to change the config option `introspection.execute.enabled`
would be `APOLLO_MCP_INTROSPECTION__EXECUTE__ENABLED`.

Below is a valid configuration file with some options filled out:

```yaml
custom_scalars: /path/to/custom/scalars
endpoint: http://127.0.0.1:4000
graphos:
  apollo_key: some.key
  apollo_graph_ref: example@graph
headers:
  X-Some-Header: example-value
introspection:
  execute:
    enabled: true
  introspect:
    enabled: false
logging:
  level: info
operations:
  source: local
  paths:
    - /path/to/operation.graphql
    - /path/to/other/operation.graphql
overrides:
  disable_type_description: false
  disable_schema_description: false
  enable_explorer: false
  mutation_mode: all
schema:
  source: local
  path: /path/to/schema.graphql
transport:
  type: streamable_http
  address: 127.0.0.1
  port: 5000
```

### 🚀 Features

#### Validate tool for verifying graphql queries before executing them - @swcollard PR #203

The introspection options in the mcp server provide introspect, execute, and search tools. The LLM often tries to validate its queries by just executing them. This may not be desired (there might be side effects, for example). This feature adds a `validate` tool so the LLM can validate the operation without actually hitting the GraphQL endpoint. It first validates the syntax of the operation, and then checks it against the introspected schema for validation.

#### Minify introspect return value - @pubmodmatt PR #178

The `introspect` and `search` tools now have an option to minify results. Minified GraphQL SDL takes up less space in the context window.

#### Add search tool - @pubmodmatt PR #171

A new experimental `search` tool has been added that allows the AI model to specify a set of terms to search for in the GraphQL schema. The top types matching that search are returned, as well as enough information to enable creation of GraphQL operations involving those types.

## 0.5.2 (2025-07-10)

### 🐛 Fixes

#### Fix ServerInfo - @pubmodmatt PR #183

The server will now report the correct server name and version to clients, rather than the Rust MCP SDK name and version.

## 0.5.1 (2025-07-08)

### 🐛 Fixes

#### Fix an issue with rmcp 0.2.x upgrade - @pubmodmatt PR #181

Fix an issue where the server was unresponsive to external events such as changes to operation collections.

## 0.5.0 (2025-07-08)

### ❗ BREAKING ❗

#### Deprecate -u,--uplink argument and use default collection - @Jephuff PR #154

`--uplink` and `-u` are deprecated and will act as an alias for `--uplink-manifest`. If a schema isn't provided, it will get fetched from uplink by default, and `--uplink-manifest` can be used to fetch the persisted queries from uplink.
The server will now default to the default MCP tools from operation collections.

### 🚀 Features

#### Add --version argument - @Jephuff PR #154

`apollo-mcp-server --version` will print the version of apollo-mcp-server currently installed

#### Support operation variable comments as description overrides - @alocay PR #164

Operation comments for variables will now act as overrides for variable descriptions

#### Include operation name with GraphQL requests - @DaleSeo PR #166

Include the operation name with GraphQL requests if it's available.

```diff
{
   "query":"query GetAlerts(: String!) { alerts(state: ) { severity description instruction } }",
   "variables":{
      "state":"CO"
   },
   "extensions":{
      "clientLibrary":{
         "name":"mcp",
         "version": ...
      }
   },
+  "operationName":"GetAlerts"
}
```

### 🐛 Fixes

#### The execute tool handles invalid operation types - @DaleSeo PR #170

The execute tool returns an invalid parameters error when the operation type does not match the mutation mode.

#### Skip unnamed operations and log a warning instead of crashing - @DaleSeo PR #173

Unnamed operations are now skipped with a warning instead of causing the server to crash

#### Support retaining argument descriptions from schema for variables - @alocay PR #147

Use descriptions for arguments from schema when building descriptions for operation variables.

#### Invalid operation should not crash the MCP Server - @DaleSeo PR #176

Gracefully handle and skip invalid GraphQL operations to prevent MCP server crashes during startup or runtime.

## 0.4.2 (2025-06-24)

### 🚀 Features

#### Pass in --collection default to use default collection - @Jephuff PR #151

--collection default will use the configured default collection on the graph variant specified by the --apollo-graph-ref arg

## 0.4.1 (2025-06-20)

### 🐛 Fixes

#### Fix tool update on every poll - @Jephuff PR #146

Only update the tool list if an operation was removed, changed, or added.

## 0.4.0 (2025-06-17)

### 🚀 Features

#### Add `--collection <COLLECTION_ID>` as another option for operation source - @Jephuff PR #118

Use operation collections as the source of operations for your MCP server. The server will watch for changes and automatically update when you change your operation collection.

#### Allow overriding registry endpoints - @Jephuff PR #134

Set APOLLO_UPLINK_ENDPOINTS and APOLLO_REGISTRY_URL to override the endpoints for fetching schemas and operations

#### Add client metadata to GraphQL requests - @pubmodmatt PR #137

The MCP Server will now identify itself to Apollo Router through the `ApolloClientMetadata` extension. This allows traffic from MCP to be identified in the router, for example through telemetry.

#### Update license to MIT - @kbychu PR #122

The Apollo MCP Server is now licensed under MIT instead of ELv2

### 🐛 Fixes

#### Fix GetAstronautsCurrentlyInSpace query - @pubmodmatt PR #114

The `GetAstronautsCurrentlyInSpace` in the Quickstart documentation was not working.

#### Change explorer tool to return URL - @pubmodmatt PR #123

The explorer tool previously opened the GraphQL query directly in the user's browser. Although convenient, this would only work if the MCP Server was hosted on the end user's machine, not remotely. It will now return the URL instead.

#### Fix bug in operation directory watching - @pubmodmatt PR #135

Operation directory watching would not trigger an update of operations in some cases.

#### fix: handle headers with colons in value - @DaleSeo PR #128

The MCP server won't crash when a header's value contains colons.

### 🛠 Maintenance

#### Automate changesets and changelog - @pubmodmatt PR #107

Contributors can now generate a changeset file automatically with:

```console
cargo xtask changeset create
```

This will generate a file in the `.changesets` directory, which can be added to the pull request.

### [0.3.0] - 2025-05-29

#### 🚀 Features

- Implement the Streamable HTTP transport. Enable with `--http-port` and/or `--http-address`. (#98)
- Include both the type description and field description in input schema (#100)
- Hide String, ID, Int, Float, and Boolean descriptions in input schema (#100)
- Set the `readOnlyHint` tool annotation for tools based on GraphQL query operations (#103)

#### 🐛 Fixes

- Fix error with recursive input types (#100)

### [0.2.1] - 2025-05-27

#### 🐛 Fixes

- Reduce the log level of many messages emitted by the server so INFO is less verbose, and add a `--log` option to specify the log level used by the MCP Server (default is INFO) (#82)
- Ignore mutations and subscriptions rather than erroring out (#91)
- Silence \_\_typename used in operations errors (#79)
- Fix issues with the `introspect` tool. (#83)
  - The tool was not working when there were top-level subscription in the schema
  - Argument types were not being resolved correctly
- Improvements to operation loading (#80)
  - When specifying multiple operation paths, all paths were reloaded when any one changed
  - Many redundant events were sent on startup, causing verbose logging about loaded operations
  - Better error handling for missing, invalid, or empty operation files
- The `execute` tool did not handle variables correctly (#77 and #93)
- Cycles in schema type definitions would lead to stack overflow (#74)

### [0.2.0] - 2025-05-21

#### 🚀 Features

- The `--operations` argument now supports hot reloading and directory paths. If a directory is specified, all .graphql files in the directory will be loaded as operations. The running server will update when files are added to or removed from the directory. (#69)
- Add an optional `--sse-address` argument to set the bind address of the MCP server. Defaults to 127.0.0.1. (#63)

#### 🐛 Fixes

- Fixed PowerShell script (#55)
- Log to stdout, not stderr (#59)
- The `--directory` argument is now optional. When using the stdio transport, it is recommended to either set this option or use absolute paths for other arguments. (#64)

#### 📚 Documentation

- Fix and simplify the example `rover dev --mcp` commands

### [0.1.0] - 2025-05-15

#### 🚀 Features

- Initial release of the Apollo MCP Server
