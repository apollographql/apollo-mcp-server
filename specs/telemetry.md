# Apollo MCP Server Telemetry Spec

| Category                  | Metric / Trace / Event                                                   | Type            | Attributes                                                         | Notes                                                                   | Priority      |
|---------------------------|--------------------------------------------------------------------------|-----------------|--------------------------------------------------------------------|-------------------------------------------------------------------------|---------------|
| **Configuration**         | `apollo_mcp.config.load_success`                                        | Counter         | error_type                                                         | Successful config / startup loads                                       | Must Have     |
|                           | `apollo_mcp.config.load_failure`                                        | Counter         | error_type                                                         | Failed startup (bad schema, manifest, endpoint)                         | Must Have     |
|                           | `apollo_mcp.tools.registered{source="builtin:introspect"}`              | Gauge           | —                                                                  | Introspect tool present if enabled (always =1)                         | Must Have     |
|                           | `apollo_mcp.tools.registered{source="builtin:search"}`                  | Gauge           | —                                                                  | Search tool present if enabled (always =1)                             | Must Have     |
|                           | `apollo_mcp.tools.registered{source="persisted_query"}`                 | Gauge           | —                                                                  | # of tools from persisted query manifest                               | Must Have     |
|                           | `apollo_mcp.tools.registered{source="operation_collection"}`            | Gauge           | —                                                                  | # of tools from operation collections                                  | Must Have     |
|                           | `apollo_mcp.tools.registered{source="graphql_file"}`                    | Gauge           | —                                                                  | # of tools from `.graphql` files                                       | Should Have   |
|                           | `apollo_mcp.tools.registered{source="introspection_generated"}`         | Gauge           | —                                                                  | # of tools auto-generated from schema introspection                    | Should Have   |
|                           | `apollo_mcp.schema.source`                                              | Attribute/Event | uplink, local_file, introspection                                 | Where schema was loaded from                                            | Must Have     |
|                           | `apollo_mcp.schema.load_success` / `apollo_mcp.schema.load_failure`     | Counter         | schema_source                                                      | Schema load status                                                      | Must Have     |
|                           | `apollo_mcp.schema.size`                                                | Gauge           | —                                                                  | # of types/fields in schema                                             | Should Have   |
|                           | `apollo_mcp.version.info`                                               | Attribute/Event | server_version, schema_hash, manifest_version, manifest_source        | Server binary version, GraphQL schema hash, manifest version, manifest type (persisted_query/operation_collection) | Must Have     |
| **Usage**                 | `apollo.mcp.calls`                                                      | Counter         | tool_name, success, error_code, client_type                       | Total tool invocations                                                  | Must Have     |
|                           | `apollo.mcp.calls.latency`                                              | Histogram       | tool_name, success, error_code, client_type                       | End-to-end request latency                                              | Must Have     |
|                           | `apollo.mcp.operation.calls`                                            | Counter         | tool_name, success, error_code, client_type, operation_name       | # of backend GraphQL operations executed                                | Must Have     |
|                           | `apollo.mcp.operation.latency`                                          | Histogram       | tool_name, success, error_code, client_type, operation_name       | Latency of GraphQL backend call (excludes tool overhead)               | Must Have     |
|                           | `apollo_mcp.operation.type.mix`                                         | Counter         | query, mutation, subscription                                      | Breakdown of operation types                                            | Should Have   |
|                           | `apollo_mcp.responses.size`                                             | Histogram       | tool_name, client_type                                             | Size of responses (bytes)                                               | Should Have   |
|                           | `apollo_mcp.responses.characters`                                        | Histogram       | tool_name, client_type                                             | Character count of response payloads (proxy for token estimation)      | Nice to Have  |
|                           | `apollo_mcp.clients.active`                                             | Gauge           | —                                                                  | # of active MCP clients                                                 | Must Have     |
|                           | `apollo_mcp.concurrency.current_requests`                               | Gauge           | —                                                                  | # of concurrent tool executions                                         | Should Have   |
| **Errors / Reliability**  | `apollo_mcp.requests.errors`                                            | Counter         | error_type, tool_name, client_type                                | Failed tool calls (generic catch-all)                                  | Must Have     |
|                           | `apollo_mcp.graphql.backend.errors`                                     | Counter         | status_code, operation_name                                        | Errors from upstream GraphQL API                                       | Must Have     |
|                           | `apollo_mcp.transport.errors`                                           | Counter         | error_type                                                         | Invalid JSON-RPC, dropped connections                                  | Should Have   |
|                           | `apollo_mcp.auth.failures`                                              | Counter         | reason, client_type                                                | Authentication failures                                                 | Must Have     |
|                           | `apollo_mcp.timeouts`                                                   | Counter         | tool_name, client_type                                             | Tool or backend operation timed out                                     | Must Have     |
| **Traces**                | Span: `mcp.tool_invocation`                                             | Trace           | tool_name, latency, success                                       | Span for each tool invocation                                           | Must Have     |
|                           | Span: `graphql.operation`                                               | Trace           | operation_name, latency, success, error_code                      | Child span for backend GraphQL operation                                | Must Have     |
|                           | Span: `serialization`                                                   | Trace           | size_bytes, latency                                               | Encoding/decoding JSON-RPC overhead                                     | Nice to Have  |
| **Events**                | `apollo_mcp.client.connected`                                           | Event           | client_type                                                        | Client connection established                                           | Should Have   |
|                           | `apollo_mcp.client.disconnected`                                        | Event           | client_type                                                        | Client disconnected                                                     | Should Have   |
|                           | `apollo_mcp.config.reload`                                              | Event           | schema_source, version_hash                                        | Config/schema/manifest/collection reload                                | Nice to Have  |
|                           | `apollo_mcp.auth.failed`                                                | Event           | client_type, reason                                                | Auth failure                                                            | Must Have     |
| **Resource Usage**        | `process.cpu.usage`                                                     | Gauge           | —                                                                  | CPU usage of MCP server process                                         | Nice to Have  |
|                           | `process.memory.usage`                                                  | Gauge           | —                                                                  | Memory usage                                                            | Nice to Have  |
|                           | `network.bytes.sent` / `network.bytes.received`                         | Counter         | —                                                                  | Network traffic                                                         | Nice to Have  |

## Implementation Notes

### Client Identification Usage
**`client_type` only:**
- Direct client interactions: calls, operation calls/latency, response size, token estimation, timeouts
- Error analysis: request errors, auth failures
- Connection events: client connected/disconnected, auth failed  
- Purpose: Analyze client behavior patterns and identify client-specific issues

**No client identification:**
- Server configuration: config loads, tool registration, schema info, version info
- System metrics: CPU, memory, network, active clients, concurrency
- Backend operations: GraphQL backend errors, operation type mix, transport errors
- Traces: tool invocation spans, GraphQL operations, serialization
- Purpose: Server-wide metrics and request-level tracing independent of client behavior

### Client Identification Implementation
- **`client_type`**: Static client identifier derived from User-Agent header or configuration
  - Examples: `"claude"`, `"chatgpt"`, `"vscode"`, `"custom"`, `"unknown"`
  - Used for understanding client behavior patterns and performance differences
  - No PII concerns - represents client software type, not individual users
  - Optional: Use `"unknown"` if client type cannot be determined or for privacy

### Privacy & Retention
- Client identification is optional - use `"unknown"` if privacy concerns exist
- No PII concerns with `client_type` - it represents software, not users
- Ensure compliance with local data protection regulations

### Token & Cost Estimation
- **Real-time**: Use `apollo_mcp.responses.characters` for fast proxy estimation
  - Rule of thumb: 1 token ≈ 3-4 characters for most content
  - No performance impact - just `response.length`
- **Offline/Optional**: For precise token counts, run tokenization in background jobs
  - Sample a subset of responses (e.g., 1-10%) to avoid performance impact
  - Use established tokenizers (tiktoken for OpenAI models, similar for others)
  - Store results separately from real-time metrics
  - Actual token counts will vary by model and tokenizer

### Configuration Metrics
- Probably useful only for Apollo
