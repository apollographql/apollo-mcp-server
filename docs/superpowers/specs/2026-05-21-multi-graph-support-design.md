# Multi-Graph Support — Design

**Date:** 2026-05-21
**Status:** Draft
**Scope:** Fork of `apollo-mcp-server` that exposes operations from multiple GraphQL APIs under one MCP server.

## Goals (v1)

1. One MCP server process serves N independent GraphQL graphs (own endpoint, own schema, own operations).
2. Single global `search` tool, optional `graph` arg. Omitted → searches every graph in parallel and tags results with the graph id.
3. `execute`, `introspect`, `validate` tools each take a **required** `graph` arg.
4. Operation-as-tool names are prefixed: `<graph>__<OperationName>`.
5. Two configuration loaders, built in parallel: a local YAML manifest and an OCI image manifest.
6. All graphs loaded once at startup. No per-graph hot reload.
7. Unknown `graph` value → MCP error listing the valid graph names.
8. Design (but do not build) the seam for v2 per-graph upstream auth: a typed `UpstreamAuthRequired { graph, www_authenticate }` error returned when an upstream replies 401.

## Non-Goals (deferred)

- Per-graph hot reload of schema or operations.
- Per-graph upstream credential storage and OAuth flow.
- Server-mediated re-auth UI/protocol.
- Federation/composition across the configured graphs.
- Per-graph override of MCP-server-level features (logging, telemetry, CORS, transport, auth).

## Architecture

### `GraphContext` — the new unit of isolation

```rust
pub struct GraphContext {
    pub name: String,                          // namespace, e.g. "mercedes"
    pub schema: Arc<RwLock<Valid<Schema>>>,    // own schema
    pub endpoint: Url,                          // own upstream
    pub headers: HeaderMap,                     // baseline upstream headers
    pub forward_headers: ForwardHeaders,        // which client headers to pass through
    pub operations: Arc<RwLock<Vec<Operation>>>, // prefixed names already applied
    pub search_index: SchemaIndex,              // per-graph Tantivy index
    pub mutation_mode: MutationMode,
    pub custom_scalar_map: Option<CustomScalarMap>,

    // v2 seam — v1 impl returns `self.headers` unchanged.
    pub credentials: Arc<dyn CredentialProvider>,
}

pub trait CredentialProvider: Send + Sync {
    fn headers_for(&self, base: &HeaderMap, user: Option<&UserId>) -> HeaderMap;
}
```

`Running` becomes:

```rust
pub(super) struct Running {
    pub(super) graphs: HashMap<String, GraphContext>,
    pub(super) execute_tool: Option<Execute>,
    pub(super) search_tool: Option<Search>,
    pub(super) introspect_tool: Option<Introspect>,
    pub(super) validate_tool: Option<Validate>,
    // unchanged: apps, prompts, peers, cancellation_token, server_info, instructions, rhai_engine, etc.
}
```

The four "introspection tools" are now **dispatchers**. They hold no schema themselves; on call they parse the `graph` arg, look up the `GraphContext`, and delegate.

### Tool dispatch

| Tool | `graph` arg | On missing | On unknown |
|---|---|---|---|
| `search` | optional | search all graphs in parallel; tag results | error |
| `execute` | required | error | error |
| `introspect` | required | error | error |
| `validate` | required | error | error |
| `<graph>__<Op>` | — (encoded in name) | n/a | METHOD_NOT_FOUND |

Errors for unknown graphs return MCP `INVALID_PARAMS` with a `data` payload listing valid graph names.

#### Search — multi-index

When `graph` is omitted, run `search` on every `GraphContext::search_index` concurrently (`futures::future::join_all`), then merge. Each returned `Content::text` block is prefixed with a graph tag, e.g.:

```
# graph: mercedes
type Vehicle { ... }

# graph: parts
type Part { ... }
```

Existing `MAX_SEARCH_RESULTS = 5` becomes a per-graph cap; the merged output is capped at `5 * N_graphs` (revisit if it gets noisy in practice).

#### Execute — routing

`Execute::Input` gains a required `graph: String` field. On call:

1. Look up `GraphContext` by name.
2. Build headers via `ctx.credentials.headers_for(&ctx.headers, user)` then merge `forward_headers` from `axum_parts` (existing logic, unchanged).
3. POST to `ctx.endpoint`.
4. If upstream responds 401, surface `UpstreamAuthRequired { graph, www_authenticate }` as a structured tool error.

#### Operation tools

`RawOperation::into_operation` already produces a `Tool` named after the GraphQL operation. We add a `name_prefix: Option<&str>` argument (or post-process the resulting `Operation`) so the registered tool name is `mercedes__GetVehicle` instead of `GetVehicle`. Internal lookup keys also use the prefixed name.

### Manifest format

The manifest enumerates graphs and where each graph's files live. The same manifest type is used by both the local file loader and the OCI loader — the only difference is how files are fetched (filesystem vs OCI layer blobs).

```yaml
# graphs.yaml
version: 1
graphs:
  - name: mercedes
    endpoint: https://api.mercedes.example.com/graphql
    schema: ./mercedes/schema.graphql
    operations:
      - ./mercedes/operations/*.graphql
    headers:
      authorization: Bearer ${env.MERCEDES_TOKEN}
  - name: parts
    endpoint: https://parts.internal/graphql
    schema: ./parts/schema.graphql
    operations:
      - ./parts/operations/list.graphql
```

OCI variant: the image's annotations carry the manifest filename (`org.apollographql.mcp.manifest=graphs.yaml`); layers carry the referenced files. Loader pulls the image, extracts layers to a temp dir, parses the manifest as if it were local.

Config top-level gains:

```yaml
graphs:
  source:
    type: local    # or: oci
    manifest: ./graphs.yaml        # local
    # image: ghcr.io/acme/mcp-bundle:v1   # oci
```

Existing top-level `schema`, `operations`, `endpoint`, and per-graph `headers` keys are **removed** in this fork. A single-graph deployment is just a manifest with one entry.

### State machine

The four-state machine (`Configuring → SchemaConfigured → OperationsConfigured → Running`) collapses to two for multi-graph:

```
Configuring → Loading → Running
```

`Loading` resolves the manifest, fetches every graph's schema and operations (in parallel), validates each, builds per-graph `Operation` lists and search indexes. Any graph failing validation is **fatal** at startup (no partial-success mode in v1 — clearer errors, simpler ops). Once all graphs are ready, transition once to `Running`.

`ConfigChanged` / SIGHUP behavior is unchanged: full restart.

### Error model

New variant in the existing error enum used by `execute`:

```rust
pub enum UpstreamError {
    Graphql(graphql::Error),
    Http(reqwest::Error),
    AuthRequired { graph: String, www_authenticate: Option<String> },
}
```

`AuthRequired` is detected by checking `response.status() == 401` on the upstream POST. The MCP `CallToolResult` returned to the client carries the graph id and the upstream's `WWW-Authenticate` header in `structured_content`, so a future v2 mediator can react.

In v1 the LLM/client just sees an error with enough text to know "graph X needs auth." No interactive flow is started.

### Telemetry

Add `apollo.mcp.graph_name` as a span attribute on `call_tool` and a label on the existing tool metrics. The build script that materializes `TelemetryAttribute` already supports this pattern (`telemetry.toml`).

## File-level change map

- `crates/apollo-mcp-server/src/server.rs` — `Server` builder takes `Vec<GraphConfig>` or `ManifestSource` instead of single `schema_source`/`operation_source`/`endpoint`/`headers`.
- `crates/apollo-mcp-server/src/server/states.rs` — collapse states; new `Loading`.
- `crates/apollo-mcp-server/src/server/states/running.rs` — `Running.graphs`, dispatcher rewrites.
- `crates/apollo-mcp-server/src/introspection/tools/{search,execute,introspect,validate}.rs` — add `graph` arg, become dispatchers that delegate to `GraphContext`.
- `crates/apollo-mcp-server/src/operations/raw_operation.rs` — accept `name_prefix` when materializing `Operation`.
- `crates/apollo-mcp-server/src/runtime/config.rs` — new `graphs:` section, remove single-graph top-level keys.
- `crates/apollo-mcp-server/src/runtime/manifest/` *(new)* — `Manifest`, `LocalLoader`, `OciLoader`, shared types.
- `crates/apollo-mcp-server/src/graphs/` *(new)* — `GraphContext`, `CredentialProvider`, factory from manifest entry.
- `telemetry.toml` — add `apollo.mcp.graph_name` attribute.

## Test plan

- Unit: search dispatcher with 0/1/N graphs; unknown graph error shape; tool-name prefixing; operation invalidation when its graph's schema changes (not relevant in v1 but covered by structural tests so v2 can lean on it).
- Snapshot: search output across two test schemas, tagged by graph.
- Integration: stand up two mock GraphQL servers via `mockito`; assert `execute { graph: "a" }` routes to A and `execute { graph: "b" }` routes to B; 401 from A surfaces as `AuthRequired { graph: "a" }`.
- Manifest: local loader parses example manifest end-to-end; OCI loader parses against a fixture image (use `ocipkg` or similar test harness — pick the lightest viable lib during planning).

## Open questions to resolve in the implementation plan

1. Exact crate for OCI image fetching. (`ocipkg`, `oci-distribution`, or pull-and-shell-out.) Doesn't block design.
2. Per-graph cap vs. global cap on `search` result count when `graph` is omitted.
3. Whether `<graph>__<Op>` separator stays `__` (LLM-friendly, unambiguous) or becomes `.` (matches GraphQL field-path conventions). `__` recommended.
4. `validate` semantics when the query references types not in the named graph's schema — surface as a regular validation error (yes, recommended) or escalate.

## Deferred to v2

- Hot reload: per-graph schema/ops watcher → swap that one `GraphContext` without restart.
- Credential mediation: real `CredentialProvider` impls; per-user credential store; OAuth flows; `WWW-Authenticate`-driven elicitation.
- Per-graph telemetry overrides, rate limits, retry policies.
