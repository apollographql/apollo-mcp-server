# Apollo MCP Server Telemetry Spec

| Category                  | Metric / Trace / Event                                                   | Type            | Attributes                                                         | Notes                                                                   | 3rd party     | Apollo        | Priority      |
|---------------------------|--------------------------------------------------------------------------|-----------------|--------------------------------------------------------------------|-------------------------------------------------------------------------|---------------|---------------|---------------|
| **Configuration**         | `apollo.mcp.config.load`                                         | Counter         | success                                                         | config / startup loads                                       | yes            | yes           | Should Have   |
|                           | `apollo.mcp.tools.registered{source="builtin:introspect"}`              | Gauge           | —                                                                  | Introspect tool present if enabled (always =1)                         | yes           | yes           | Should Have   |
|                           | `apollo.mcp.tools.registered{source="builtin:search"}`                  | Gauge           | —                                                                  | Search tool present if enabled (always =1)                             | yes           | yes           | Should Have   |
|                           | `apollo.mcp.tools.registered{source="persisted_query"}`                 | Gauge           | —                                                                  | # of tools from persisted query manifest                               | yes           | yes           | Should Have   |
|                           | `apollo.mcp.tools.registered{source="operation_collection"}`            | Gauge           | —                                                                  | # of tools from operation collections                                  | yes           | yes           | Should Have   |
|                           | `apollo.mcp.tools.registered{source="graphql_file"}`                    | Gauge           | —                                                                  | # of tools from `.graphql` files                                       | yes           | yes           | Should Have   |
|                           | `apollo.mcp.tools.registered{source="introspection_generated"}`         | Gauge           | —                                                                  | # of tools auto-generated from schema introspection                    | yes           | yes           | Should Have   |
|                           | `apollo.mcp.schema.source`                                              | Attribute/Event | uplink, local_file, introspection                                 | Where schema was loaded from                                            | yes           | yes           | Should Have   |
|                           | `apollo.mcp.schema.load`     | Counter         | schema_source, success                                                      | Schema load status                                                      | yes           | yes           | Should Have   |
|                           | `apollo.mcp.schema.size`                                                | Gauge           | —                                                                  | # of types/fields in schema                                             | no            | yes           | Should Have   |
|                           | `apollo.mcp.version.info`                                               | Attribute/Event | server_version, schema_hash, manifest_version, manifest_source        | Server binary version, GraphQL schema hash, manifest version, manifest type (persisted_query/operation_collection) | yes            | yes           | Should Have   |
| **Usage**                 | `apollo.mcp.tool.call.count`                                                      | Counter         | tool_name, success, error_code, client_type                       | Total tool invocations                                                  | yes           | yes           | Must Have     |
|                           | `apollo.mcp.tool.call.duration`                                              | Histogram       | tool_name, success, error_code, client_type                       | End-to-end request latency                                              | yes           | yes           | Must Have     |
|                           | `apollo.mcp.graphql.operation.count`                                            | Counter         | tool_name, success, error_code, client_type, operation_name, operation_type       | # of backend GraphQL operations executed                                | yes           | yes           | Must Have     |
|                           | `apollo.mcp.graphql.operation.duration`                                          | Histogram       | tool_name, success, error_code, client_type, operation_name, operation_type       | Latency of GraphQL backend call (excludes tool overhead)               | yes           | yes           | Must Have     |
|                           | `apollo.mcp.responses.size`                                             | Histogram       | tool_name, client_type                                             | Size of responses (bytes)                                               | yes           | yes           | Should Have   |
|                           | `apollo.mcp.responses.characters`                                        | Histogram       | tool_name, client_type                                             | Character count of response payloads (proxy for token estimation)      | yes           | yes           | Nice to Have  |
|                           | `apollo.mcp.clients.active`                                             | Gauge           | —                                                                  | # of active MCP clients                                                 | yes           | yes           | Must Have     |
|                           | `apollo.mcp.concurrency.current_requests`                               | Gauge           | —                                                                  | # of concurrent tool executions                                         | yes           | yes           | Should Have   |
|                           | `apollo.mcp.auth.failures`                                              | Counter         | reason, client_type                                                | Authentication failures                                                 | yes           | yes           | Must Have     |
|                           | `apollo.mcp.timeouts`                                                   | Counter         | tool_name, client_type                                             | Tool or backend operation timed out                                     | yes           | yes           | Must Have     |
| **Traces**                | Span: `apollo.mcp.tool_invocation`                                             | Trace           | tool_name, latency, success                                       | Span for each tool invocation                                           | yes           | yes           | Must Have     |
|                           | Span: `apollo.mcp.graphql.operation`                                               | Trace           | operation_name, latency, success, error_code                      | Child span for backend GraphQL operation                                | yes           | yes           | Must Have     |
|                           | Span: `serialization`                                                   | Trace           | size_bytes, latency                                               | Encoding/decoding JSON-RPC overhead                                     | no            | yes           | Nice to Have  |
| **Events**                | `apollo.mcp.client.connected`                                           | Event           | client_type                                                        | Client connection established                                           | yes           | yes           | Should Have   |
|                           | `apollo.mcp.client.disconnected`                                        | Event           | client_type                                                        | Client disconnected                                                     | yes           | yes           | Should Have   |
|                           | `apollo.mcp.config.reload`                                              | Event           | schema_source, version_hash                                        | Config/schema/manifest/collection reload                                | no            | yes           | Nice to Have  |
|                           | `apollo.mcp.auth.failed`                                                | Event           | client_type, reason                                                | Auth failure                                                            | yes           | yes           | Must Have     |
| **HTTP Metrics**          | `http.server.request.duration`                                                     | Histogram           | —                                                                  | Duration of HTTP server requests.	                                         | yes           | yes           | Nice to Have  |
|                           | `http.server.active_requests`                                                  | Counter           | —                                                                  | Number of active HTTP server requests.                                                           | yes           | yes           | Nice to Have  |
|                           | `http.server.request.body.size`                         | Histogram         | —                                                                  | 	Size of HTTP server request bodies.                                                         | yes           | yes           | Nice to Have  |
|                           | `http.server.response.body.size`                         | Histogram         | —                                                                  | Size of HTTP server response bodies.                                                       | yes           | yes           | Nice to Have  |
| **Query Analysis**        | `apollo.mcp.query.depth.max`                                              | Histogram       | tool_name, operation_name                                          | Maximum selection depth in query                                           | no            | yes           | Nice to Have  |
|                           | `apollo.mcp.query.fields.total`                                           | Histogram       | tool_name, operation_name                                          | Total number of fields selected                                             | no            | yes           | Nice to Have  |
|                           | `apollo.mcp.query.fields.leaf`                                            | Histogram       | tool_name, operation_name                                          | Number of leaf fields selected                                              | no            | yes           | Nice to Have  |
|                           | `apollo.mcp.query.breadth.max`                                            | Histogram       | tool_name, operation_name                                          | Maximum breadth at any level                                               | no            | yes           | Nice to Have  |
|                           | `apollo.mcp.query.shape.pattern`                                          | Counter         | tool_name, pattern_type                                            | Categorized patterns: "shallow_broad", "deep_narrow", "mixed"              | no            | yes           | Nice to Have  |
|                           | `apollo.mcp.query.directives.skip`                                        | Counter         | tool_name, operation_name                                          | Usage of @skip directive                                                   | no            | yes           | Nice to Have  |
|                           | `apollo.mcp.query.directives.include`                                     | Counter         | tool_name, operation_name                                          | Usage of @include directive                                                | no            | yes           | Nice to Have  |
|                           | `apollo.mcp.query.aliases.count`                                          | Histogram       | tool_name, operation_name                                          | Number of field aliases used                                               | no            | yes           | Nice to Have  |
|                           | `apollo.mcp.query.fragments.count`                                        | Histogram       | tool_name, operation_name                                          | Number of fragments used                                                   | no            | yes           | Nice to Have  |
|                           | `apollo.mcp.query.variables.count`                                        | Histogram       | tool_name, operation_name                                          | Number of variables used                                                   | no            | yes           | Nice to Have  |




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

### Query Analysis
**Implementation Requirements**: Add GraphQL AST parsing to the MCP Server to analyze queries before forwarding them to the backend.

**Current Architecture**: MCP Server acts as a proxy, forwarding query strings without parsing. Query complexity analysis requires adding a GraphQL parser dependency (e.g., `graphql-parser` crate) to parse queries into AST before execution.

**Alternative: Router-Based Analysis**: Apollo Router already captures query complexity metrics, but correlating Router data with MCP tool calls would require users to configure both systems with matching headers/trace IDs - an unrealistic deployment requirement.

**Zero-Configuration Approach**: Implement AST parsing directly in MCP Server for immediate, out-of-the-box insights without external coordination.

**Performance Considerations**:
- AST parsing overhead is minimal compared to network/GraphQL execution time
- Optional sampling (e.g., 10% of queries) can further reduce overhead if needed
- Analysis happens once per tool call, not per field resolution

**Pattern Detection**:
- **Shallow vs Deep**: Track max depth and breadth to identify query patterns
- **Advanced Features**: Count usage of directives, aliases, fragments, variables
- **Categorization**: Automatically classify as "shallow_broad", "deep_narrow", or "mixed" based on depth/breadth ratios

This approach provides immediate insights into MCP tool usage patterns without requiring users to configure multiple systems.
