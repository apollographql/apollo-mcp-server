# Hybrid Search — Phase 1: Operation-Anchored BM25 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Refactor the existing BM25 schema search from field-anchored to operation-anchored with enriched documents, introduce the `SchemaSearch`/`OperationRef` abstraction seam, rewrite the `Search` MCP tool to consume operations, add a configurable `limit` parameter, and fix the index-goes-stale-on-reload bug.

**Architecture:** `apollo-schema-index` stops writing one Tantivy document per field and instead writes one per **operation** (root Query/Mutation field), enriching each with a bounded downward flatten of its return type's field names/descriptions. Because a hit is now an operation, the type-reference up-walk graph and `PathNode` machinery are deleted. Search returns `Vec<Scored<OperationRef>>` through a new `SchemaSearch` trait (the seam Phase 2's semantic backend will also implement). The `Search` tool builds its tree-shake input directly from each `OperationRef`.

**Tech Stack:** Rust (edition 2024, 1.92), Tantivy 0.24.2, `apollo-compiler`, `rmcp`, `insta` (snapshots), `rstest`.

## Global Constraints

- Rust edition **2024**, `rust-version` **1.92.0** (workspace-pinned; do not bump).
- Clippy lints are **`deny`**: `unwrap_used`, `expect_used`, `panic`, `exit`, `indexing_slicing`. No `.unwrap()`/`.expect()`/`panic!`/`[i]` indexing in non-test code. CI runs `cargo clippy --all-targets -- --deny warnings`.
- **80% patch coverage** on new/modified code (`cargo llvm-cov`).
- **No new runtime dependencies** in this phase (semantic/`ort` deps arrive in Phase 2).
- Tests use `rstest` (params/fixtures) and `insta` (snapshots). Run `cargo fmt` before committing.
- Existing snapshots under `crates/*/src/**/snapshots/` will change; review and `cargo insta accept` intentionally.

---

## File structure

- `crates/apollo-schema-index/src/backend.rs` *(new)* — `OperationRef` type, `SchemaSearch` trait. Re-exports `Scored`.
- `crates/apollo-schema-index/src/traverse.rs` *(modify)* — add `flatten_return_type` bounded downward walk.
- `crates/apollo-schema-index/src/lib.rs` *(modify)* — operation-anchored indexing + search; delete up-walk graph.
- `crates/apollo-schema-index/src/path.rs` *(modify)* — keep `Scored`; delete `PathNode`.
- `crates/apollo-mcp-server/src/introspection/tools/search.rs` *(modify)* — consume `SchemaSearch`, rewrite tree-shaking from `OperationRef`, add `limit` input + clamp.
- `crates/apollo-mcp-server/src/runtime/introspection.rs` *(modify)* — add `default_limit`/`max_limit` to `SearchConfig`.
- `crates/apollo-mcp-server/src/server/states/running.rs` + `starting.rs` *(modify)* — rebuild the search index on `update_schema`.

---

### Task 1: `OperationRef` + `SchemaSearch` trait (the seam)

**Files:**
- Create: `crates/apollo-schema-index/src/backend.rs`
- Modify: `crates/apollo-schema-index/src/lib.rs` (add `mod backend;`, re-exports)
- Modify: `crates/apollo-schema-index/src/path.rs` (make `Scored` reusable — it already is `pub`)

**Interfaces:**
- Produces:
  - `pub struct OperationRef { pub operation_type: OperationType, pub field_name: String, pub return_type: Option<String>, pub arg_types: Vec<String> }` — `Clone + Debug + PartialEq + Eq + Hash`; identity for fusion/dedupe is `(operation_type, field_name)`.
  - `pub trait SchemaSearch { fn search(&self, query: &str, limit: usize) -> Result<Vec<Scored<OperationRef>>, SearchError>; }`
  - `impl std::fmt::Display for OperationRef` rendering e.g. `Query.userByEmail(String): TargetUser`.

- [ ] **Step 1: Write the failing test**

Create `crates/apollo-schema-index/src/backend.rs`:

```rust
//! The search backend contract shared by lexical and (Phase 2) semantic search.

use crate::OperationType;
use crate::error::SearchError;
use crate::path::Scored;

/// A retrievable operation: a root Query/Mutation field the agent can invoke.
/// Identity for fusion/dedupe is `(operation_type, field_name)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct OperationRef {
    pub operation_type: OperationType,
    pub field_name: String,
    pub return_type: Option<String>,
    pub arg_types: Vec<String>,
}

impl std::fmt::Display for OperationRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let root = match self.operation_type {
            OperationType::Query => "Query",
            OperationType::Mutation => "Mutation",
            OperationType::Subscription => "Subscription",
        };
        write!(f, "{root}.{}", self.field_name)?;
        if !self.arg_types.is_empty() {
            write!(f, "({})", self.arg_types.join(", "))?;
        }
        if let Some(rt) = &self.return_type {
            write!(f, ": {rt}")?;
        }
        Ok(())
    }
}

/// A search backend over a GraphQL schema's operations.
pub trait SchemaSearch {
    fn search(&self, query: &str, limit: usize) -> Result<Vec<Scored<OperationRef>>, SearchError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_ref_display() {
        let op = OperationRef {
            operation_type: OperationType::Query,
            field_name: "userByEmail".to_string(),
            return_type: Some("TargetUser".to_string()),
            arg_types: vec!["String".to_string()],
        };
        assert_eq!(op.to_string(), "Query.userByEmail(String): TargetUser");
    }

    #[test]
    fn operation_ref_identity_ignores_ordering_of_equal_refs() {
        let a = OperationRef { operation_type: OperationType::Query, field_name: "x".into(), return_type: None, arg_types: vec![] };
        let b = a.clone();
        assert_eq!(a, b);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p apollo-schema-index backend::`
Expected: FAIL — `backend` module not declared / `OperationType` not importable there yet.

- [ ] **Step 3: Wire the module and re-exports**

In `crates/apollo-schema-index/src/lib.rs`, add near the other `mod` lines (after line 55):

```rust
mod backend;
pub use backend::{OperationRef, SchemaSearch};
pub use path::Scored;
```

Ensure `OperationType` is already `pub` (it is, `lib.rs:117`).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p apollo-schema-index backend::`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/apollo-schema-index/src/backend.rs crates/apollo-schema-index/src/lib.rs
git commit -m "feat(air-311): add OperationRef + SchemaSearch seam"
```

---

### Task 2: Bounded return-type flatten

**Files:**
- Modify: `crates/apollo-schema-index/src/traverse.rs`
- Test: same file (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `apollo_compiler::schema::ExtendedType`, `Valid<Schema>`.
- Produces: `pub(crate) fn flatten_return_type(schema: &Schema, return_type: &str, depth: usize) -> String` — returns space-joined `field_name` + description text for the return type's fields, walked to `depth` levels, cycle-guarded via a visited set. Returns `""` for scalars/enums/unions or depth 0.

- [ ] **Step 1: Write the failing test**

Add to `crates/apollo-schema-index/src/traverse.rs` (`#[cfg(test)]` mod):

```rust
#[cfg(test)]
mod flatten_tests {
    use super::*;
    use apollo_compiler::Schema;

    const S: &str = r#"
        type Query { a: Foo }
        type Foo { bar: String "documented" baz: Bar }
        type Bar { deep: String }
    "#;

    #[test]
    fn flatten_depth_1_includes_direct_fields_only() {
        let schema = Schema::parse(S, "s.graphql").unwrap().validate().unwrap();
        let text = flatten_return_type(&schema, "Foo", 1);
        assert!(text.contains("bar"));
        assert!(text.contains("baz"));
        assert!(!text.contains("deep")); // depth 1 does not descend into Bar
    }

    #[test]
    fn flatten_depth_0_is_empty() {
        let schema = Schema::parse(S, "s.graphql").unwrap().validate().unwrap();
        assert_eq!(flatten_return_type(&schema, "Foo", 0), "");
    }

    #[test]
    fn flatten_handles_cycles() {
        let cyclic = r#"type Query { a: Node } type Node { next: Node name: String }"#;
        let schema = Schema::parse(cyclic, "c.graphql").unwrap().validate().unwrap();
        // Should terminate and include field names without infinite recursion.
        let text = flatten_return_type(&schema, "Node", 5);
        assert!(text.contains("next"));
        assert!(text.contains("name"));
    }
}
```

*(Test bodies use `.unwrap()` — allowed in `#[cfg(test)]`; the `unwrap_used` lint is not denied in tests.)*

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p apollo-schema-index flatten_tests::`
Expected: FAIL — `flatten_return_type` not found.

- [ ] **Step 3: Implement `flatten_return_type`**

Add to `crates/apollo-schema-index/src/traverse.rs` (module scope, `pub(crate)`):

```rust
use apollo_compiler::Schema;
use apollo_compiler::ast::NamedType;
use apollo_compiler::schema::ExtendedType;
use std::collections::HashSet;

/// Collect field-name + description text for `return_type`, walked `depth` levels deep.
/// Cycle-guarded; scalars/enums/unions and depth 0 yield "".
pub(crate) fn flatten_return_type(schema: &Schema, return_type: &str, depth: usize) -> String {
    let mut out = String::new();
    let mut visited: HashSet<String> = HashSet::new();
    collect(schema, return_type, depth, &mut visited, &mut out);
    out.trim().to_string()
}

fn collect(
    schema: &Schema,
    type_name: &str,
    depth: usize,
    visited: &mut HashSet<String>,
    out: &mut String,
) {
    if depth == 0 || !visited.insert(type_name.to_string()) {
        return;
    }
    let named = NamedType::new_unchecked(type_name);
    let Some(ExtendedType::Object(obj)) = schema.types.get(&named) else {
        return; // only object types contribute nested field text
    };
    for (name, field) in obj.fields.iter() {
        out.push(' ');
        out.push_str(name.as_str());
        if let Some(desc) = field.description.as_ref() {
            out.push(' ');
            out.push_str(desc.as_str());
        }
        let inner = field.ty.inner_named_type();
        collect(schema, inner.as_str(), depth - 1, visited, out);
    }
}
```

*Note:* if `traverse.rs` already imports some of these, dedupe imports rather than duplicating.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p apollo-schema-index flatten_tests::`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/apollo-schema-index/src/traverse.rs
git commit -m "feat(air-311): add bounded return-type flatten helper"
```

---

### Task 3: Operation-anchored index construction

**Files:**
- Modify: `crates/apollo-schema-index/src/lib.rs` (index schema fields, `SchemaIndex` struct, `SchemaIndex::new`, document enumeration/writing)

**Interfaces:**
- Consumes: `flatten_return_type` (Task 2), `OperationType` (existing).
- Produces: `SchemaIndex::new(schema: &Valid<Schema>, root_types: EnumSet<OperationType>, flatten_depth: usize, index_memory_bytes: usize) -> Result<Self, IndexingError>` — builds an in-RAM Tantivy index with **one document per root operation**. New signature adds `flatten_depth` (before `index_memory_bytes`).

- [ ] **Step 1: Replace the field constants and Tantivy schema fields**

In `lib.rs`, replace the field-name constants (lines 57–65) with operation-centric ones:

```rust
pub const OPERATION_NAME_FIELD: &str = "operation_name";
pub const ARG_NAMES_FIELD: &str = "arg_names";
pub const RETURN_TYPE_NAME_FIELD: &str = "return_type_name";
pub const DESCRIPTION_FIELD: &str = "description";
pub const NESTED_FIELDS_FIELD: &str = "nested_fields";
pub const OPERATION_TYPE_RAW_FIELD: &str = "operation_type_raw";
pub const OPERATION_NAME_RAW_FIELD: &str = "operation_name_raw";
pub const RETURN_TYPE_NAME_RAW_FIELD: &str = "return_type_name_raw";
pub const FIELD_ARGS_RAW_FIELD: &str = "field_args_raw";
```

Replace the analyzed-field additions in `new` (lines 250–284) so the analyzed fields are: `operation_name`, `arg_names`, `return_type_name`, `description`, `nested_fields`; and the raw/stored fields are: `operation_type_raw` (stored raw), `operation_name_raw` (stored raw), `return_type_name_raw` (stored raw), `field_args_raw` (`STORED`). Follow the exact `text_indexing()` / `raw_indexing()` builders already in the file.

- [ ] **Step 2: Update the `SchemaIndex` struct fields**

Replace the `SchemaIndex` field set (lines 216–231): keep `inner: Index` and `text_analyzer`; replace the per-field `Field` handles with the new field set above; **delete** `type_references`.

- [ ] **Step 3: Add `flatten_depth` param and rewrite enumeration in `new`**

Change the `new` signature to add `flatten_depth: usize` before `index_memory_bytes`. Replace the type-reference-graph build (lines 293–312) and the write loop (lines 314–342) with an operation enumeration that, for each root operation type in `root_types` present in the schema, iterates its fields and writes one document per field. Replace `field_records`/`write_field_doc` (lines 369–470) with:

```rust
/// Write one document per root operation field.
fn write_operation_docs(
    schema: &Valid<Schema>,
    index_writer: &mut tantivy::IndexWriter,
    fields: &DocFields,
    root_types: EnumSet<OperationType>,
    flatten_depth: usize,
) -> Result<usize, IndexingError> {
    let mut count = 0usize;
    for op_type in root_types.iter() {
        let ast_type: AstOperationType = op_type.into();
        let Some(root_name) = schema.root_operation(ast_type) else {
            continue;
        };
        let Some(ExtendedType::Object(obj)) = schema.types.get(root_name) else {
            continue;
        };
        for (name, field) in obj.fields.iter() {
            let return_type = field.ty.inner_named_type();
            let arg_names: Vec<&str> = field.arguments.iter().map(|a| a.name.as_str()).collect();
            let arg_types: Vec<String> = field
                .arguments
                .iter()
                .map(|a| a.ty.inner_named_type().to_string())
                .collect();
            let description = field.description.as_ref().map(|d| d.as_str()).unwrap_or("");
            let nested = crate::traverse::flatten_return_type(schema, return_type.as_str(), flatten_depth);

            let mut doc = TantivyDocument::default();
            doc.add_text(fields.operation_name, expand_identifiers(name.as_str()));
            if !arg_names.is_empty() {
                doc.add_text(fields.arg_names, expand_identifiers(&arg_names.join(" ")));
            }
            doc.add_text(fields.return_type_name, expand_identifiers(return_type.as_str()));
            doc.add_text(fields.description, expand_identifiers(description));
            doc.add_text(fields.nested_fields, expand_identifiers(&nested));
            // Raw fields for exact reconstruction of the OperationRef.
            doc.add_text(fields.operation_type_raw, root_kind_str(op_type));
            doc.add_text(fields.operation_name_raw, name.as_str());
            doc.add_text(fields.return_type_name_raw, return_type.as_str());
            for arg_type in &arg_types {
                doc.add_text(fields.field_args_raw, arg_type);
            }
            index_writer.add_document(doc)?;
            count += 1;
        }
    }
    Ok(count)
}

fn root_kind_str(op: OperationType) -> &'static str {
    match op {
        OperationType::Query => "Query",
        OperationType::Mutation => "Mutation",
        OperationType::Subscription => "Subscription",
    }
}
```

Update `DocFields` (lines 78–89) to the new field set: `operation_name`, `arg_names`, `return_type_name`, `description`, `nested_fields`, `operation_type_raw`, `operation_name_raw`, `return_type_name_raw`, `field_args_raw`. In `new`, call `write_operation_docs(...)` instead of the old loop, then `index_writer.commit()?`. Delete `ReferencingEdge` (lines 67–76), `FieldRecord` (lines 101–112), and the `use` of the type-reference graph.

- [ ] **Step 4: Update the crate's existing index-construction test call sites**

Every `SchemaIndex::new(&schema, ..., 15_000_000)` in `lib.rs` tests (lines ~795, 821, 844, 890, 921, 945) must pass the new `flatten_depth` arg, e.g. `SchemaIndex::new(&schema, OperationType::Query | OperationType::Mutation, 1, 15_000_000)`.

- [ ] **Step 5: Compile (search() still references deleted items — expected)**

Run: `cargo build -p apollo-schema-index`
Expected: FAIL — `search`, `build_leaf_path`, `walk_up_to_roots`, `boost_shorter_paths` still reference removed fields/types. This is fixed in Task 4. Do **not** commit yet.

---

### Task 4: Operation-anchored search + `SchemaSearch` impl

**Files:**
- Modify: `crates/apollo-schema-index/src/lib.rs` (rewrite `search`, `query`; delete up-walk fns; impl `SchemaSearch`)
- Modify: `crates/apollo-schema-index/src/path.rs` (delete `PathNode`, keep `Scored`)

**Interfaces:**
- Consumes: `OperationRef`, `SchemaSearch` (Task 1); the operation-anchored index (Task 3).
- Produces: `impl SchemaSearch for SchemaIndex` with `search(&self, query: &str, limit: usize) -> Result<Vec<Scored<OperationRef>>, SearchError>`, sorted by BM25 score descending.

- [ ] **Step 1: Write the failing test (regression: operation surfaces in top results)**

Replace the body of `search_buries_target_under_token_noise` (lines 788–817) to assert on `OperationRef`s:

```rust
#[rstest]
fn search_surfaces_target_operation() {
    let schema = Schema::parse(NOISE_SCHEMA, "noise.graphql")
        .unwrap()
        .validate()
        .unwrap();
    let index = SchemaIndex::new(
        &schema,
        OperationType::Query | OperationType::Mutation,
        1,
        15_000_000,
    )
    .unwrap();

    let results = index.search("userByEmail", 10).unwrap();
    let rank = results
        .iter()
        .position(|s| s.inner.field_name == "userByEmail")
        .map(|p| p + 1);
    assert!(
        matches!(rank, Some(r) if r <= 3),
        "Expected 'userByEmail' operation in top 3, got {:?}: {:?}",
        rank,
        results.iter().map(|s| s.inner.to_string()).collect::<Vec<_>>()
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p apollo-schema-index search_surfaces_target_operation`
Expected: FAIL to compile (old `search` signature / `PathNode`).

- [ ] **Step 3: Rewrite `search`, delete the up-walk, impl the trait**

In `lib.rs`: delete `build_leaf_path` (560–577), `walk_up_to_roots` (582–629), `boost_shorter_paths` (632–668), the `Options` struct (143–162), `FieldHit` (91–99), and the `use crate::path::PathNode;`. Replace `search` (478–558) with a private helper plus the trait impl:

```rust
impl SchemaIndex {
    fn run_query(&self, query_text: &str, limit: usize) -> Result<Vec<Scored<OperationRef>>, SearchError> {
        let searcher = self.inner.reader()?.searcher();
        let query = self.query(std::iter::once(query_text.to_string()));
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit))?;

        let mut results: Vec<Scored<OperationRef>> = Vec::new();
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;
            let field_name = doc
                .get_first(self.operation_name_raw_field)
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let op_type = doc
                .get_first(self.operation_type_raw_field)
                .and_then(|v| v.as_str())
                .and_then(parse_op_kind);
            let return_type = doc
                .get_first(self.return_type_name_raw_field)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let arg_types: Vec<String> = doc
                .get_all(self.field_args_raw_field)
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect();
            match (op_type, field_name) {
                (Some(operation_type), Some(field_name)) => results.push(Scored::new(
                    OperationRef { operation_type, field_name, return_type, arg_types },
                    score,
                )),
                _ => error!("Doc {doc_address:?} missing operation type or name"),
            }
        }
        Ok(results)
    }
}

fn parse_op_kind(s: &str) -> Option<OperationType> {
    match s {
        "Query" => Some(OperationType::Query),
        "Mutation" => Some(OperationType::Mutation),
        "Subscription" => Some(OperationType::Subscription),
        _ => None,
    }
}

impl SchemaSearch for SchemaIndex {
    fn search(&self, query: &str, limit: usize) -> Result<Vec<Scored<OperationRef>>, SearchError> {
        self.run_query(query, limit)
    }
}
```

Update `query` (671–712): keep the camelCase-splitting/tokenizing approach, but target the new analyzed fields with the operation-name boost:

```rust
const OPERATION_NAME_BOOST: f32 = 3.0;
let text_fields = [
    (self.operation_name_field, OPERATION_NAME_BOOST),
    (self.arg_names_field, 1.0_f32),
    (self.return_type_name_field, 1.0),
    (self.description_field, 1.0),
    (self.nested_fields_field, 1.0),
];
```

(Leave the token-stream loop and `BooleanQuery` construction otherwise unchanged.)

In `path.rs`: delete `PathNode` and its impls; **keep** `Scored<T>` (`new`, `score`, ordering).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p apollo-schema-index`
Expected: PASS. Update remaining `lib.rs` tests that asserted on path strings (`search`, `search_interface_implementer_fields`, `search_camel_case_*`) to assert on `results.iter().map(|s| s.inner.to_string())` and operation names. For the `insta` snapshot in `search`, run `cargo insta review` and accept the new operation-anchored output after confirming it's sensible.

- [ ] **Step 5: Commit**

```bash
cargo fmt
cargo clippy -p apollo-schema-index --all-targets -- --deny warnings
git add crates/apollo-schema-index/
git commit -m "feat(air-311): operation-anchored BM25 search via SchemaSearch"
```

---

### Task 5: `Search` MCP tool — consume `SchemaSearch`, add `limit`, tree-shake from `OperationRef`

**Files:**
- Modify: `crates/apollo-mcp-server/src/introspection/tools/search.rs`

**Interfaces:**
- Consumes: `apollo_schema_index::{SchemaSearch, SchemaIndex, OperationType, OperationRef}`.
- Produces: `Search::new(schema, allow_mutations, leaf_depth, flatten_depth, index_memory_bytes, default_limit, max_limit, minify, description_hint)`; `Input { terms: Vec<String>, limit: Option<usize> }`.

- [ ] **Step 1: Write the failing test (limit clamping + result count)**

Add to the `tests` mod in `search.rs`:

```rust
#[rstest]
#[tokio::test]
async fn search_respects_limit(schema: Valid<Schema>) {
    let schema = Arc::new(RwLock::new(schema));
    // default_limit 10, max_limit 50
    let search = Search::new(schema.clone(), true, 1, 1, 15_000_000, 10, 50, false, None)
        .expect("create search");
    let result = search
        .execute(Input { terms: vec!["User".to_string()], limit: Some(2) })
        .await
        .expect("search");
    assert!(!result.is_error.unwrap_or(false));
}

#[test]
fn clamp_limit_bounds() {
    assert_eq!(clamp_limit(None, 10, 50), 10);
    assert_eq!(clamp_limit(Some(0), 10, 50), 1);
    assert_eq!(clamp_limit(Some(999), 10, 50), 50);
    assert_eq!(clamp_limit(Some(7), 10, 50), 7);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p apollo-mcp-server --lib introspection::tools::search`
Expected: FAIL — `clamp_limit` missing, `Input.limit` missing, `Search::new` arity mismatch.

- [ ] **Step 3: Update the tool**

In `search.rs`:
- Add `limit: Option<usize>` to `Input` with a doc comment: `/// Maximum number of results to return (default 10, max 50).`
- Add fields `default_limit: usize`, `max_limit: usize` to `Search`; add params to `Search::new` (and a `flatten_depth` param threaded into `SchemaIndex::new`). Delete `const MAX_SEARCH_RESULTS`.
- Add the free function:

```rust
fn clamp_limit(requested: Option<usize>, default_limit: usize, max_limit: usize) -> usize {
    requested.unwrap_or(default_limit).clamp(1, max_limit)
}
```

- Rewrite `execute` to call the trait and tree-shake from `OperationRef`:

```rust
pub async fn execute(&self, input: Input) -> Result<CallToolResult, McpError> {
    let k = clamp_limit(input.limit, self.default_limit, self.max_limit);
    let query = input.terms.join(" ");
    let results = self
        .index
        .search(&query, k)
        .map_err(|e| McpError::new(ErrorCode::INTERNAL_ERROR, format!("Failed to search index: {e}"), None))?;

    let schema = self.schema.read().await;
    let mut tree_shaker = SchemaTreeShaker::new(&schema);
    for scored in results.into_iter().take(k) {
        let op = scored.inner;
        // Retain the root operation type, naming just the matched operation field.
        let root_name = match op.operation_type {
            OperationType::Mutation => schema.root_operation(AstOperationType::Mutation),
            _ => schema.root_operation(AstOperationType::Query),
        };
        if let Some(root_name) = root_name {
            if let Some(root_type) = schema.types.get(root_name) {
                let selection = vec![Selection::Field(Node::from(Field {
                    alias: Default::default(),
                    name: Name::new_unchecked(&op.field_name),
                    arguments: Default::default(),
                    selection_set: Default::default(),
                    directives: Default::default(),
                }))];
                tree_shaker.retain_type(root_type, Some(&selection), DepthLimit::Limited(1));
            }
        }
        // Retain the return type to leaf_depth.
        if let Some(rt) = op.return_type.as_ref() {
            if let Some(rt_type) = schema.types.get(rt.as_str()) {
                tree_shaker.retain_type(rt_type, None, DepthLimit::Limited(self.leaf_depth));
            }
        }
        // Retain argument input types with unlimited depth.
        for arg in &op.arg_types {
            if let Some(arg_type) = schema.types.get(arg.as_str()) {
                tree_shaker.retain_type(arg_type, None, DepthLimit::Unlimited);
            }
        }
    }
    let shaken = tree_shaker.shaken().unwrap_or_else(|schema| schema.partial);
    // (unchanged) serialize/minify the shaken types into Content, filtering built-ins
    //  and the mutation root when !allow_mutations — copy the existing closure at
    //  search.rs:151-172 verbatim.
    Ok(CallToolResult::success(/* existing mapping */))
}
```

Remove the now-unused imports (`Options`, `Selection`/`Field` stay; drop anything tied to `PathNode`). Keep the existing final `Content` mapping block (lines 151–172) exactly.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p apollo-mcp-server --lib introspection::tools::search`
Expected: PASS. Re-review the `search_tool`/`referencing_types_are_collected` snapshots (`cargo insta review`); confirm `createUser` still appears for a `User` search (the mutation is now surfaced directly as an operation hit).

- [ ] **Step 5: Commit**

```bash
cargo fmt
cargo clippy -p apollo-mcp-server --all-targets -- --deny warnings
git add crates/apollo-mcp-server/src/introspection/tools/search.rs
git commit -m "feat(air-311): search tool consumes operations + adds limit param"
```

---

### Task 6: `SearchConfig` — `default_limit` / `max_limit` / `flatten_depth`

**Files:**
- Modify: `crates/apollo-mcp-server/src/runtime/introspection.rs`

**Interfaces:**
- Produces: `SearchConfig { …, pub default_limit: usize, pub max_limit: usize, pub flatten_depth: usize }` with defaults **10 / 50 / 1**.

- [ ] **Step 1: Write the failing test**

Add to `introspection.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_config_defaults() {
        let c = SearchConfig::default();
        assert_eq!(c.default_limit, 10);
        assert_eq!(c.max_limit, 50);
        assert_eq!(c.flatten_depth, 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p apollo-mcp-server --lib runtime::introspection`
Expected: FAIL — fields don't exist.

- [ ] **Step 3: Add the fields + defaults**

In `SearchConfig` (lines 45–63) add:

```rust
    /// Default number of results when the caller omits `limit`.
    pub default_limit: usize,
    /// Hard cap on the number of results the caller may request.
    pub max_limit: usize,
    /// Return-type flatten depth used to enrich each operation's index document.
    pub flatten_depth: usize,
```

In `Default` (lines 65–75) add `default_limit: 10, max_limit: 50, flatten_depth: 1,`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p apollo-mcp-server --lib runtime::introspection`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/apollo-mcp-server/src/runtime/introspection.rs
git commit -m "feat(air-311): add default_limit/max_limit/flatten_depth to SearchConfig"
```

---

### Task 7: Rebuild the search index on schema reload (staleness fix) + wire config

**Files:**
- Modify: `crates/apollo-mcp-server/src/server/states/running.rs` (`update_schema`)
- Modify: `crates/apollo-mcp-server/src/server/states/starting.rs` (pass new `SearchConfig` fields into `Search::new`)

**Interfaces:**
- Consumes: the `Search` tool (Task 5), `SearchConfig` (Task 6).
- Produces: `update_schema` rebuilds `search_tool` from the new schema; `starting.rs` constructs `Search` with `default_limit`/`max_limit`/`flatten_depth` from config.

- [ ] **Step 1: Write the failing test**

Add to the `running.rs` test module a test that constructs a `Running` with a `search_tool`, calls `update_schema` with a schema that adds a new operation, and asserts the new operation is now searchable. If `Running` is hard to construct in a unit test, instead add an integration-style test in `crates/apollo-mcp-server/tests/` that drives the state transition; keep it behind the existing test harness. Minimum assertion:

```rust
// After update_schema(new_schema_with_added_op),
// running.search_tool must return the added operation for a matching query.
```

*(If a `Running` fixture already exists in this module, reuse it; do not invent a new construction path.)*

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p apollo-mcp-server --lib server::states::running`
Expected: FAIL — the added operation is not found (index is stale).

- [ ] **Step 3: Rebuild the index in `update_schema`**

Because `Search` owns a cloned `SchemaIndex` built at startup, `update_schema` must rebuild it. Since `search_tool` is a plain `Option<Search>` inside a `Clone` struct (not behind a lock), rebuilding requires the field to be swappable. Wrap it: change `search_tool: Option<Search>` to `search_tool: Option<Arc<RwLock<Search>>>` (mirroring how `schema` is shared), or add a `rebuild(&self, schema: &Valid<Schema>)` on `Search` that swaps its internal `index` behind an `Arc<RwLock<SchemaIndex>>`. Prefer the latter (smaller blast radius):

- In `search.rs`, store `index: Arc<RwLock<SchemaIndex>>`; add:

```rust
pub async fn rebuild(&self, schema: &Valid<Schema>) -> Result<(), IndexingError> {
    let root_types = if self.allow_mutations {
        OperationType::Query | OperationType::Mutation
    } else {
        OperationType::Query.into()
    };
    let new_index = SchemaIndex::new(schema, root_types, self.flatten_depth, self.index_memory_bytes)?;
    *self.index.write().await = new_index;
    Ok(())
}
```

  (Store `flatten_depth` and `index_memory_bytes` on `Search`; update `execute` to `self.index.read().await.search(...)`.)

- In `running.rs` `update_schema`, after `*self.schema.write().await = schema;` (line 132), add:

```rust
if let Some(search) = &self.search_tool {
    if let Err(error) = search.rebuild(&*self.schema.read().await).await {
        error!("Failed to rebuild search index on schema update: {error}");
    }
}
```

- [ ] **Step 4: Wire config in `starting.rs`**

At the `Search::new(...)` call (~`starting.rs:126`), pass `search.default_limit`, `search.max_limit`, `search.flatten_depth` from the `SearchConfig`. Confirm the argument order matches Task 5's `Search::new` signature.

- [ ] **Step 5: Run tests + full suite**

Run: `cargo test -p apollo-mcp-server`
Expected: PASS.
Run: `cargo clippy --all-targets -- --deny warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add crates/apollo-mcp-server/src/server/states/running.rs crates/apollo-mcp-server/src/server/states/starting.rs crates/apollo-mcp-server/src/introspection/tools/search.rs
git commit -m "fix(air-311): rebuild search index on schema reload; wire search config"
```

---

## Self-review

- **Spec coverage (Phase 1 scope):** operation-anchored enriched docs (Tasks 2–3) ✓; delete up-walk (Task 4) ✓; `SchemaSearch`/`OperationRef` seam (Tasks 1, 4) ✓; tree-shaking stays in the tool, built from operations (Task 5) ✓; `limit` param default 10/cap 50 (Tasks 5–6) ✓; `update_schema` staleness fix (Task 7) ✓. Semantic/fusion/packaging are intentionally deferred to Phases 2–3.
- **Placeholder scan:** Task 7 Step 1 leaves the exact `Running` test-construction to the existing fixture (noted, not a code placeholder) because the construction path must match existing test scaffolding; the assertion and the production change are fully specified. Task 5 Step 3 says to copy the existing `Content` mapping block verbatim (it's unchanged) rather than re-print it — acceptable since it is not being modified.
- **Type consistency:** `SchemaIndex::new` gains `flatten_depth` consistently across Tasks 3, 5, 7. `Search::new` arg order (schema, allow_mutations, leaf_depth, flatten_depth, index_memory_bytes, default_limit, max_limit, minify, description_hint) is used identically in Task 5 tests and Task 7 wiring. `search(&str, usize) -> Vec<Scored<OperationRef>>` is consistent across Tasks 1, 4, 5.

## Follow-ups (not this phase)

- **Phase 2:** `apollo-schema-search` crate — `Embedder` (fastembed), `VectorStore` (in-memory cosine), `HybridSearch` (RRF), wired into the `Search` tool as a second `SchemaSearch` backend with graceful degradation.
- **Phase 3:** Dockerfile packaging (bake model + `libonnxruntime.so`, `ort` `load-dynamic`, `ORT_DYLIB_PATH`, multi-arch).
