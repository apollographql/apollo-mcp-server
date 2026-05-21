# Multi-Graph Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fork `apollo-mcp-server` to expose operations from N independent GraphQL graphs under one MCP server. One global `search` (optional `graph`), one each of `execute`/`introspect`/`validate` (required `graph`), and operation-as-tool names prefixed with `<graph>__`.

**Architecture:** Introduce a `GraphContext` value type (one per graph: schema, endpoint, headers, ops, search index, credential seam). `Running` becomes a `HashMap<String, GraphContext>`. The four built-in tools become thin dispatchers that route by `graph` arg. Multi-graph configuration loads via a `Manifest` consumed by either a local YAML loader or an OCI image loader. State machine collapses to `Configuring → Loading → Running`. v2 upstream-auth is reserved via a `CredentialProvider` trait + typed `UpstreamAuthRequired` error variant; no behavior is wired in v1.

**Tech Stack:** Rust 2021, tokio, axum 0.8, rmcp 0.14, apollo-compiler, apollo-federation, apollo-mcp-registry (already in workspace), Tantivy (via `apollo-schema-index`), `oci-distribution` (for the OCI loader), `mockito` for tests, `rstest` for parameterized tests, `insta` for snapshots.

**Reference spec:** `docs/superpowers/specs/2026-05-21-multi-graph-support-design.md`

---

## Phase 1: Manifest Types and Local Loader

### Task 1: Define `Manifest` and `GraphConfig` types

**Files:**
- Create: `crates/apollo-mcp-server/src/runtime/manifest/mod.rs`
- Create: `crates/apollo-mcp-server/src/runtime/manifest/types.rs`
- Modify: `crates/apollo-mcp-server/src/runtime.rs` (add `pub mod manifest;`)

- [ ] **Step 1: Write the failing test**

Create `crates/apollo-mcp-server/src/runtime/manifest/types.rs`:

```rust
use std::path::PathBuf;
use schemars::JsonSchema;
use serde::Deserialize;
use url::Url;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub graphs: Vec<GraphConfig>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GraphConfig {
    pub name: String,
    #[schemars(schema_with = "Url::json_schema")]
    pub endpoint: Url,
    pub schema: PathBuf,
    #[serde(default)]
    pub operations: Vec<String>,
    #[serde(default)]
    pub headers: std::collections::HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_parses_a_minimal_manifest() {
        let yaml = r#"
            version: 1
            graphs:
              - name: a
                endpoint: http://localhost:4000/
                schema: ./a/schema.graphql
        "#;
        let m: Manifest = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(m.version, 1);
        assert_eq!(m.graphs.len(), 1);
        assert_eq!(m.graphs[0].name, "a");
        assert_eq!(m.graphs[0].operations.len(), 0);
    }

    #[test]
    fn it_rejects_unknown_fields() {
        let yaml = r#"
            version: 1
            graphs: []
            extra: field
        "#;
        let result: Result<Manifest, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err());
    }

    #[test]
    fn it_requires_name_endpoint_schema() {
        let yaml = r#"
            version: 1
            graphs:
              - endpoint: http://x/
        "#;
        let result: Result<Manifest, _> = serde_yaml::from_str(yaml);
        assert!(result.is_err());
    }
}
```

Create `crates/apollo-mcp-server/src/runtime/manifest/mod.rs`:

```rust
pub mod types;
pub use types::{GraphConfig, Manifest};
```

Modify `crates/apollo-mcp-server/src/runtime.rs` — add line near other `pub mod` lines:

```rust
pub mod manifest;
```

- [ ] **Step 2: Run tests to verify they pass**

```bash
cargo test -p apollo-mcp-server --lib runtime::manifest
```

Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/apollo-mcp-server/src/runtime/manifest crates/apollo-mcp-server/src/runtime.rs
git commit -m "feat(multi-graph): add Manifest and GraphConfig types"
```

---

### Task 2: Local manifest loader (parse + resolve relative paths)

**Files:**
- Create: `crates/apollo-mcp-server/src/runtime/manifest/local.rs`
- Modify: `crates/apollo-mcp-server/src/runtime/manifest/mod.rs`
- Test: same file

- [ ] **Step 1: Write the failing test**

Create `crates/apollo-mcp-server/src/runtime/manifest/local.rs`:

```rust
use std::path::{Path, PathBuf};

use super::types::Manifest;

#[derive(Debug, thiserror::Error)]
pub enum LocalLoadError {
    #[error("failed to read manifest file {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("failed to parse manifest YAML: {0}")]
    Parse(#[from] serde_yaml::Error),
}

/// Load a manifest from a YAML file on disk. Relative file paths inside the
/// manifest (schema, operations) are resolved against the manifest's parent dir.
pub fn load_local(path: &Path) -> Result<Manifest, LocalLoadError> {
    let text = std::fs::read_to_string(path).map_err(|source| LocalLoadError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut manifest: Manifest = serde_yaml::from_str(&text)?;

    if let Some(parent) = path.parent() {
        for g in &mut manifest.graphs {
            if g.schema.is_relative() {
                g.schema = parent.join(&g.schema);
            }
            for op in &mut g.operations {
                let op_path = PathBuf::from(&op);
                if op_path.is_relative() {
                    *op = parent.join(op_path).to_string_lossy().into_owned();
                }
            }
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn it_loads_and_resolves_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let manifest_path = dir.path().join("graphs.yaml");
        let mut f = std::fs::File::create(&manifest_path).unwrap();
        write!(
            f,
            "version: 1\n\
             graphs:\n\
             - name: a\n\
             \x20 endpoint: http://localhost:4000/\n\
             \x20 schema: ./a/schema.graphql\n\
             \x20 operations:\n\
             \x20   - ./a/ops/list.graphql\n"
        )
        .unwrap();
        drop(f);

        let manifest = load_local(&manifest_path).unwrap();
        let g = &manifest.graphs[0];
        assert_eq!(g.schema, dir.path().join("a/schema.graphql"));
        assert_eq!(g.operations[0], dir.path().join("a/ops/list.graphql").to_string_lossy());
    }

    #[test]
    fn it_returns_an_error_when_file_missing() {
        let err = load_local(Path::new("/nonexistent/manifest.yaml")).unwrap_err();
        assert!(matches!(err, LocalLoadError::Read { .. }));
    }
}
```

Add to `runtime/manifest/mod.rs`:

```rust
pub mod local;
pub mod types;
pub use local::{LocalLoadError, load_local};
pub use types::{GraphConfig, Manifest};
```

Add `tempfile` to `crates/apollo-mcp-server/Cargo.toml` under `[dev-dependencies]` if not already present:

```bash
cargo add --dev tempfile --package apollo-mcp-server
```

- [ ] **Step 2: Run tests to verify they pass**

```bash
cargo test -p apollo-mcp-server --lib runtime::manifest::local
```

Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/apollo-mcp-server/src/runtime/manifest crates/apollo-mcp-server/Cargo.toml
git commit -m "feat(multi-graph): add local manifest loader"
```

---

### Task 3: New `graphs:` config section, remove single-graph top-level keys

**Files:**
- Modify: `crates/apollo-mcp-server/src/runtime/config.rs`
- Create: `crates/apollo-mcp-server/src/runtime/graphs_source.rs`
- Modify: `crates/apollo-mcp-server/src/runtime.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/apollo-mcp-server/src/runtime/graphs_source.rs`:

```rust
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;

/// Where to load the multi-graph manifest from.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum GraphsSource {
    /// Load the manifest from a YAML file on the local filesystem.
    Local { manifest: PathBuf },
    /// Pull an OCI image and read the manifest from one of its layers.
    Oci { image: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_parses_local_source() {
        let yaml = "type: local\nmanifest: ./graphs.yaml\n";
        let s: GraphsSource = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(s, GraphsSource::Local { .. }));
    }

    #[test]
    fn it_parses_oci_source() {
        let yaml = "type: oci\nimage: ghcr.io/acme/bundle:v1\n";
        let s: GraphsSource = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(s, GraphsSource::Oci { .. }));
    }
}
```

Add to `runtime.rs`:

```rust
pub mod graphs_source;
```

Modify `crates/apollo-mcp-server/src/runtime/config.rs` to replace the single-graph top-level fields with `graphs`. Replace the entire `pub struct Config { ... }` block with:

```rust
use super::graphs_source::GraphsSource;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// CORS configuration
    pub cors: CorsConfig,

    /// Server metadata configuration
    #[serde(default)]
    pub server_info: ServerInfoConfig,

    /// Optional instructions returned in the MCP `initialize` response.
    #[serde(default)]
    pub instructions: Option<String>,

    /// Path to a custom scalar map
    pub custom_scalars: Option<PathBuf>,

    /// Multi-graph configuration source
    pub graphs: GraphsSource,

    /// Apollo-specific credential overrides (still used for GraphOS Studio
    /// telemetry; no longer used for schema/operations).
    pub graphos: GraphOSConfig,

    /// Hard-coded headers included on every upstream request (applies to every
    /// graph). Per-graph headers in the manifest override these.
    #[serde(deserialize_with = "parsers::map_from_str")]
    #[schemars(schema_with = "super::schemas::header_map")]
    pub headers: HeaderMap,

    /// Header names to forward from MCP client to GraphQL upstreams (every graph).
    #[serde(default)]
    pub forward_headers: ForwardHeaders,

    /// Health check configuration
    #[serde(default)]
    pub health_check: HealthCheckConfig,

    /// Introspection configuration
    pub introspection: Introspection,

    /// Logging configuration
    pub logging: Logging,

    /// Telemetry configuration
    pub telemetry: Telemetry,

    /// Overrides for server behaviour
    pub overrides: Overrides,

    /// The type of server transport to use
    pub transport: Transport,
}
```

Remove the now-unused imports of `OperationSource`, `SchemaSource`, `Endpoint`. Then add a `Default` impl for `GraphsSource` so `Config::default()` still works:

```rust
impl Default for GraphsSource {
    fn default() -> Self {
        GraphsSource::Local {
            manifest: PathBuf::from("./graphs.yaml"),
        }
    }
}
```

Place this at the bottom of `graphs_source.rs`.

Update `crates/apollo-mcp-server/src/runtime/config.rs` tests to remove references to the removed fields. Specifically delete `it_parses_instructions`'s `endpoint:` line:

```rust
#[test]
fn it_parses_instructions() {
    let yaml = r#"
        instructions: "Use semantic search before listing."
        graphs:
          type: local
          manifest: ./graphs.yaml
    "#;
    let config: Config = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(
        config.instructions.as_deref(),
        Some("Use semantic search before listing."),
    );
}
```

- [ ] **Step 2: Build and run tests**

```bash
cargo build -p apollo-mcp-server
```

Expected: many compile errors in `main.rs`, `runtime.rs`, and `server.rs` (they still reference the removed `OperationSource`/`SchemaSource`/`endpoint` paths). Leave them for Task 8 where we rewire `Server`. For now confirm only the new modules compile in isolation:

```bash
cargo test -p apollo-mcp-server --lib runtime::graphs_source
cargo test -p apollo-mcp-server --lib runtime::manifest
```

Expected: both pass. (Full crate build fails — expected.)

- [ ] **Step 3: Commit**

```bash
git add crates/apollo-mcp-server/src/runtime crates/apollo-mcp-server/src/runtime.rs
git commit -m "feat(multi-graph): add graphs: config section, remove single-graph keys"
```

Note: this commit leaves the crate failing to build. The build will be restored in Task 8.

---

## Phase 2: GraphContext and CredentialProvider

### Task 4: `CredentialProvider` trait + default impl

**Files:**
- Create: `crates/apollo-mcp-server/src/graphs/mod.rs`
- Create: `crates/apollo-mcp-server/src/graphs/credentials.rs`
- Modify: `crates/apollo-mcp-server/src/lib.rs` (add `pub mod graphs;`)

- [ ] **Step 1: Write the failing test**

Create `crates/apollo-mcp-server/src/graphs/credentials.rs`:

```rust
use std::sync::Arc;

use reqwest::header::HeaderMap;

/// A v2 seam: produces the upstream headers to use for a given (graph, user)
/// combination. v1 always returns `base` unchanged.
pub trait CredentialProvider: Send + Sync + std::fmt::Debug {
    fn headers_for(&self, base: &HeaderMap, user: Option<&str>) -> HeaderMap;
}

/// Default v1 implementation: returns the base headers untouched.
#[derive(Debug, Default)]
pub struct PassthroughCredentials;

impl CredentialProvider for PassthroughCredentials {
    fn headers_for(&self, base: &HeaderMap, _user: Option<&str>) -> HeaderMap {
        base.clone()
    }
}

pub fn default_provider() -> Arc<dyn CredentialProvider> {
    Arc::new(PassthroughCredentials)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderValue, AUTHORIZATION};

    #[test]
    fn passthrough_returns_base_unchanged() {
        let mut base = HeaderMap::new();
        base.insert(AUTHORIZATION, HeaderValue::from_static("Bearer x"));
        let p = PassthroughCredentials;
        let got = p.headers_for(&base, None);
        assert_eq!(got.get(AUTHORIZATION).unwrap(), "Bearer x");
    }
}
```

Create `crates/apollo-mcp-server/src/graphs/mod.rs`:

```rust
pub mod credentials;
```

Add to `crates/apollo-mcp-server/src/lib.rs` (after `pub mod operations;` or near it):

```rust
pub mod graphs;
```

- [ ] **Step 2: Run test**

```bash
cargo test -p apollo-mcp-server --lib graphs::credentials
```

Expected: 1 test passes.

- [ ] **Step 3: Commit**

```bash
git add crates/apollo-mcp-server/src/graphs crates/apollo-mcp-server/src/lib.rs
git commit -m "feat(multi-graph): add CredentialProvider trait and passthrough impl"
```

---

### Task 5: `GraphContext` struct

**Files:**
- Create: `crates/apollo-mcp-server/src/graphs/context.rs`
- Modify: `crates/apollo-mcp-server/src/graphs/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/apollo-mcp-server/src/graphs/context.rs`:

```rust
use std::sync::Arc;

use apollo_compiler::{Schema, validation::Valid};
use apollo_schema_index::SchemaIndex;
use reqwest::header::HeaderMap;
use tokio::sync::RwLock;
use url::Url;

use crate::custom_scalar_map::CustomScalarMap;
use crate::headers::ForwardHeaders;
use crate::operations::{MutationMode, Operation};

use super::credentials::CredentialProvider;

/// One graph's worth of state. Held by `Running` inside a `HashMap<String, GraphContext>`.
pub struct GraphContext {
    pub name: String,
    pub schema: Arc<RwLock<Valid<Schema>>>,
    pub endpoint: Url,
    pub headers: HeaderMap,
    pub forward_headers: ForwardHeaders,
    pub operations: Arc<RwLock<Vec<Operation>>>,
    pub search_index: SchemaIndex,
    pub mutation_mode: MutationMode,
    pub custom_scalar_map: Option<CustomScalarMap>,
    pub credentials: Arc<dyn CredentialProvider>,
}

impl std::fmt::Debug for GraphContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphContext")
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphs::credentials::default_provider;
    use apollo_schema_index::OperationType;

    fn parsed_schema() -> Valid<Schema> {
        Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap()
    }

    #[tokio::test]
    async fn it_constructs_a_context() {
        let schema = Arc::new(RwLock::new(parsed_schema()));
        let locked = schema.try_read().unwrap();
        let index =
            SchemaIndex::new(&locked, OperationType::Query.into(), 1_000_000).unwrap();
        drop(locked);

        let ctx = GraphContext {
            name: "g".into(),
            schema,
            endpoint: Url::parse("http://localhost:4000/").unwrap(),
            headers: HeaderMap::new(),
            forward_headers: vec![],
            operations: Arc::new(RwLock::new(vec![])),
            search_index: index,
            mutation_mode: MutationMode::None,
            custom_scalar_map: None,
            credentials: default_provider(),
        };

        assert_eq!(ctx.name, "g");
    }
}
```

Modify `crates/apollo-mcp-server/src/graphs/mod.rs`:

```rust
pub mod context;
pub mod credentials;

pub use context::GraphContext;
pub use credentials::{CredentialProvider, PassthroughCredentials, default_provider};
```

- [ ] **Step 2: Run test**

```bash
cargo test -p apollo-mcp-server --lib graphs::context
```

Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/apollo-mcp-server/src/graphs
git commit -m "feat(multi-graph): add GraphContext struct"
```

---

### Task 6: `GraphContext` factory from `GraphConfig`

**Files:**
- Create: `crates/apollo-mcp-server/src/graphs/factory.rs`
- Modify: `crates/apollo-mcp-server/src/graphs/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/apollo-mcp-server/src/graphs/factory.rs`:

```rust
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use apollo_compiler::{Schema, validation::Valid};
use apollo_schema_index::{OperationType, SchemaIndex};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::sync::RwLock;

use crate::custom_scalar_map::CustomScalarMap;
use crate::errors::OperationError;
use crate::operations::{
    AnnotationOverrides, MutationMode, Operation, RawOperation,
};
use crate::runtime::manifest::GraphConfig;

use super::context::GraphContext;
use super::credentials::default_provider;

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("failed to read schema file for graph '{graph}': {source}")]
    ReadSchema {
        graph: String,
        source: std::io::Error,
    },
    #[error("schema validation failed for graph '{graph}': {source}")]
    InvalidSchema {
        graph: String,
        source: apollo_compiler::validation::WithErrors<Schema>,
    },
    #[error("failed to glob operations for graph '{graph}': {source}")]
    GlobOps {
        graph: String,
        source: glob::PatternError,
    },
    #[error("failed to read operation file {path} for graph '{graph}': {source}")]
    ReadOp {
        graph: String,
        path: String,
        source: std::io::Error,
    },
    #[error("invalid operation in graph '{graph}': {source}")]
    InvalidOp {
        graph: String,
        source: OperationError,
    },
    #[error("invalid header in graph '{graph}': {message}")]
    BadHeader { graph: String, message: String },
    #[error("failed to build search index for graph '{graph}': {source}")]
    Index {
        graph: String,
        source: apollo_schema_index::error::IndexingError,
    },
}

#[expect(clippy::too_many_arguments)]
pub async fn build_graph_context(
    config: GraphConfig,
    index_memory_bytes: usize,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
    enable_output_schema: bool,
    annotation_overrides: &HashMap<String, AnnotationOverrides>,
    description_overrides: &HashMap<String, String>,
    custom_scalar_map: Option<CustomScalarMap>,
    base_headers: &HeaderMap,
    base_forward_headers: &crate::headers::ForwardHeaders,
) -> Result<GraphContext, BuildError> {
    let schema_text =
        std::fs::read_to_string(&config.schema).map_err(|source| BuildError::ReadSchema {
            graph: config.name.clone(),
            source,
        })?;

    let parsed = Schema::parse(schema_text, "schema.graphql")
        .and_then(|s| s.validate())
        .map_err(|source| BuildError::InvalidSchema {
            graph: config.name.clone(),
            source,
        })?;

    let root_types = match mutation_mode {
        MutationMode::None => OperationType::Query.into(),
        _ => OperationType::Query | OperationType::Mutation,
    };
    let index = SchemaIndex::new(&parsed, root_types, index_memory_bytes).map_err(|source| {
        BuildError::Index {
            graph: config.name.clone(),
            source,
        }
    })?;

    let mut raw_ops: Vec<RawOperation> = Vec::new();
    for pattern in &config.operations {
        let entries = glob::glob(pattern).map_err(|source| BuildError::GlobOps {
            graph: config.name.clone(),
            source,
        })?;
        for entry in entries.flatten() {
            let text = std::fs::read_to_string(&entry).map_err(|source| BuildError::ReadOp {
                graph: config.name.clone(),
                path: entry.display().to_string(),
                source,
            })?;
            raw_ops.push(RawOperation::from((
                text,
                Some(entry.display().to_string()),
            )));
        }
    }

    let mut operations: Vec<Operation> = Vec::new();
    for raw in raw_ops {
        let op = raw
            .into_operation_with_prefix(
                &parsed,
                custom_scalar_map.as_ref(),
                mutation_mode,
                disable_type_description,
                disable_schema_description,
                enable_output_schema,
                annotation_overrides,
                description_overrides,
                Some(&config.name),
            )
            .map_err(|source| BuildError::InvalidOp {
                graph: config.name.clone(),
                source,
            })?;
        if let Some(op) = op {
            operations.push(op);
        }
    }

    let mut headers = base_headers.clone();
    for (k, v) in &config.headers {
        let name = HeaderName::from_str(k).map_err(|e| BuildError::BadHeader {
            graph: config.name.clone(),
            message: e.to_string(),
        })?;
        let value = HeaderValue::from_str(v).map_err(|e| BuildError::BadHeader {
            graph: config.name.clone(),
            message: e.to_string(),
        })?;
        headers.insert(name, value);
    }

    Ok(GraphContext {
        name: config.name,
        schema: Arc::new(RwLock::new(parsed)),
        endpoint: config.endpoint,
        headers,
        forward_headers: base_forward_headers.clone(),
        operations: Arc::new(RwLock::new(operations)),
        search_index: index,
        mutation_mode,
        custom_scalar_map,
        credentials: default_provider(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    #[tokio::test]
    async fn it_builds_context_with_one_operation() {
        let dir = tempfile::tempdir().unwrap();
        let schema = write_file(dir.path(), "schema.graphql", "type Query { id: String }");
        let op = write_file(dir.path(), "op.graphql", "query GetId { id }");

        let config = GraphConfig {
            name: "g".into(),
            endpoint: url::Url::parse("http://localhost:4000/").unwrap(),
            schema,
            operations: vec![op.display().to_string()],
            headers: HashMap::new(),
        };

        let ctx = build_graph_context(
            config,
            1_000_000,
            MutationMode::None,
            false,
            false,
            false,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &HeaderMap::new(),
            &vec![],
        )
        .await
        .unwrap();

        let ops = ctx.operations.read().await;
        assert_eq!(ops.len(), 1);
        let tool: &rmcp::model::Tool = ops[0].as_ref();
        assert_eq!(tool.name.as_ref(), "g__GetId");
    }
}
```

Add `glob` to `crates/apollo-mcp-server/Cargo.toml` if missing:

```bash
cargo add glob --package apollo-mcp-server
```

Update `crates/apollo-mcp-server/src/graphs/mod.rs`:

```rust
pub mod context;
pub mod credentials;
pub mod factory;

pub use context::GraphContext;
pub use credentials::{CredentialProvider, PassthroughCredentials, default_provider};
pub use factory::{build_graph_context, BuildError};
```

(The test calls `into_operation_with_prefix`, which Task 7 implements. Skip the test run until Task 7 is done.)

- [ ] **Step 2: Commit**

```bash
git add crates/apollo-mcp-server/src/graphs crates/apollo-mcp-server/Cargo.toml
git commit -m "feat(multi-graph): add GraphContext factory from manifest entry"
```

---

## Phase 3: Prefixed Operation Names

### Task 7: `RawOperation::into_operation_with_prefix`

**Files:**
- Modify: `crates/apollo-mcp-server/src/operations/raw_operation.rs`
- Modify: `crates/apollo-mcp-server/src/operations/operation.rs`

- [ ] **Step 1: Read the existing `Operation::from_raw`**

```bash
sed -n '1,80p' crates/apollo-mcp-server/src/operations/operation.rs
```

The current `Operation::from_raw` builds a `Tool` whose `name` matches the GraphQL operation name. We need a sibling that applies a prefix.

- [ ] **Step 2: Add `name_prefix` plumbing to `Operation::from_raw`**

First read the existing function and locate every site where the operation name is used:

```bash
sed -n '1,250p' crates/apollo-mcp-server/src/operations/operation.rs
grep -n "Tool::new\|tool.name\|\.name =" crates/apollo-mcp-server/src/operations/operation.rs
```

Identify the local variable that holds the GraphQL operation's name (commonly `name`, `operation_name`, or similar). The transformation is:

1. Add `name_prefix: Option<&str>` as a new last parameter to `Operation::from_raw`.
2. Immediately after the existing name binding, compute the tool name:
   ```rust
   let tool_name = match name_prefix {
       Some(prefix) => format!("{prefix}__{name}"),
       None => name.clone(),
   };
   ```
3. Use `tool_name` (instead of `name`) **only** as the first argument to `Tool::new` and anywhere the value is stored as the MCP tool's identifier (e.g. a struct field like `Operation { name: tool_name, ... }`).
4. Keep using the original `name` everywhere the GraphQL operation name itself is needed (e.g. when calling `operation_defs`, when including the name in the outgoing GraphQL request body as `operationName`).

The split is: GraphQL operation name stays as-is; MCP tool name is prefixed.

- [ ] **Step 3: Add the prefixed entry point to `RawOperation`**

Modify `crates/apollo-mcp-server/src/operations/raw_operation.rs`, add an `into_operation_with_prefix` method below the existing `into_operation`:

```rust
impl RawOperation {
    #[expect(clippy::too_many_arguments)]
    pub(crate) fn into_operation_with_prefix(
        self,
        schema: &Valid<apollo_compiler::Schema>,
        custom_scalars: Option<&CustomScalarMap>,
        mutation_mode: MutationMode,
        disable_type_description: bool,
        disable_schema_description: bool,
        enable_output_schema: bool,
        annotation_overrides: &HashMap<String, AnnotationOverrides>,
        description_overrides: &HashMap<String, String>,
        name_prefix: Option<&str>,
    ) -> Result<Option<Operation>, OperationError> {
        Operation::from_raw(
            self,
            schema,
            custom_scalars,
            mutation_mode,
            disable_type_description,
            disable_schema_description,
            enable_output_schema,
            annotation_overrides,
            description_overrides,
            name_prefix,
        )
    }
}
```

Update the existing `into_operation` to call `from_raw(... , None)` (passing no prefix). Also update every other in-crate caller of `Operation::from_raw` (grep first):

```bash
grep -rn "Operation::from_raw\|into_operation(" crates/apollo-mcp-server/src
```

For every caller that ignored the prefix concept, pass `None`.

- [ ] **Step 4: Run prefix unit test**

Add to `operations/operation.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn prefixed_tool_name() {
    let schema = apollo_compiler::Schema::parse("type Query { id: String }", "s.graphql")
        .unwrap()
        .validate()
        .unwrap();
    let raw = crate::operations::RawOperation::from((
        "query GetId { id }".to_string(),
        Some("op.graphql".to_string()),
    ));
    let op = raw
        .into_operation_with_prefix(
            &schema,
            None,
            crate::operations::MutationMode::None,
            false,
            false,
            false,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            Some("g"),
        )
        .unwrap()
        .unwrap();
    let tool: &rmcp::model::Tool = op.as_ref();
    assert_eq!(tool.name.as_ref(), "g__GetId");
}

#[test]
fn unprefixed_tool_name() {
    let schema = apollo_compiler::Schema::parse("type Query { id: String }", "s.graphql")
        .unwrap()
        .validate()
        .unwrap();
    let raw = crate::operations::RawOperation::from((
        "query GetId { id }".to_string(),
        Some("op.graphql".to_string()),
    ));
    let op = raw
        .into_operation_with_prefix(
            &schema,
            None,
            crate::operations::MutationMode::None,
            false,
            false,
            false,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            None,
        )
        .unwrap()
        .unwrap();
    let tool: &rmcp::model::Tool = op.as_ref();
    assert_eq!(tool.name.as_ref(), "GetId");
}
```

Run:

```bash
cargo test -p apollo-mcp-server --lib operations::operation
cargo test -p apollo-mcp-server --lib graphs::factory
```

Expected: both pass.

- [ ] **Step 5: Commit**

```bash
git add crates/apollo-mcp-server/src/operations crates/apollo-mcp-server/src/graphs
git commit -m "feat(multi-graph): allow prefixing operation tool names"
```

---

## Phase 4: Refactor `Server` and State Machine

### Task 8: `Server` builder accepts manifest source

**Files:**
- Modify: `crates/apollo-mcp-server/src/server.rs`

- [ ] **Step 1: Replace single-graph fields with multi-graph fields**

In `Server`, remove `schema_source`, `operation_source`, `endpoint`, and the now-graph-specific `headers`/`forward_headers` fields *as defaults for one graph*. Replace with a single `graphs_source: GraphsSource` plus retain `headers`/`forward_headers` as *server-wide baseline* (applied to every graph by the factory).

Updated struct (replace only the fields shown — preserve everything else):

```rust
use crate::runtime::graphs_source::GraphsSource;

pub struct Server {
    config_path: Option<PathBuf>,
    transport: Transport,
    graphs_source: GraphsSource,
    headers: HeaderMap,
    forward_headers: ForwardHeaders,
    // ... everything else unchanged ...
}
```

Update the `#[builder]` signature and the `new(...)` constructor body identically — replace `schema_source: SchemaSource`, `operation_source: OperationSource`, `endpoint: Url` with `graphs_source: GraphsSource`, and drop those three from the `Self { ... }` block in favor of `graphs_source`.

Then update `Server::start` — it currently does:

```rust
let schema_stream = server.schema_source.into_stream()...
let operation_stream = server.operation_source.into_stream().await.boxed();
```

Replace with a single fan-in stream that emits a "graphs loaded" event once after building all contexts. New `start`:

```rust
pub async fn start(self) -> Result<ShutdownReason, ServerError> {
    StateMachine {}.start(self).await
}
```

(Body unchanged; the state machine will own the new loading flow.)

- [ ] **Step 2: Update `main.rs` and `runtime.rs` to feed `graphs_source` into the builder**

```bash
grep -rn "schema_source\|operation_source\|\.endpoint(" crates/apollo-mcp-server/src/main.rs crates/apollo-mcp-server/src/runtime.rs
```

For each call site, replace the per-source wiring with:

```rust
.graphs_source(config.graphs.clone())
```

Remove the now-dead `SchemaSource`/`OperationSource`/`Endpoint` imports.

- [ ] **Step 3: Build**

```bash
cargo build -p apollo-mcp-server
```

Expected: state-machine files still fail to compile (Task 9–10 fixes them). Confirm only `server.rs`, `main.rs`, `runtime.rs` are clean.

- [ ] **Step 4: Commit**

```bash
git add crates/apollo-mcp-server/src/server.rs crates/apollo-mcp-server/src/main.rs crates/apollo-mcp-server/src/runtime.rs
git commit -m "refactor(multi-graph): Server takes GraphsSource"
```

---

### Task 9: `Running` holds `HashMap<String, GraphContext>`

**Files:**
- Modify: `crates/apollo-mcp-server/src/server/states/running.rs`

- [ ] **Step 1: Rewrite the `Running` struct**

Replace the fields listed below; leave the others (peers, cancellation_token, server_info, instructions, rhai_engine, apps, prompts, health_check) unchanged:

```rust
pub(super) struct Running {
    pub(super) graphs: std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, crate::graphs::GraphContext>>>,
    pub(super) apps: Vec<crate::apps::App>,
    pub(super) prompts: Vec<crate::prompts::PromptFile>,
    pub(super) execute_tool: Option<Execute>,
    pub(super) introspect_tool: Option<Introspect>,
    pub(super) search_tool: Option<Search>,
    pub(super) explorer_tool: Option<Explorer>,
    pub(super) validate_tool: Option<Validate>,
    pub(super) peers: std::sync::Arc<tokio::sync::RwLock<Vec<rmcp::Peer<rmcp::RoleServer>>>>,
    pub(super) cancellation_token: tokio_util::sync::CancellationToken,
    pub(super) disable_auth_token_passthrough: bool,
    pub(super) health_check: Option<HealthCheck>,
    pub(super) server_info: ServerInfoConfig,
    pub(super) instructions: Option<String>,
    pub(super) rhai_engine: std::sync::Arc<parking_lot::Mutex<apollo_mcp_rhai::RhaiEngine>>,
}
```

Remove these now-graph-scoped fields: `schema`, `operations`, `headers`, `forward_headers`, `endpoint`, `mutation_mode`, `custom_scalar_map`, `disable_type_description`, `disable_schema_description`, `enable_output_schema`, `descriptions`, `annotations`.

- [ ] **Step 2: Remove `update_schema` and `update_operations`**

These were the hot-reload paths. v1 has no per-graph hot reload, so delete both methods. Also delete the related tests in this file (the `mod update_operations` block).

- [ ] **Step 3: Rewrite `list_tools_impl`**

The tool list now aggregates operations across every graph and adds the four built-in tools once:

```rust
async fn list_tools_impl(
    &self,
    extensions: Extensions,
    client_capabilities: Option<&ClientCapabilities>,
    protocol_version: Option<&ProtocolVersion>,
) -> Result<ListToolsResult, McpError> {
    let meter = &meter::METER;
    meter
        .u64_counter(TelemetryMetric::ListToolsCount.as_str())
        .build()
        .add(1, &[]);

    let app_param = extract_app_param(&extensions);
    let app_target = AppTarget::try_from((extensions, client_capabilities))?;

    let mut tools: Vec<rmcp::model::Tool> = Vec::new();

    if let Some(app_name) = app_param {
        let app = self
            .apps
            .iter()
            .find(|app| app.name == app_name)
            .ok_or_else(|| {
                McpError::new(
                    ErrorCode::INVALID_REQUEST,
                    format!("App {app_name} not found"),
                    None,
                )
            })?;
        let graphs = self.graphs.read().await;
        for ctx in graphs.values() {
            let ops = ctx.operations.read().await;
            tools.extend(ops.iter().map(|op| op.as_ref().clone()));
        }
        if let Some(e) = &self.execute_tool {
            tools.push(make_tool_private(e.tool.clone()));
        }
        for tool in &app.tools {
            tools.push(attach_tool_metadata(app, tool, &app_target));
        }
    } else {
        let graphs = self.graphs.read().await;
        for ctx in graphs.values() {
            let ops = ctx.operations.read().await;
            tools.extend(ops.iter().map(|op| op.as_ref().clone()));
        }
        if let Some(e) = &self.execute_tool { tools.push(e.tool.clone()); }
        if let Some(e) = &self.introspect_tool { tools.push(e.tool.clone()); }
        if let Some(e) = &self.search_tool { tools.push(e.tool.clone()); }
        if let Some(e) = &self.explorer_tool { tools.push(e.tool.clone()); }
        if let Some(e) = &self.validate_tool { tools.push(e.tool.clone()); }
    }

    let mut result = ListToolsResult { next_cursor: None, tools, meta: None };
    if !self.client_supports_output_schema(protocol_version) {
        for tool in &mut result.tools {
            tool.output_schema = None;
        }
    }
    Ok(result)
}
```

- [ ] **Step 4: Stub out `call_tool_impl` for now**

The existing call-tool body references the deleted fields. Replace its body with an early-return placeholder that calls the tool dispatchers (which Tasks 11–14 implement). Concretely, for each tool branch where it does `execute_tool.execute(...)` or similar, replace the in-place arg construction with a single call:

```rust
} else if tool_name == EXECUTE_TOOL_NAME
    && let Some(execute_tool) = &self.execute_tool
{
    execute_tool.dispatch(&self.graphs, request.arguments.as_ref(), axum_parts, &self.rhai_engine).await
}
```

And similarly for search / introspect / validate. For now define a temporary stub on each tool (Task 11–14 will replace these):

```rust
impl Execute {
    pub async fn dispatch(
        &self,
        _graphs: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, crate::graphs::GraphContext>>>,
        _args: Option<&rmcp::model::JsonObject>,
        _axum_parts: Option<&axum::http::request::Parts>,
        _rhai_engine: &std::sync::Arc<parking_lot::Mutex<apollo_mcp_rhai::RhaiEngine>>,
    ) -> Result<CallToolResult, McpError> {
        unimplemented!("Task 11")
    }
}
```

Put these stubs at the bottom of each tool's existing file (`introspection/tools/{execute,search,introspect,validate}.rs`). The point of this step is just to make the file compile so we can land the Running refactor in isolation.

- [ ] **Step 5: Build**

```bash
cargo build -p apollo-mcp-server
```

Expected: the crate compiles. Tests will fail (Task 10 fixes states; Tasks 11–14 fill the stubs).

- [ ] **Step 6: Commit**

```bash
git add crates/apollo-mcp-server/src/server/states/running.rs crates/apollo-mcp-server/src/introspection/tools
git commit -m "refactor(multi-graph): Running holds HashMap<String, GraphContext>"
```

---

### Task 10: Collapse state machine to `Configuring → Loading → Running`

**Files:**
- Modify: `crates/apollo-mcp-server/src/server/states.rs`
- Modify: `crates/apollo-mcp-server/src/server/states/configuring.rs`
- Create: `crates/apollo-mcp-server/src/server/states/loading.rs`
- Delete: `crates/apollo-mcp-server/src/server/states/schema_configured.rs`
- Delete: `crates/apollo-mcp-server/src/server/states/operations_configured.rs`

- [ ] **Step 1: Delete the unused states**

```bash
git rm crates/apollo-mcp-server/src/server/states/schema_configured.rs crates/apollo-mcp-server/src/server/states/operations_configured.rs
```

Update `states.rs` to remove the two `mod` lines and the two `use` lines, plus the two `State` enum variants and their `From` impls.

- [ ] **Step 2: Add `Loading` state**

Create `crates/apollo-mcp-server/src/server/states/loading.rs`:

```rust
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::errors::ServerError;
use crate::graphs::{GraphContext, build_graph_context};
use crate::runtime::graphs_source::GraphsSource;
use crate::runtime::manifest::{Manifest, load_local};

use super::{Config, running::Running};

pub(crate) struct Loading {
    pub(super) config: Config,
    pub(super) graphs_source: GraphsSource,
}

impl Loading {
    pub(super) async fn load(self) -> Result<Running, ServerError> {
        let manifest = match self.graphs_source {
            GraphsSource::Local { manifest } => load_local(&manifest)
                .map_err(|e| ServerError::ManifestLoad(e.to_string()))?,
            GraphsSource::Oci { .. } => {
                return Err(ServerError::ManifestLoad(
                    "OCI loader not yet implemented".into(),
                ));
            }
        };

        let mut graphs: std::collections::HashMap<String, GraphContext> =
            std::collections::HashMap::new();
        for g in manifest.graphs {
            let name = g.name.clone();
            let ctx = build_graph_context(
                g,
                self.config.index_memory_bytes,
                self.config.mutation_mode,
                self.config.disable_type_description,
                self.config.disable_schema_description,
                self.config.enable_output_schema,
                &self.config.annotations,
                &self.config.descriptions,
                self.config.custom_scalar_map.clone(),
                &self.config.headers,
                &self.config.forward_headers,
            )
            .await
            .map_err(|e| ServerError::ManifestLoad(e.to_string()))?;
            graphs.insert(name, ctx);
        }

        Ok(Running::from_config(self.config, Arc::new(RwLock::new(graphs))))
    }
}
```

Add new `ServerError::ManifestLoad(String)` variant in `crates/apollo-mcp-server/src/errors.rs`. Search for `pub enum ServerError` and add one line:

```rust
#[error("failed to load graphs manifest: {0}")]
ManifestLoad(String),
```

- [ ] **Step 3: Implement `Running::from_config`**

In `running.rs`, add an associated constructor:

```rust
impl Running {
    pub(super) fn from_config(
        config: super::Config,
        graphs: Arc<RwLock<HashMap<String, GraphContext>>>,
    ) -> Self {
        let search_allow_mutations = matches!(config.mutation_mode, crate::operations::MutationMode::All);
        let execute_tool = config.execute_introspection.then(|| {
            Execute::new(config.mutation_mode, config.execute_tool_hint.as_deref())
        });
        let introspect_tool = config.introspect_introspection.then(|| {
            Introspect::new_dispatcher(config.introspect_minify, config.introspect_tool_hint.as_deref())
        });
        let search_tool = config.search_introspection.then(|| {
            Search::new_dispatcher(
                search_allow_mutations,
                config.search_leaf_depth,
                config.search_minify,
                config.search_tool_hint.as_deref(),
            )
        });
        let validate_tool = config.validate_introspection.then(|| {
            Validate::new_dispatcher(config.validate_tool_hint.as_deref())
        });
        let explorer_tool = config.explorer_graph_ref.as_deref().map(Explorer::new);

        Running {
            graphs,
            apps: vec![], // TODO: load apps later if applicable
            prompts: vec![],
            execute_tool,
            introspect_tool,
            search_tool,
            explorer_tool,
            validate_tool,
            peers: Arc::new(RwLock::new(vec![])),
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            disable_auth_token_passthrough: config.disable_auth_token_passthrough,
            health_check: None,
            server_info: config.server_info,
            instructions: config.instructions,
            rhai_engine: Arc::new(parking_lot::Mutex::new(apollo_mcp_rhai::RhaiEngine::new("rhai"))),
        }
    }
}
```

The `_dispatcher` constructors are introduced in Tasks 11–14. For now add empty stubs in each tool file that just return a tool with the existing name + description but the existing-style `new(...)` body:

```rust
impl Search {
    pub fn new_dispatcher(
        allow_mutations: bool,
        leaf_depth: usize,
        minify: bool,
        description_hint: Option<&str>,
    ) -> Self {
        unimplemented!("Task 12")
    }
}
```

Add identical stubs to `Execute::new` (it already exists — augment), `Introspect`, `Validate`.

- [ ] **Step 4: Rewrite `Configuring` to skip directly to `Loading`**

In `states/configuring.rs`, replace the `set_schema` / `set_operations` methods with a single transition fired by the new "manifest source given" event:

```rust
use super::loading::Loading;
use crate::runtime::graphs_source::GraphsSource;

impl Configuring {
    pub(super) fn start_loading(self, graphs_source: GraphsSource) -> Loading {
        Loading { config: self.config, graphs_source }
    }
}
```

- [ ] **Step 5: Update `StateMachine::start` to drive the new flow**

In `states.rs`, replace the schema/operation stream wiring with: the moment we enter the loop, immediately transition `Configuring → Loading → Running`. There is no streaming for multi-graph load in v1:

```rust
impl StateMachine {
    pub(crate) async fn start(self, server: Server) -> Result<ShutdownReason, ServerError> {
        let config_validator = server.config_validator;
        let graphs_source = server.graphs_source.clone();
        let config = Config { /* same field-by-field as before, minus removed fields */ };

        let loading = Configuring { config }.start_loading(graphs_source);
        let mut state: State = match loading.load().await {
            Ok(running) => State::Running(running),
            Err(e) => return Err(e),
        };

        let ctrl_c_stream = Self::ctrl_c_stream().boxed();
        let rhai_stream = Self::rhai_watch_stream().boxed();
        let config_stream = Self::config_watch_stream(server.config_path.as_deref()).boxed();
        let sighup_stream = Self::sighup_stream().boxed();
        let mut stream = stream::select_all(vec![
            ctrl_c_stream, rhai_stream, config_stream, sighup_stream,
        ]);

        // The Starting state still owns binding the HTTP transport, etc.
        if let State::Running(running) = &state {
            // (Keep existing logic that hands off Running to the transport. The
            // existing Starting state's `start()` returned a Running; here Running
            // is already ready, so call its serve loop directly.)
        }

        while let Some(event) = stream.next().await {
            state = Self::process_event(state, event, &config_validator).await?;
            if matches!(&state, State::Error(_) | State::Stopping | State::Restarting) {
                break;
            }
        }

        match state {
            State::Error(e) => Err(e),
            State::Restarting => Ok(ShutdownReason::Restart),
            _ => Ok(ShutdownReason::Shutdown),
        }
    }
}
```

Note: this collapses the schema/operations stream handling. The `process_event` function should be reduced to just `ConfigChanged`, `RhaiScriptsChanged`, `Shutdown` arms. Remove the `SchemaUpdated`, `OperationsUpdated`, `OperationError`, `CollectionError` arms. Delete `ServerEvent::SchemaUpdated`, `ServerEvent::OperationsUpdated`, `ServerEvent::OperationError`, `ServerEvent::CollectionError` from `crates/apollo-mcp-server/src/event.rs`.

- [ ] **Step 6: Build and test**

```bash
cargo build -p apollo-mcp-server
```

Expected: still failing because the tool stubs all `unimplemented!()`. That's fine.

```bash
cargo test -p apollo-mcp-server --lib runtime
cargo test -p apollo-mcp-server --lib graphs
```

Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add -A crates/apollo-mcp-server/src
git commit -m "refactor(multi-graph): collapse state machine to Configuring -> Loading -> Running"
```

---

## Phase 5: Tool Dispatchers

### Task 11: Rewrite `Execute` as a dispatcher with `graph` arg

**Files:**
- Modify: `crates/apollo-mcp-server/src/introspection/tools/execute.rs`

- [ ] **Step 1: Add the new input shape and dispatch method**

Replace the `Input` struct and `Execute` impl in `execute.rs` with:

```rust
#[derive(JsonSchema, Deserialize)]
pub struct Input {
    /// The namespace of the graph to execute against. Required.
    pub graph: String,

    /// The GraphQL operation
    pub query: String,

    /// The variable values represented as JSON
    #[schemars(schema_with = "String::json_schema", default)]
    pub variables: Option<Value>,
}
```

Update `Execute::new` description to reflect the new shape:

```rust
impl Execute {
    pub fn new(mutation_mode: MutationMode, description_hint: Option<&str>) -> Self {
        let description = append_description_hint(
            "Execute a GraphQL operation against a specific graph. The `graph` argument names the target graph; required. Use the `search` tool (optionally scoped with the same `graph` argument) to discover types, and `introspect` to inspect a graph's root types.",
            description_hint,
        );
        Self {
            mutation_mode,
            tool: Tool::new(EXECUTE_TOOL_NAME, description, schema_from_type!(Input)),
        }
    }

    /// Look up `graph` in `graphs`, route the query, return the result.
    pub async fn dispatch(
        &self,
        graphs: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, crate::graphs::GraphContext>>>,
        args: Option<&rmcp::model::JsonObject>,
        axum_parts: Option<&axum::http::request::Parts>,
        rhai_engine: &std::sync::Arc<parking_lot::Mutex<apollo_mcp_rhai::RhaiEngine>>,
    ) -> Result<rmcp::model::CallToolResult, crate::errors::McpError> {
        let raw = match args {
            Some(v) => Value::Object(v.clone()),
            None => Value::Null,
        };
        let input: Input = match serde_json::from_value(raw) {
            Ok(i) => i,
            Err(e) => {
                return Ok(rmcp::model::CallToolResult::error(vec![
                    rmcp::model::Content::text(format!("Invalid input: {e}")),
                ]));
            }
        };

        let graphs_read = graphs.read().await;
        let ctx = match graphs_read.get(&input.graph) {
            Some(c) => c,
            None => {
                let names: Vec<String> = graphs_read.keys().cloned().collect();
                return Ok(rmcp::model::CallToolResult::error(vec![
                    rmcp::model::Content::text(format!(
                        "Unknown graph '{}'. Available graphs: {}",
                        input.graph,
                        names.join(", ")
                    )),
                ]));
            }
        };

        // Build per-graph effective headers via the credential seam.
        let base = ctx.credentials.headers_for(&ctx.headers, None);
        let effective_headers = if let Some(parts) = axum_parts {
            crate::headers::build_request_headers(
                &base,
                &ctx.forward_headers,
                &parts.headers,
                &parts.extensions,
                /* disable_auth_token_passthrough */ false,
            )
        } else {
            base
        };

        // Validate the query as an ad-hoc operation against this graph's schema
        // and execute. Reuse the existing `graphql::Executable` path.
        let exec_input = serde_json::json!({
            "query": input.query,
            "variables": input.variables,
        });

        crate::operations::execute_operation(
            self,
            &effective_headers,
            Some(&exec_input.as_object().cloned().unwrap_or_default()),
            &ctx.endpoint,
            rhai_engine,
            axum_parts,
            EXECUTE_TOOL_NAME,
        )
        .await
    }
}
```

- [ ] **Step 2: Update `graphql::Executable for Execute`**

The existing `Executable` impl's `operation` method takes the raw input. It deserializes the old `Input` shape. Update it to accept the new shape (with `graph` field present but unused at validation time):

```rust
impl graphql::Executable for Execute {
    fn operation(&self, input: Value) -> Result<OperationDetails, ValidationError> {
        let input = serde_json::from_value::<Input>(input)
            .map_err(|e| ValidationError(format!("Invalid input: {e}")))?;
        // existing parsing logic, using input.query
        let (_, operation_def, source_path) =
            operation_defs(&input.query, self.mutation_mode == MutationMode::All, None)
                .map_err(|e| ValidationError(e.to_string()))?
                .ok_or_else(|| ValidationError("Invalid operation type".into()))?;
        let op_name = operation_name(&operation_def, source_path).ok();
        let (query, private_fields) = match process_private_directives(&input.query) {
            Some((stripped, tree)) => (stripped, Some(tree)),
            None => (input.query, None),
        };
        Ok(OperationDetails { query, operation_name: op_name, private_fields })
    }

    fn variables(&self, input: Value) -> Result<Value, ValidationError> {
        let input = serde_json::from_value::<Input>(input)
            .map_err(|e| ValidationError(format!("Invalid input: {e}")))?;
        match input.variables {
            None => Ok(Value::Null),
            Some(Value::Null) => Ok(Value::Null),
            Some(Value::String(s)) => serde_json::from_str(&s)
                .map_err(|e| ValidationError(format!("Invalid variables: {e}"))),
            Some(obj) if obj.is_object() => Ok(obj),
            _ => Err(ValidationError("Variables must be a JSON object or string".into())),
        }
    }

    fn headers(&self, default_headers: &reqwest::header::HeaderMap) -> reqwest::header::HeaderMap {
        default_headers.clone()
    }
}
```

- [ ] **Step 3: Update existing tests for the new shape**

Every test in `execute.rs` that builds an `Input` JSON needs a `graph` field. Update each:

```rust
let input = json!({
    "graph": "g",
    "query": query,
    "variables": variables.to_string()
});
```

- [ ] **Step 4: Add a dispatch-routing test**

```rust
#[tokio::test]
async fn dispatch_routes_to_the_named_graph() {
    use crate::graphs::{credentials::default_provider, GraphContext};
    use apollo_compiler::Schema;
    use apollo_schema_index::{OperationType, SchemaIndex};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    let schema = Schema::parse("type Query { id: String }", "schema.graphql").unwrap().validate().unwrap();
    let locked = schema.clone();
    let index = SchemaIndex::new(&locked, OperationType::Query.into(), 1_000_000).unwrap();
    let ctx = GraphContext {
        name: "g".into(),
        schema: Arc::new(RwLock::new(schema)),
        endpoint: url::Url::parse("http://127.0.0.1:1/").unwrap(),
        headers: Default::default(),
        forward_headers: vec![],
        operations: Arc::new(RwLock::new(vec![])),
        search_index: index,
        mutation_mode: MutationMode::None,
        custom_scalar_map: None,
        credentials: default_provider(),
    };
    let mut map = std::collections::HashMap::new();
    map.insert("g".into(), ctx);
    let graphs = Arc::new(RwLock::new(map));

    let execute = Execute::new(MutationMode::None, None);
    let result = execute
        .dispatch(
            &graphs,
            Some(
                &serde_json::json!({"graph": "missing", "query": "{ id }"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
            None,
            &Arc::new(parking_lot::Mutex::new(apollo_mcp_rhai::RhaiEngine::new("rhai"))),
        )
        .await
        .unwrap();

    let text: String = result.content.iter().filter_map(|c| {
        if let rmcp::model::RawContent::Text(t) = c.deref() { Some(t.text.clone()) } else { None }
    }).collect();
    assert!(text.contains("Unknown graph 'missing'"));
}
```

- [ ] **Step 5: Build and test**

```bash
cargo test -p apollo-mcp-server --lib introspection::tools::execute
```

Expected: pass.

- [ ] **Step 6: Wire `dispatch` into `Running::call_tool_impl`**

Replace the `EXECUTE_TOOL_NAME` branch in `server/states/running.rs::call_tool_impl` with:

```rust
} else if tool_name == EXECUTE_TOOL_NAME
    && let Some(execute_tool) = &self.execute_tool
{
    execute_tool.dispatch(&self.graphs, request.arguments.as_ref(), axum_parts, &self.rhai_engine).await
}
```

- [ ] **Step 7: Commit**

```bash
git add crates/apollo-mcp-server/src/introspection/tools/execute.rs crates/apollo-mcp-server/src/server/states/running.rs
git commit -m "feat(multi-graph): Execute tool dispatches by required graph arg"
```

---

### Task 12: Rewrite `Search` for multi-graph with optional `graph` arg

**Files:**
- Modify: `crates/apollo-mcp-server/src/introspection/tools/search.rs`
- Modify: `crates/apollo-mcp-server/src/server/states/running.rs`

- [ ] **Step 1: Update `Input` and constructor**

```rust
#[derive(JsonSchema, Deserialize, Debug)]
pub struct Input {
    /// The search terms
    pub terms: Vec<String>,

    /// Optional graph namespace. When omitted, searches every configured graph.
    #[serde(default)]
    pub graph: Option<String>,
}

#[derive(Clone)]
pub struct Search {
    allow_mutations: bool,
    leaf_depth: usize,
    minify: bool,
    pub tool: Tool,
}

impl Search {
    pub fn new_dispatcher(
        allow_mutations: bool,
        leaf_depth: usize,
        minify: bool,
        description_hint: Option<&str>,
    ) -> Self {
        let default_description = format!(
            "Search GraphQL schemas for types matching the provided search terms. \
            Returns type definitions tagged with the graph they came from. \
            Provide `graph` to scope the search to a single graph; omit it to search all configured graphs.{}",
            if minify {
                " - T=type,I=input,E=enum,U=union,F=interface;s=String,i=Int,f=Float,b=Boolean,d=ID;@D=deprecated;!=required,[]=list,<>=implements"
            } else {
                ""
            }
        );
        let description =
            append_description_hint(&default_description, description_hint).into_owned();
        Self {
            allow_mutations,
            leaf_depth,
            minify,
            tool: Tool::new(SEARCH_TOOL_NAME, description, schema_from_type!(Input)),
        }
    }
}
```

Delete the old `Search::new` (which captured a single schema/index) and its associated tests. The `_dispatcher` form is the only one going forward.

- [ ] **Step 2: Add `dispatch` method that fans out across graphs**

```rust
impl Search {
    pub async fn dispatch(
        &self,
        graphs: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, crate::graphs::GraphContext>>>,
        args: Option<&rmcp::model::JsonObject>,
    ) -> Result<rmcp::model::CallToolResult, crate::errors::McpError> {
        let raw = match args {
            Some(v) => Value::Object(v.clone()),
            None => Value::Null,
        };
        let input: Input = match serde_json::from_value(raw) {
            Ok(i) => i,
            Err(e) => return Ok(rmcp::model::CallToolResult::error(vec![
                rmcp::model::Content::text(format!("Invalid input: {e}")),
            ])),
        };

        let graphs_read = graphs.read().await;

        let targets: Vec<&crate::graphs::GraphContext> = if let Some(name) = &input.graph {
            match graphs_read.get(name) {
                Some(ctx) => vec![ctx],
                None => {
                    let available: Vec<String> = graphs_read.keys().cloned().collect();
                    return Ok(rmcp::model::CallToolResult::error(vec![
                        rmcp::model::Content::text(format!(
                            "Unknown graph '{}'. Available graphs: {}",
                            name,
                            available.join(", ")
                        )),
                    ]));
                }
            }
        } else {
            graphs_read.values().collect()
        };

        let mut all_contents: Vec<rmcp::model::Content> = Vec::new();
        for ctx in targets {
            let per_graph = self.search_one(ctx, &input.terms).await?;
            all_contents.push(rmcp::model::Content::text(format!("# graph: {}", ctx.name)));
            all_contents.extend(per_graph);
        }
        Ok(rmcp::model::CallToolResult::success(all_contents))
    }

    async fn search_one(
        &self,
        ctx: &crate::graphs::GraphContext,
        terms: &[String],
    ) -> Result<Vec<rmcp::model::Content>, crate::errors::McpError> {
        // Body: copy the existing `Search::execute` logic but use ctx.search_index and
        // ctx.schema in place of self.index / self.schema; cap at MAX_SEARCH_RESULTS.
        let mut root_paths = ctx.search_index
            .search(terms.to_vec(), apollo_schema_index::Options::default())
            .map_err(|e| crate::errors::McpError::new(
                rmcp::model::ErrorCode::INTERNAL_ERROR,
                format!("Failed to search index for {}: {e}", ctx.name),
                None,
            ))?;
        root_paths.truncate(MAX_SEARCH_RESULTS);

        let schema = ctx.schema.read().await;
        let mut tree_shaker = crate::schema_tree_shake::SchemaTreeShaker::new(&schema);
        for root_path in root_paths {
            let path_len = root_path.inner.len();
            for (i, path_node) in root_path.inner.into_iter().enumerate() {
                if let Some(extended_type) = schema.types.get(path_node.node_type.as_str()) {
                    let (selection_set, depth) = if i == path_len - 1 {
                        (None, crate::schema_tree_shake::DepthLimit::Limited(self.leaf_depth))
                    } else {
                        (
                            path_node.field_name.as_ref().map(|fname| {
                                vec![apollo_compiler::ast::Selection::Field(apollo_compiler::Node::from(apollo_compiler::ast::Field {
                                    alias: Default::default(),
                                    name: apollo_compiler::Name::new_unchecked(fname),
                                    arguments: Default::default(),
                                    selection_set: Default::default(),
                                    directives: Default::default(),
                                }))]
                            }),
                            crate::schema_tree_shake::DepthLimit::Limited(1),
                        )
                    };
                    tree_shaker.retain_type(extended_type, selection_set.as_ref(), depth);
                }
                for field_arg in path_node.field_args {
                    if let Some(extended_type) = schema.types.get(field_arg.as_str()) {
                        tree_shaker.retain_type(extended_type, None, crate::schema_tree_shake::DepthLimit::Unlimited);
                    }
                }
            }
        }
        let shaken = tree_shaker.shaken().unwrap_or_else(|s| s.partial);
        Ok(shaken
            .types
            .iter()
            .filter(|(_, t)| {
                !t.is_built_in()
                    && schema
                        .root_operation(apollo_compiler::ast::OperationType::Mutation)
                        .is_none_or(|n| t.name() != n || self.allow_mutations)
            })
            .map(|(_, t)| if self.minify { use crate::introspection::minify::MinifyExt as _; t.minify() } else { t.serialize().to_string() })
            .map(rmcp::model::Content::text)
            .collect())
    }
}
```

- [ ] **Step 3: Update `Running::call_tool_impl` for search**

```rust
} else if tool_name == SEARCH_TOOL_NAME
    && let Some(search_tool) = &self.search_tool
{
    search_tool.dispatch(&self.graphs, request.arguments.as_ref()).await
}
```

- [ ] **Step 4: Rewrite existing search tests**

Delete the old tests at the bottom of `search.rs` (they relied on the deleted `Search::new` constructor). Add a single new test exercising the fan-out:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphs::{credentials::default_provider, GraphContext};
    use apollo_compiler::Schema;
    use apollo_schema_index::{OperationType, SchemaIndex};
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn ctx_for(name: &str, sdl: &str) -> GraphContext {
        let schema = Schema::parse(sdl, "s.graphql").unwrap().validate().unwrap();
        let idx = SchemaIndex::new(&schema, OperationType::Query.into(), 1_000_000).unwrap();
        GraphContext {
            name: name.into(),
            schema: Arc::new(RwLock::new(schema)),
            endpoint: url::Url::parse("http://localhost/").unwrap(),
            headers: Default::default(),
            forward_headers: vec![],
            operations: Arc::new(RwLock::new(vec![])),
            search_index: idx,
            mutation_mode: crate::operations::MutationMode::None,
            custom_scalar_map: None,
            credentials: default_provider(),
        }
    }

    #[tokio::test]
    async fn search_with_no_graph_arg_searches_every_graph() {
        let mut map = std::collections::HashMap::new();
        map.insert("a".to_string(), ctx_for("a", "type Query { alpha: String }"));
        map.insert("b".to_string(), ctx_for("b", "type Query { beta: String }"));
        let graphs = Arc::new(RwLock::new(map));

        let search = Search::new_dispatcher(false, 1, false, None);
        let result = search
            .dispatch(
                &graphs,
                Some(
                    &serde_json::json!({"terms": ["alpha", "beta"]})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();

        let combined: String = result.content.iter().filter_map(|c| {
            if let rmcp::model::RawContent::Text(t) = c.deref() { Some(t.text.clone()) } else { None }
        }).collect::<Vec<_>>().join("\n");
        assert!(combined.contains("# graph: a"));
        assert!(combined.contains("# graph: b"));
    }

    #[tokio::test]
    async fn search_with_unknown_graph_arg_errors() {
        let mut map = std::collections::HashMap::new();
        map.insert("a".to_string(), ctx_for("a", "type Query { alpha: String }"));
        let graphs = Arc::new(RwLock::new(map));

        let search = Search::new_dispatcher(false, 1, false, None);
        let result = search
            .dispatch(
                &graphs,
                Some(
                    &serde_json::json!({"terms": ["alpha"], "graph": "nope"})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();

        assert_eq!(result.is_error, Some(true));
    }
}
```

- [ ] **Step 5: Build and test**

```bash
cargo test -p apollo-mcp-server --lib introspection::tools::search
```

Expected: 2 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/apollo-mcp-server/src/introspection/tools/search.rs crates/apollo-mcp-server/src/server/states/running.rs
git commit -m "feat(multi-graph): Search tool fans out across graphs"
```

---

### Task 13: Rewrite `Introspect` with required `graph` arg

**Files:**
- Modify: `crates/apollo-mcp-server/src/introspection/tools/introspect.rs`
- Modify: `crates/apollo-mcp-server/src/server/states/running.rs`

- [ ] **Step 1: Add `graph` to `Input`**

In `introspect.rs`, locate the existing `Input` struct (read first):

```bash
sed -n '1,80p' crates/apollo-mcp-server/src/introspection/tools/introspect.rs
```

Add a required `graph` field and a `new_dispatcher` constructor that no longer captures a schema:

```rust
#[derive(JsonSchema, Deserialize, Debug)]
pub struct Input {
    /// The namespace of the graph to introspect. Required.
    pub graph: String,
    // ... preserve every existing field unchanged ...
}

impl Introspect {
    pub fn new_dispatcher(minify: bool, description_hint: Option<&str>) -> Self {
        // copy the description from the existing constructor; drop the
        // schema field; add the dispatcher tool name.
        let description = append_description_hint(
            "Introspect a specific graph's schema. The `graph` argument selects which configured graph to inspect. Provide a starting type name (often a root type from `search`).",
            description_hint,
        );
        Self {
            minify,
            tool: Tool::new(INTROSPECT_TOOL_NAME, description, schema_from_type!(Input)),
        }
    }

    pub async fn dispatch(
        &self,
        graphs: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, crate::graphs::GraphContext>>>,
        args: Option<&rmcp::model::JsonObject>,
    ) -> Result<rmcp::model::CallToolResult, crate::errors::McpError> {
        let raw = match args {
            Some(v) => Value::Object(v.clone()),
            None => Value::Null,
        };
        let input: Input = match serde_json::from_value(raw) {
            Ok(i) => i,
            Err(e) => return Ok(rmcp::model::CallToolResult::error(vec![
                rmcp::model::Content::text(format!("Invalid input: {e}")),
            ])),
        };

        let graphs_read = graphs.read().await;
        let ctx = match graphs_read.get(&input.graph) {
            Some(c) => c,
            None => {
                let names: Vec<String> = graphs_read.keys().cloned().collect();
                return Ok(rmcp::model::CallToolResult::error(vec![
                    rmcp::model::Content::text(format!(
                        "Unknown graph '{}'. Available graphs: {}",
                        input.graph,
                        names.join(", ")
                    )),
                ]));
            }
        };

        // Delegate to the original per-schema introspection logic, but read
        // the schema from `ctx.schema` instead of `self.schema`.
        let schema = ctx.schema.read().await;
        let body = self.introspect_against(&schema, /* preserved fields from Input */).await;
        Ok(rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(body)]))
    }
}
```

For the `introspect_against` body, extract the existing `Introspect::execute` body into a free function or method that takes a `&Valid<Schema>` and the input fields. The structure is identical — only the data source changes.

- [ ] **Step 2: Adjust tests**

Existing introspect tests probably build a `Schema` and call `Introspect::new(schema, ...)`. Update them to instead construct a `GraphContext` and call `dispatch` like the search tests.

- [ ] **Step 3: Wire into `Running::call_tool_impl`**

```rust
} else if tool_name == INTROSPECT_TOOL_NAME
    && let Some(introspect_tool) = &self.introspect_tool
{
    introspect_tool.dispatch(&self.graphs, request.arguments.as_ref()).await
}
```

- [ ] **Step 4: Build and test**

```bash
cargo test -p apollo-mcp-server --lib introspection::tools::introspect
```

Expected: tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/apollo-mcp-server/src/introspection/tools/introspect.rs crates/apollo-mcp-server/src/server/states/running.rs
git commit -m "feat(multi-graph): Introspect dispatches on required graph arg"
```

---

### Task 14: Rewrite `Validate` with required `graph` arg

**Files:**
- Modify: `crates/apollo-mcp-server/src/introspection/tools/validate.rs`
- Modify: `crates/apollo-mcp-server/src/server/states/running.rs`

- [ ] **Step 1: Read existing `Validate`**

```bash
sed -n '1,120p' crates/apollo-mcp-server/src/introspection/tools/validate.rs
```

Note the existing `Input` fields (typically a `query: String`) and how `Validate::execute` walks the schema.

- [ ] **Step 2: Add `graph` to `Input` and define `new_dispatcher`**

Replace the `Input` struct with:

```rust
#[derive(JsonSchema, Deserialize, Debug)]
pub struct Input {
    /// The namespace of the graph to validate against. Required.
    pub graph: String,
    /// The GraphQL document to validate
    pub query: String,
}
```

Replace `Validate::new` with:

```rust
impl Validate {
    pub fn new_dispatcher(description_hint: Option<&str>) -> Self {
        let description = append_description_hint(
            "Validate a GraphQL document against a specific graph's schema. The `graph` argument selects which configured graph to validate against. Returns validation errors or success.",
            description_hint,
        );
        Self {
            tool: Tool::new(VALIDATE_TOOL_NAME, description, schema_from_type!(Input)),
        }
    }
}
```

(If `Validate` currently has captured-schema fields, drop them — the dispatcher reads from `GraphContext`.)

- [ ] **Step 3: Add `dispatch` method**

```rust
impl Validate {
    pub async fn dispatch(
        &self,
        graphs: &std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<String, crate::graphs::GraphContext>>>,
        args: Option<&rmcp::model::JsonObject>,
    ) -> Result<rmcp::model::CallToolResult, crate::errors::McpError> {
        let raw = match args {
            Some(v) => Value::Object(v.clone()),
            None => Value::Null,
        };
        let input: Input = match serde_json::from_value(raw) {
            Ok(i) => i,
            Err(e) => return Ok(rmcp::model::CallToolResult::error(vec![
                rmcp::model::Content::text(format!("Invalid input: {e}")),
            ])),
        };

        let graphs_read = graphs.read().await;
        let ctx = match graphs_read.get(&input.graph) {
            Some(c) => c,
            None => {
                let names: Vec<String> = graphs_read.keys().cloned().collect();
                return Ok(rmcp::model::CallToolResult::error(vec![
                    rmcp::model::Content::text(format!(
                        "Unknown graph '{}'. Available graphs: {}",
                        input.graph,
                        names.join(", ")
                    )),
                ]));
            }
        };

        let schema = ctx.schema.read().await;
        // Reuse the existing validation logic. If the prior `execute` method had a
        // body like `apollo_compiler::ExecutableDocument::parse_and_validate(&schema, &input.query, "query.graphql")`,
        // call that directly here.
        match apollo_compiler::ExecutableDocument::parse_and_validate(&schema, input.query.as_str(), "query.graphql") {
            Ok(_) => Ok(rmcp::model::CallToolResult::success(vec![
                rmcp::model::Content::text("Document is valid."),
            ])),
            Err(e) => Ok(rmcp::model::CallToolResult::error(vec![
                rmcp::model::Content::text(format!("Validation errors:\n{e}")),
            ])),
        }
    }
}
```

If the existing `Validate::execute` does more than parse-and-validate (e.g. mutation-mode filtering), preserve that logic inside `dispatch` after the `match graphs_read.get(...)` block.

- [ ] **Step 4: Update existing tests**

Add a `graph: "g"` field to every JSON-shaped test input in `validate.rs`. Replace the schema-captured `Validate::new(schema, ...)` call sites with a `GraphContext`-driven dispatch test mirroring the search test in Task 12.

- [ ] **Step 5: Wire into `Running::call_tool_impl`**

```rust
} else if tool_name == VALIDATE_TOOL_NAME
    && let Some(validate_tool) = &self.validate_tool
{
    validate_tool.dispatch(&self.graphs, request.arguments.as_ref()).await
}
```

- [ ] **Step 6: Build, test, commit**

```bash
cargo test -p apollo-mcp-server --lib introspection::tools::validate
git add crates/apollo-mcp-server/src/introspection/tools/validate.rs crates/apollo-mcp-server/src/server/states/running.rs
git commit -m "feat(multi-graph): Validate dispatches on required graph arg"
```

---

## Phase 6: Auth Seam (Plan-For, Don't-Flow)

### Task 15: Add `UpstreamAuthRequired` error variant

**Files:**
- Modify: `crates/apollo-mcp-server/src/graphql.rs` (or wherever upstream errors live — grep first)
- Test: same file

- [ ] **Step 1: Locate the upstream-error enum**

```bash
grep -rn "pub enum.*Error" crates/apollo-mcp-server/src/graphql.rs | head -20
```

Identify the enum that wraps reqwest errors and GraphQL response errors (likely `graphql::Error` or a sibling).

- [ ] **Step 2: Add the new variant**

```rust
#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    // ... existing variants ...

    #[error("graph '{graph}' upstream requires authentication")]
    AuthRequired {
        graph: String,
        www_authenticate: Option<String>,
    },
}
```

If the existing enum is named differently, add the variant there. The constraint is just that `Execute::dispatch` can construct and surface it.

- [ ] **Step 3: Detect 401 on the upstream response**

Find the function inside `graphql.rs` (or `operations/execution.rs`) that posts to the upstream:

```bash
grep -rn "reqwest::Client\|\.post(\|\.send().await" crates/apollo-mcp-server/src/graphql.rs crates/apollo-mcp-server/src/operations/execution.rs
```

The transformation has three concrete edits:

**(a)** Add a `graph_name: &str` parameter to the executor function (e.g. `execute_operation` and any internal helper that calls `.send().await` on the upstream request). Update every call site to pass the value. In `Execute::dispatch` (Task 11), pass `&input.graph`. In `find_and_execute_operation` (used for per-operation tools), the graph name comes from the tool-name prefix — split the tool name on `__` and pass the prefix part.

**(b)** After the response is received and *before* parsing the body, insert:

```rust
if response.status() == reqwest::StatusCode::UNAUTHORIZED {
    let www = response.headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    return Err(UpstreamError::AuthRequired {
        graph: graph_name.to_string(),
        www_authenticate: www,
    });
}
```

**(c)** Wherever `UpstreamError` (or whichever enum holds the variant) is converted into a user-facing `CallToolResult`, add the new arm shown in Step 4.

- [ ] **Step 4: Surface the variant as a structured tool result**

In `execute_operation` (whichever site catches upstream errors and converts them to `CallToolResult`), add:

```rust
Err(UpstreamError::AuthRequired { graph, www_authenticate }) => {
    let mut structured = serde_json::Map::new();
    structured.insert("error".into(), serde_json::Value::String("upstream_auth_required".into()));
    structured.insert("graph".into(), serde_json::Value::String(graph.clone()));
    if let Some(w) = www_authenticate {
        structured.insert("www_authenticate".into(), serde_json::Value::String(w));
    }
    Ok(rmcp::model::CallToolResult::error(vec![
        rmcp::model::Content::text(format!("Graph '{graph}' requires authentication")),
    ]).with_structured_content(serde_json::Value::Object(structured)))
}
```

(If `CallToolResult` lacks a builder method, set `structured_content` directly.)

- [ ] **Step 5: Test**

```rust
#[tokio::test]
async fn upstream_401_surfaces_as_auth_required() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
        .mock("POST", "/")
        .with_status(401)
        .with_header("www-authenticate", "Bearer realm=\"demo\"")
        .create_async()
        .await;

    // Build a single-graph map, point its endpoint at server.url(), call Execute::dispatch
    // with a valid query, assert the resulting CallToolResult contains "upstream_auth_required"
    // and graph name.
}
```

(Fill in the GraphContext construction the same way as the Execute test in Task 11.)

- [ ] **Step 6: Commit**

```bash
git add crates/apollo-mcp-server/src/graphql.rs crates/apollo-mcp-server/src/operations crates/apollo-mcp-server/src/introspection/tools/execute.rs
git commit -m "feat(multi-graph): surface upstream 401 as UpstreamAuthRequired"
```

---

## Phase 7: Telemetry

### Task 16: Add `apollo.mcp.graph_name` attribute

**Files:**
- Modify: `crates/apollo-mcp-server/telemetry.toml`
- Modify: `crates/apollo-mcp-server/src/server/states/running.rs`

- [ ] **Step 1: Add the attribute to `telemetry.toml`**

Inspect the file:

```bash
cat crates/apollo-mcp-server/telemetry.toml | head -50
```

Add a new attribute next to `apollo.mcp.tool_name`. Use the same format the file already uses for attribute declarations.

- [ ] **Step 2: Set the attribute on `call_tool` spans**

In `running.rs::call_tool`, after the existing `apollo.mcp.tool_name` recording, extract the graph from the request arguments if present and record it:

```rust
if let Some(args) = &request.arguments {
    if let Some(graph) = args.get("graph").and_then(|v| v.as_str()) {
        span.record("apollo.mcp.graph_name", graph);
    }
}
```

Also update the `#[tracing::instrument(...)]` attribute on `call_tool` to declare `apollo.mcp.graph_name = tracing::field::Empty`.

- [ ] **Step 3: Add label to existing tool metrics**

Add to the attributes vec in `call_tool_impl`:

```rust
if let Some(args) = &request.arguments
    && let Some(graph) = args.get("graph").and_then(|v| v.as_str())
{
    attributes.push(KeyValue::new(TelemetryAttribute::GraphName.to_key(), graph.to_string()));
}
```

- [ ] **Step 4: Build**

```bash
cargo build -p apollo-mcp-server
```

Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/apollo-mcp-server/telemetry.toml crates/apollo-mcp-server/src/server/states/running.rs
git commit -m "feat(multi-graph): record apollo.mcp.graph_name on spans and metrics"
```

---

## Phase 8: OCI Loader

### Task 17: Add `oci-distribution` dependency

**Files:**
- Modify: `crates/apollo-mcp-server/Cargo.toml`

- [ ] **Step 1: Add the crate**

```bash
cargo add oci-distribution --no-default-features --features rustls-tls --package apollo-mcp-server
```

- [ ] **Step 2: Commit**

```bash
git add crates/apollo-mcp-server/Cargo.toml crates/apollo-mcp-server/Cargo.lock 2>/dev/null || git add crates/apollo-mcp-server/Cargo.toml
git commit -m "build: add oci-distribution dependency"
```

(If `Cargo.lock` lives at workspace root, `git add Cargo.lock` from the repo root.)

---

### Task 18: OCI manifest loader

**Files:**
- Create: `crates/apollo-mcp-server/src/runtime/manifest/oci.rs`
- Modify: `crates/apollo-mcp-server/src/runtime/manifest/mod.rs`

- [ ] **Step 1: Define the loader**

```rust
use std::path::PathBuf;

use oci_distribution::{Reference, client::ClientConfig, secrets::RegistryAuth};

use super::local::{LocalLoadError, load_local};
use super::types::Manifest;

/// Annotation key carrying the manifest filename inside the image.
pub const MANIFEST_ANNOTATION: &str = "org.apollographql.mcp.manifest";

/// The media type we expect for layers carrying the bundle contents.
pub const LAYER_MEDIA_TYPE: &str = "application/vnd.apollographql.mcp.bundle.v1+tar";

#[derive(Debug, thiserror::Error)]
pub enum OciLoadError {
    #[error("invalid OCI image reference '{image}': {source}")]
    BadReference {
        image: String,
        source: oci_distribution::ParseError,
    },
    #[error("failed to pull OCI image '{image}': {source}")]
    Pull {
        image: String,
        source: oci_distribution::errors::OciDistributionError,
    },
    #[error("image '{image}' is missing the {MANIFEST_ANNOTATION} annotation")]
    MissingAnnotation { image: String },
    #[error("failed to extract bundle for image '{image}': {source}")]
    Extract {
        image: String,
        source: std::io::Error,
    },
    #[error(transparent)]
    Local(#[from] LocalLoadError),
}

/// Pull `image`, extract its layers into a temp dir, read the manifest the
/// image's annotation points to, and parse it like a local manifest.
pub async fn load_oci(image: &str) -> Result<(Manifest, tempfile::TempDir), OciLoadError> {
    let reference: Reference = image
        .parse()
        .map_err(|source| OciLoadError::BadReference { image: image.into(), source })?;

    let client = oci_distribution::Client::new(ClientConfig::default());
    let (image_manifest, _digest, _config) = client
        .pull_image_manifest(&reference, &RegistryAuth::Anonymous)
        .await
        .map_err(|source| OciLoadError::Pull { image: image.into(), source })?;

    let manifest_filename = image_manifest
        .annotations
        .as_ref()
        .and_then(|a| a.get(MANIFEST_ANNOTATION))
        .cloned()
        .ok_or_else(|| OciLoadError::MissingAnnotation { image: image.into() })?;

    let pulled = client
        .pull(
            &reference,
            &RegistryAuth::Anonymous,
            vec![LAYER_MEDIA_TYPE, oci_distribution::manifest::IMAGE_LAYER_MEDIA_TYPE],
        )
        .await
        .map_err(|source| OciLoadError::Pull { image: image.into(), source })?;

    let tmp = tempfile::tempdir().map_err(|source| OciLoadError::Extract { image: image.into(), source })?;
    for layer in pulled.layers {
        let cursor = std::io::Cursor::new(layer.data);
        let mut archive = tar::Archive::new(cursor);
        archive.unpack(tmp.path()).map_err(|source| OciLoadError::Extract { image: image.into(), source })?;
    }

    let manifest_path: PathBuf = tmp.path().join(manifest_filename);
    let manifest = load_local(&manifest_path)?;
    Ok((manifest, tmp))
}
```

Add `tar` to `Cargo.toml`:

```bash
cargo add tar --package apollo-mcp-server
```

- [ ] **Step 2: Export from manifest module**

Update `runtime/manifest/mod.rs`:

```rust
pub mod local;
pub mod oci;
pub mod types;

pub use local::{LocalLoadError, load_local};
pub use oci::{OciLoadError, load_oci};
pub use types::{GraphConfig, Manifest};
```

- [ ] **Step 3: Wire OCI into `Loading::load`**

In `server/states/loading.rs`, replace the `GraphsSource::Oci` arm:

```rust
GraphsSource::Oci { image } => {
    let (manifest, _tempdir) = crate::runtime::manifest::load_oci(&image).await
        .map_err(|e| ServerError::ManifestLoad(e.to_string()))?;
    // Hold `_tempdir` for the lifetime of the running server so the extracted
    // files aren't deleted. Pass it into Running below.
    self.tempdir = Some(_tempdir);
    manifest
}
```

Add a `tempdir: Option<tempfile::TempDir>` field on `Loading` (default `None`) and on `Running` (so it isn't dropped while the server runs). For local-only loads it stays `None`.

- [ ] **Step 4: Tests (deferred)**

A full integration test against a real or mocked OCI registry adds significant test infrastructure (manifest JSON, layer tarball, registry HTTP mock) that's disproportionate to the PoC scope. Defer to a follow-up: file an issue titled "Add OCI loader integration test" listing the registry+tar fixture as the remaining work. The local loader is fully covered (Task 2); the OCI path's correctness is verified end-to-end by running the binary against a real test image.

- [ ] **Step 5: Commit**

```bash
git add crates/apollo-mcp-server/src/runtime/manifest crates/apollo-mcp-server/src/server/states/loading.rs crates/apollo-mcp-server/src/server/states/running.rs crates/apollo-mcp-server/Cargo.toml
git commit -m "feat(multi-graph): OCI image manifest loader"
```

---

## Phase 9: Integration

### Task 19: Two-graph end-to-end integration test

**Files:**
- Create: `crates/apollo-mcp-server/tests/multi_graph.rs`

- [ ] **Step 1: Spin up two mock upstreams + the MCP server**

```rust
use mockito::Server;
use std::path::PathBuf;

#[tokio::test]
async fn execute_routes_to_the_correct_graph() {
    let mut graph_a = Server::new_async().await;
    let mut graph_b = Server::new_async().await;

    let _mock_a = graph_a
        .mock("POST", "/")
        .with_status(200)
        .with_body(r#"{"data":{"id":"from-a"}}"#)
        .create_async()
        .await;

    let _mock_b = graph_b
        .mock("POST", "/")
        .with_status(200)
        .with_body(r#"{"data":{"id":"from-b"}}"#)
        .create_async()
        .await;

    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.graphql"), "type Query { id: String }").unwrap();
    std::fs::write(tmp.path().join("b.graphql"), "type Query { id: String }").unwrap();
    let manifest_path = tmp.path().join("graphs.yaml");
    std::fs::write(
        &manifest_path,
        format!(
            "version: 1\n\
             graphs:\n\
             - name: a\n\
             \x20 endpoint: {}/\n\
             \x20 schema: ./a.graphql\n\
             - name: b\n\
             \x20 endpoint: {}/\n\
             \x20 schema: ./b.graphql\n",
            graph_a.url(),
            graph_b.url(),
        ),
    )
    .unwrap();

    // Build a Running directly using `Loading::load` so we can call dispatch
    // without standing up a full transport.
    // (Construct Config with defaults; pass the manifest path through.)
    // ... assert that dispatching execute { graph: "a" } returns "from-a"
    // and { graph: "b" } returns "from-b".
}
```

Fill in the Running construction by mirroring `Loading::load` directly in the test (or expose a `pub(crate)` helper for tests).

- [ ] **Step 2: Run**

```bash
cargo test -p apollo-mcp-server --test multi_graph
```

- [ ] **Step 3: Commit**

```bash
git add crates/apollo-mcp-server/tests/multi_graph.rs
git commit -m "test(multi-graph): two-graph end-to-end execute routing"
```

---

### Task 20: Final lint, format, full test pass

- [ ] **Step 1: Format**

```bash
cargo fmt
```

- [ ] **Step 2: Lint (CI enforces `--deny warnings`)**

```bash
cargo clippy --all-targets -- --deny warnings
```

Fix all warnings.

- [ ] **Step 3: Full test sweep**

```bash
cargo test
```

Expected: every test passes.

- [ ] **Step 4: Commit any fix-ups**

```bash
git add -u
git commit -m "chore: fmt + clippy after multi-graph refactor" --allow-empty
```

- [ ] **Step 5: Push branch** (only after explicit user confirmation)

This is the only step that needs an interactive check-in: the multi-graph refactor is a substantial fork; do not push without the user's say-so.

---

## Self-Review

**Spec coverage check:**
- One MCP process, N graphs → Tasks 5, 9, 10 (GraphContext, Running, state machine).
- Global `search` w/ optional `graph` → Task 12.
- `execute`/`introspect`/`validate` w/ required `graph` → Tasks 11, 13, 14.
- Prefixed operation tool names → Tasks 6, 7.
- Local + OCI manifest loaders → Tasks 1, 2, 17, 18.
- All graphs loaded at startup, no hot reload → Task 10 (state machine collapse, removed update_schema/update_operations).
- Unknown graph → MCP error listing available → Tasks 11–14 (each dispatcher's "Unknown graph" branch).
- `UpstreamAuthRequired` typed error variant → Task 15.
- Telemetry attribute → Task 16.

**Open questions deferred to implementation judgement:**
- Search result cap when no `graph` is given: currently per-graph cap of 5; revisit during Task 12 if noisy.
- OCI crate choice: locked to `oci-distribution` in Task 17. Swap if it proves unwieldy.
- Separator: locked to `__` in Task 7 (`tests assert "g__GetId"`).
- `validate` against query referencing unknown types: surface normal validation error (Task 14 default).

**Type consistency:**
- `GraphContext` field names match across Tasks 5, 6, 9, 11, 12, 13, 14.
- `Input.graph` field present and named identically in `Execute::Input`, `Search::Input`, `Introspect::Input`, `Validate::Input`.
- `new_dispatcher` constructor naming consistent across Search/Introspect/Validate (Execute keeps its existing `new`).

**Known plan gaps (acknowledged):**
- Apps integration (Task 9 leaves `apps: vec![]`). The fork's PoC scope does not address apps. Add a follow-up plan if apps must keep working in multi-graph mode.
- Prompts integration is also stubbed to `vec![]` for the same reason.
- The state-machine's `Starting` state still exists in the codebase; the rewrite in Task 10 collapses it into `Loading`. Verify during implementation that no other code path constructs `Starting` directly — if so, delete it too.
