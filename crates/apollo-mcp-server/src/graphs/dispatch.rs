//! Multi-graph tool dispatchers.
//!
//! Each function takes the configured `HashMap<String, GraphContext>` plus the
//! tool's JSON arguments and routes to the right graph. They're plain async
//! functions (not methods on tool structs) so they can be wired into Running
//! without restructuring the existing single-graph tool types.

use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::{Name, Schema, ast::OperationType, validation::Valid};
use apollo_schema_index::Options;
use http::request::Parts;
use parking_lot::Mutex;
use rmcp::model::{CallToolResult, Content, ErrorCode, JsonObject};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::RwLock;

use crate::errors::McpError;
use crate::introspection::minify::MinifyExt;
use crate::introspection::tools::execute::Execute;
use crate::operations::{MutationMode, execute_operation};
use crate::schema_tree_shake::{DepthLimit, SchemaTreeShaker};
use apollo_compiler::ast::{Field, Selection};
use apollo_mcp_rhai::RhaiEngine;

use super::context::GraphContext;

/// Per-graph cap on search results (mirrors the legacy single-graph cap).
const MAX_SEARCH_RESULTS_PER_GRAPH: usize = 5;

pub type Graphs = Arc<RwLock<HashMap<String, GraphContext>>>;

#[derive(Deserialize)]
struct ExecuteInput {
    graph: String,
    query: String,
    variables: Option<Value>,
}

#[derive(Deserialize)]
struct SearchInput {
    terms: Vec<String>,
    #[serde(default)]
    graph: Option<String>,
}

#[derive(Deserialize)]
struct IntrospectInput {
    graph: String,
    #[serde(default)]
    type_name: Option<String>,
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(Deserialize)]
struct ValidateInput {
    graph: String,
    query: String,
}

fn invalid_input(err: impl std::fmt::Display) -> CallToolResult {
    CallToolResult::error(vec![Content::text(format!("Invalid input: {err}"))])
}

fn unknown_graph(name: &str, available: &[String]) -> CallToolResult {
    CallToolResult::error(vec![Content::text(format!(
        "Unknown graph '{}'. Available graphs: {}",
        name,
        available.join(", ")
    ))])
}

fn parse_args<T: for<'de> Deserialize<'de>>(args: Option<&JsonObject>) -> Result<T, String> {
    let raw = match args {
        Some(v) => Value::Object(v.clone()),
        None => Value::Null,
    };
    serde_json::from_value(raw).map_err(|e| e.to_string())
}

/// Execute a query against the named graph.
#[tracing::instrument(skip_all, fields(apollo.mcp.graph_name = tracing::field::Empty))]
pub async fn dispatch_execute(
    graphs: &Graphs,
    execute_tool: &Execute,
    args: Option<&JsonObject>,
    axum_parts: Option<&Parts>,
    rhai_engine: &Arc<Mutex<RhaiEngine>>,
) -> Result<CallToolResult, McpError> {
    let input: ExecuteInput = match parse_args(args) {
        Ok(v) => v,
        Err(e) => return Ok(invalid_input(e)),
    };
    tracing::Span::current().record("apollo.mcp.graph_name", input.graph.as_str());

    let graphs_read = graphs.read().await;
    let Some(ctx) = graphs_read.get(&input.graph) else {
        return Ok(unknown_graph(
            &input.graph,
            &graphs_read.keys().cloned().collect::<Vec<_>>(),
        ));
    };

    let base = ctx.credentials.headers_for(&ctx.headers, None);
    let effective_headers = if let Some(parts) = axum_parts {
        crate::headers::build_request_headers(
            &base,
            &ctx.forward_headers,
            &parts.headers,
            &parts.extensions,
            false,
        )
    } else {
        base
    };

    let exec_args = serde_json::json!({
        "query": input.query,
        "variables": input.variables,
    });

    let graph_name = ctx.name.clone();
    let endpoint = ctx.endpoint.clone();
    drop(graphs_read);

    let mut result = execute_operation(
        execute_tool,
        &effective_headers,
        exec_args.as_object(),
        &endpoint,
        rhai_engine,
        axum_parts,
        "execute",
    )
    .await?;

    // If the upstream signaled 401, tag the structured error with the graph name
    // so a future v2 mediator can drive a per-graph re-auth flow.
    if let Some(structured) = result.structured_content.as_mut()
        && let Some(obj) = structured.as_object_mut()
        && obj.get("error").and_then(|v| v.as_str()) == Some("upstream_auth_required")
    {
        obj.insert("graph".to_string(), Value::String(graph_name));
    }

    Ok(result)
}

/// Search one or all graphs.
pub async fn dispatch_search(
    graphs: &Graphs,
    leaf_depth: usize,
    minify: bool,
    args: Option<&JsonObject>,
) -> Result<CallToolResult, McpError> {
    let input: SearchInput = match parse_args(args) {
        Ok(v) => v,
        Err(e) => return Ok(invalid_input(e)),
    };

    let graphs_read = graphs.read().await;
    let targets: Vec<&GraphContext> = if let Some(name) = &input.graph {
        match graphs_read.get(name) {
            Some(ctx) => vec![ctx],
            None => {
                let avail: Vec<String> = graphs_read.keys().cloned().collect();
                return Ok(unknown_graph(name, &avail));
            }
        }
    } else {
        graphs_read.values().collect()
    };

    let mut contents: Vec<Content> = Vec::new();
    for ctx in targets {
        let per_graph = search_one(ctx, &input.terms, leaf_depth, minify)?;
        if per_graph.is_empty() {
            continue;
        }
        contents.push(Content::text(format!("# graph: {}", ctx.name)));
        contents.extend(per_graph);
    }
    Ok(CallToolResult::success(contents))
}

fn search_one(
    ctx: &GraphContext,
    terms: &[String],
    leaf_depth: usize,
    minify: bool,
) -> Result<Vec<Content>, McpError> {
    let mut root_paths = ctx
        .search_index
        .search(terms.to_vec(), Options::default())
        .map_err(|e| {
            McpError::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to search index for graph {}: {e}", ctx.name),
                None,
            )
        })?;
    root_paths.truncate(MAX_SEARCH_RESULTS_PER_GRAPH);

    // schema is behind an async RwLock; this fn is sync, so we use try_read.
    // In practice this is only called while holding the outer read lock on
    // graphs, and the schema lock is only ever held by the dispatcher.
    let schema = ctx.schema.try_read().map_err(|_| {
        McpError::new(
            ErrorCode::INTERNAL_ERROR,
            format!("Schema busy for graph {}", ctx.name),
            None,
        )
    })?;

    let allow_mutations = !matches!(ctx.mutation_mode, MutationMode::None);
    let mut tree_shaker = SchemaTreeShaker::new(&schema);
    for root_path in root_paths {
        let path_len = root_path.inner.len();
        for (i, path_node) in root_path.inner.into_iter().enumerate() {
            if let Some(extended_type) = schema.types.get(path_node.node_type.as_str()) {
                let (selection_set, depth) = if i == path_len - 1 {
                    (None, DepthLimit::Limited(leaf_depth))
                } else {
                    (
                        path_node.field_name.as_ref().map(|fname| {
                            vec![Selection::Field(apollo_compiler::Node::from(Field {
                                alias: Default::default(),
                                name: Name::new_unchecked(fname),
                                arguments: Default::default(),
                                selection_set: Default::default(),
                                directives: Default::default(),
                            }))]
                        }),
                        DepthLimit::Limited(1),
                    )
                };
                tree_shaker.retain_type(extended_type, selection_set.as_ref(), depth);
            }
            for field_arg in path_node.field_args {
                if let Some(extended_type) = schema.types.get(field_arg.as_str()) {
                    tree_shaker.retain_type(extended_type, None, DepthLimit::Unlimited);
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
                    .root_operation(OperationType::Mutation)
                    .is_none_or(|n| t.name() != n || allow_mutations)
        })
        .map(|(_, t)| {
            if minify {
                t.minify()
            } else {
                t.serialize().to_string()
            }
        })
        .map(Content::text)
        .collect())
}

/// Introspect a single type in the named graph.
pub async fn dispatch_introspect(
    graphs: &Graphs,
    minify: bool,
    args: Option<&JsonObject>,
) -> Result<CallToolResult, McpError> {
    let input: IntrospectInput = match parse_args(args) {
        Ok(v) => v,
        Err(e) => return Ok(invalid_input(e)),
    };

    let schema_lock = {
        let graphs_read = graphs.read().await;
        let Some(ctx) = graphs_read.get(&input.graph) else {
            return Ok(unknown_graph(
                &input.graph,
                &graphs_read.keys().cloned().collect::<Vec<_>>(),
            ));
        };
        ctx.schema.clone()
    };

    let schema = schema_lock.read().await;
    let starting_type = input
        .type_name
        .as_deref()
        .or_else(|| {
            schema
                .root_operation(OperationType::Query)
                .map(Name::as_str)
        })
        .map(|s| s.to_string())
        .ok_or_else(|| {
            McpError::new(
                ErrorCode::INVALID_PARAMS,
                "No type_name provided and no root Query type in schema".to_string(),
                None,
            )
        })?;

    let depth = input.depth.unwrap_or(3);
    let body = introspect_type(&schema, &starting_type, depth, minify);
    Ok(CallToolResult::success(vec![Content::text(body)]))
}

fn introspect_type(schema: &Valid<Schema>, type_name: &str, depth: usize, minify: bool) -> String {
    let Some(start) = schema.types.get(type_name) else {
        return format!("Type '{type_name}' not found in schema");
    };
    let mut tree_shaker = SchemaTreeShaker::new(schema);
    tree_shaker.retain_type(start, None, DepthLimit::Limited(depth));
    let shaken = tree_shaker.shaken().unwrap_or_else(|s| s.partial);
    shaken
        .types
        .iter()
        .filter(|(_, t)| !t.is_built_in())
        .map(|(_, t)| {
            if minify {
                t.minify()
            } else {
                t.serialize().to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Validate a GraphQL document against the named graph's schema.
pub async fn dispatch_validate(
    graphs: &Graphs,
    args: Option<&JsonObject>,
) -> Result<CallToolResult, McpError> {
    let input: ValidateInput = match parse_args(args) {
        Ok(v) => v,
        Err(e) => return Ok(invalid_input(e)),
    };

    let schema_lock = {
        let graphs_read = graphs.read().await;
        let Some(ctx) = graphs_read.get(&input.graph) else {
            return Ok(unknown_graph(
                &input.graph,
                &graphs_read.keys().cloned().collect::<Vec<_>>(),
            ));
        };
        ctx.schema.clone()
    };

    let schema = schema_lock.read().await;
    match apollo_compiler::ExecutableDocument::parse_and_validate(
        &schema,
        input.query.as_str(),
        "query.graphql",
    ) {
        Ok(_) => Ok(CallToolResult::success(vec![Content::text(
            "Document is valid.",
        )])),
        Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
            "Validation errors:\n{e}"
        ))])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphs::{GraphContext, credentials::default_provider};
    use apollo_compiler::Schema;
    use apollo_schema_index::{OperationType, SchemaIndex};
    use reqwest::header::HeaderMap;
    use std::collections::HashMap;
    use std::ops::Deref;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn text_of(result: CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| {
                if let rmcp::model::RawContent::Text(t) = c.deref() {
                    Some(t.text.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn ctx_for(name: &str, sdl: &str, endpoint: &str) -> GraphContext {
        let schema = Schema::parse(sdl, "s.graphql").unwrap().validate().unwrap();
        let locked = schema.clone();
        let idx = SchemaIndex::new(&locked, OperationType::Query.into(), 15_000_000).unwrap();
        GraphContext {
            name: name.into(),
            schema: Arc::new(RwLock::new(schema)),
            endpoint: url::Url::parse(endpoint).unwrap(),
            headers: HeaderMap::new(),
            forward_headers: vec![],
            operations: Arc::new(RwLock::new(vec![])),
            search_index: idx,
            mutation_mode: MutationMode::None,
            custom_scalar_map: None,
            credentials: default_provider(),
        }
    }

    fn obj(v: serde_json::Value) -> Option<JsonObject> {
        v.as_object().cloned()
    }

    #[tokio::test]
    async fn search_no_graph_arg_searches_all_graphs() {
        let mut map = HashMap::new();
        map.insert(
            "a".to_string(),
            ctx_for("a", "type Query { alpha: String }", "http://a.test/"),
        );
        map.insert(
            "b".to_string(),
            ctx_for("b", "type Query { beta: String }", "http://b.test/"),
        );
        let graphs: Graphs = Arc::new(RwLock::new(map));

        let args = obj(serde_json::json!({"terms": ["alpha", "beta"]}));
        let result = dispatch_search(&graphs, 1, false, args.as_ref())
            .await
            .unwrap();

        let combined = text_of(result);
        assert!(combined.contains("# graph: a"));
        assert!(combined.contains("# graph: b"));
    }

    #[tokio::test]
    async fn search_omits_graphs_with_no_matches() {
        let mut map = HashMap::new();
        map.insert(
            "match".to_string(),
            ctx_for(
                "match",
                "type Query { glacier: Glacier } type Glacier { id: ID! }",
                "http://m.test/",
            ),
        );
        map.insert(
            "nomatch".to_string(),
            ctx_for(
                "nomatch",
                "type Query { penguin: Penguin } type Penguin { id: ID! }",
                "http://n.test/",
            ),
        );
        let graphs: Graphs = Arc::new(RwLock::new(map));

        let args = obj(serde_json::json!({"terms": ["glacier"]}));
        let result = dispatch_search(&graphs, 1, false, args.as_ref())
            .await
            .unwrap();

        let combined = text_of(result);
        assert!(
            combined.contains("# graph: match"),
            "expected matching graph in output: {combined}"
        );
        assert!(
            !combined.contains("# graph: nomatch"),
            "expected non-matching graph to be omitted: {combined}"
        );
    }

    #[tokio::test]
    async fn search_unknown_graph_errors() {
        let mut map = HashMap::new();
        map.insert(
            "a".to_string(),
            ctx_for("a", "type Query { alpha: String }", "http://a.test/"),
        );
        let graphs: Graphs = Arc::new(RwLock::new(map));

        let args = obj(serde_json::json!({"terms": ["alpha"], "graph": "nope"}));
        let result = dispatch_search(&graphs, 1, false, args.as_ref())
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn execute_unknown_graph_errors() {
        use crate::introspection::tools::execute::Execute;
        let mut map = HashMap::new();
        map.insert(
            "a".to_string(),
            ctx_for("a", "type Query { alpha: String }", "http://a.test/"),
        );
        let graphs: Graphs = Arc::new(RwLock::new(map));
        let execute_tool = Execute::new(MutationMode::None, None);
        let rhai = Arc::new(parking_lot::Mutex::new(apollo_mcp_rhai::RhaiEngine::new(
            "rhai",
        )));

        let args = obj(serde_json::json!({"graph": "nope", "query": "{ alpha }"}));
        let result = dispatch_execute(&graphs, &execute_tool, args.as_ref(), None, &rhai)
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn introspect_unknown_graph_errors() {
        let mut map = HashMap::new();
        map.insert(
            "a".to_string(),
            ctx_for("a", "type Query { alpha: String }", "http://a.test/"),
        );
        let graphs: Graphs = Arc::new(RwLock::new(map));

        let args = obj(serde_json::json!({"graph": "nope"}));
        let result = dispatch_introspect(&graphs, false, args.as_ref())
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn introspect_returns_root_query_when_no_type_given() {
        let mut map = HashMap::new();
        map.insert(
            "a".to_string(),
            ctx_for("a", "type Query { alpha: String }", "http://a.test/"),
        );
        let graphs: Graphs = Arc::new(RwLock::new(map));

        let args = obj(serde_json::json!({"graph": "a"}));
        let result = dispatch_introspect(&graphs, false, args.as_ref())
            .await
            .unwrap();
        let combined = text_of(result);
        assert!(combined.contains("type Query"));
        assert!(combined.contains("alpha"));
    }

    #[tokio::test]
    async fn validate_returns_success_on_valid_doc() {
        let mut map = HashMap::new();
        map.insert(
            "a".to_string(),
            ctx_for("a", "type Query { alpha: String }", "http://a.test/"),
        );
        let graphs: Graphs = Arc::new(RwLock::new(map));

        let args = obj(serde_json::json!({"graph": "a", "query": "{ alpha }"}));
        let result = dispatch_validate(&graphs, args.as_ref()).await.unwrap();
        assert_ne!(result.is_error, Some(true));
    }

    #[tokio::test]
    async fn validate_returns_error_on_invalid_doc() {
        let mut map = HashMap::new();
        map.insert(
            "a".to_string(),
            ctx_for("a", "type Query { alpha: String }", "http://a.test/"),
        );
        let graphs: Graphs = Arc::new(RwLock::new(map));

        let args = obj(serde_json::json!({"graph": "a", "query": "{ nonexistent }"}));
        let result = dispatch_validate(&graphs, args.as_ref()).await.unwrap();
        assert_eq!(result.is_error, Some(true));
    }
}
