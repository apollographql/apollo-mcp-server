#![allow(clippy::unwrap_used, clippy::expect_used)]
//! End-to-end multi-graph dispatch test.
//!
//! Stands up two mock GraphQL upstreams via mockito, constructs two
//! `GraphContext`s pointing at them, and asserts that `dispatch_execute`
//! routes by the `graph` argument.

use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::Schema;
use apollo_mcp_server::graphs::{
    GraphContext, Graphs, default_provider, dispatch_execute, dispatch_search,
};
use apollo_mcp_server::introspection::tools::execute::Execute;
use apollo_mcp_server::operations::MutationMode;
use apollo_schema_index::{OperationType, SchemaIndex};
use mockito::Server;
use parking_lot::Mutex;
use reqwest::header::HeaderMap;
use rmcp::model::{JsonObject, RawContent};
use tokio::sync::RwLock;

fn ctx(name: &str, sdl: &str, endpoint: &str) -> GraphContext {
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

fn obj(v: serde_json::Value) -> JsonObject {
    v.as_object().cloned().unwrap()
}

fn extract_text(result: rmcp::model::CallToolResult) -> String {
    use std::ops::Deref;
    result
        .content
        .iter()
        .filter_map(|c| {
            if let RawContent::Text(t) = c.deref() {
                Some(t.text.clone())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn execute_routes_to_the_correct_graph() {
    let mut graph_a = Server::new_async().await;
    let mut graph_b = Server::new_async().await;

    let mock_a = graph_a
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":{"id":"from-a"}}"#)
        .expect(1)
        .create_async()
        .await;

    let mock_b = graph_b
        .mock("POST", "/")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":{"id":"from-b"}}"#)
        .expect(1)
        .create_async()
        .await;

    let mut map = HashMap::new();
    map.insert(
        "a".to_string(),
        ctx(
            "a",
            "type Query { id: String }",
            &format!("{}/", graph_a.url()),
        ),
    );
    map.insert(
        "b".to_string(),
        ctx(
            "b",
            "type Query { id: String }",
            &format!("{}/", graph_b.url()),
        ),
    );
    let graphs: Graphs = Arc::new(RwLock::new(map));

    let execute = Execute::new(MutationMode::None, None);
    let rhai = Arc::new(Mutex::new(apollo_mcp_rhai::RhaiEngine::new("rhai")));

    let result_a = dispatch_execute(
        &graphs,
        &execute,
        Some(&obj(
            serde_json::json!({"graph": "a", "query": "query Q { id }"}),
        )),
        None,
        &rhai,
    )
    .await
    .unwrap();

    let result_b = dispatch_execute(
        &graphs,
        &execute,
        Some(&obj(
            serde_json::json!({"graph": "b", "query": "query Q { id }"}),
        )),
        None,
        &rhai,
    )
    .await
    .unwrap();

    let text_a = extract_text(result_a);
    let text_b = extract_text(result_b);

    assert!(
        text_a.contains("from-a"),
        "expected graph 'a' response, got: {text_a}"
    );
    assert!(
        text_b.contains("from-b"),
        "expected graph 'b' response, got: {text_b}"
    );

    mock_a.assert_async().await;
    mock_b.assert_async().await;
}

#[tokio::test]
async fn execute_unknown_graph_returns_error_with_available_names() {
    let mut map = HashMap::new();
    map.insert(
        "alpha".to_string(),
        ctx("alpha", "type Query { id: String }", "http://127.0.0.1:1/"),
    );
    map.insert(
        "beta".to_string(),
        ctx("beta", "type Query { id: String }", "http://127.0.0.1:1/"),
    );
    let graphs: Graphs = Arc::new(RwLock::new(map));

    let execute = Execute::new(MutationMode::None, None);
    let rhai = Arc::new(Mutex::new(apollo_mcp_rhai::RhaiEngine::new("rhai")));

    let result = dispatch_execute(
        &graphs,
        &execute,
        Some(&obj(
            serde_json::json!({"graph": "missing", "query": "query Q { id }"}),
        )),
        None,
        &rhai,
    )
    .await
    .unwrap();

    let text = extract_text(result);
    assert!(text.contains("Unknown graph 'missing'"));
    assert!(text.contains("alpha"));
    assert!(text.contains("beta"));
}

#[tokio::test]
async fn upstream_401_surfaces_as_upstream_auth_required_with_graph_name() {
    let mut graph = Server::new_async().await;
    let _mock = graph
        .mock("POST", "/")
        .with_status(401)
        .with_header("www-authenticate", "Bearer realm=\"demo\"")
        .expect(1)
        .create_async()
        .await;

    let mut map = HashMap::new();
    map.insert(
        "g".to_string(),
        ctx(
            "g",
            "type Query { id: String }",
            &format!("{}/", graph.url()),
        ),
    );
    let graphs: Graphs = Arc::new(RwLock::new(map));

    let execute = Execute::new(MutationMode::None, None);
    let rhai = Arc::new(Mutex::new(apollo_mcp_rhai::RhaiEngine::new("rhai")));

    let result = dispatch_execute(
        &graphs,
        &execute,
        Some(&obj(
            serde_json::json!({"graph": "g", "query": "query Q { id }"}),
        )),
        None,
        &rhai,
    )
    .await
    .unwrap();

    assert_eq!(result.is_error, Some(true));
    let structured = result
        .structured_content
        .as_ref()
        .expect("expected structured_content for upstream 401");
    let obj = structured.as_object().unwrap();
    assert_eq!(
        obj.get("error").and_then(|v| v.as_str()),
        Some("upstream_auth_required")
    );
    assert_eq!(obj.get("graph").and_then(|v| v.as_str()), Some("g"));
    assert_eq!(
        obj.get("www_authenticate").and_then(|v| v.as_str()),
        Some("Bearer realm=\"demo\"")
    );
}

#[tokio::test]
async fn search_fans_out_across_graphs_and_tags_results() {
    let mut map = HashMap::new();
    map.insert(
        "north".to_string(),
        ctx(
            "north",
            "type Query { polar: String }",
            "http://north.test/",
        ),
    );
    map.insert(
        "south".to_string(),
        ctx(
            "south",
            "type Query { antarctic: String }",
            "http://south.test/",
        ),
    );
    let graphs: Graphs = Arc::new(RwLock::new(map));

    let result = dispatch_search(
        &graphs,
        1,
        false,
        Some(&obj(serde_json::json!({"terms": ["polar", "antarctic"]}))),
    )
    .await
    .unwrap();

    let text = extract_text(result);
    assert!(text.contains("# graph: north"));
    assert!(text.contains("# graph: south"));
}
