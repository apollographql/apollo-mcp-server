use std::sync::Arc;

use http::HeaderMap;
use http::request::Parts;
use opentelemetry::Context;
use opentelemetry::trace::FutureExt;
use parking_lot::Mutex;
use rmcp::model::{CallToolResult, JsonObject};
use serde_json::Value;
use url::Url;

use crate::errors::McpError;
use crate::graphql::{self, Executable};
use apollo_mcp_rhai::{RhaiEngine, checkpoints};

use super::Operation;

pub(crate) async fn find_and_execute_operation(
    operations: &[Operation],
    tool_name: &str,
    headers: &HeaderMap,
    arguments: Option<&JsonObject>,
    endpoint: &Url,
    rhai_engine: &Arc<Mutex<RhaiEngine>>,
    axum_parts: Option<&Parts>,
) -> Option<Result<CallToolResult, McpError>> {
    let operation = operations.iter().find(|op| op.as_ref().name == tool_name)?;
    Some(
        execute_operation(
            operation,
            headers,
            arguments,
            endpoint,
            rhai_engine,
            axum_parts,
            tool_name,
        )
        .await,
    )
}

pub(crate) async fn execute_operation(
    executable: &impl Executable,
    headers: &HeaderMap,
    arguments: Option<&JsonObject>,
    endpoint: &Url,
    rhai_engine: &Arc<Mutex<RhaiEngine>>,
    axum_parts: Option<&Parts>,
    tool_name: &str,
) -> Result<CallToolResult, McpError> {
    let (endpoint, headers) = checkpoints::on_execute_graphql_operation(
        rhai_engine,
        endpoint,
        headers,
        axum_parts,
        tool_name,
    )?;

    let graphql_request = graphql::Request {
        input: Value::from(arguments.cloned()),
        endpoint: &endpoint,
        headers: &headers,
    };

    executable
        .execute(graphql_request)
        .with_context(Context::current())
        .await
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;

    use crate::operations::{MutationMode, RawOperation};

    use super::*;

    #[tokio::test]
    async fn returns_none_when_operation_not_found() {
        let schema = Schema::parse("type Query { hello: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();
        let operation = RawOperation::from(("query GetHello { hello }".to_string(), None))
            .into_operation(&schema, None, MutationMode::All, true, true, true)
            .unwrap()
            .unwrap();

        let result = find_and_execute_operation(
            &[operation],
            "NonFound",
            &HeaderMap::new(),
            None,
            &"http://localhost:4000".parse().unwrap(),
            &Arc::new(parking_lot::Mutex::new(RhaiEngine::new())),
            None,
        )
        .await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn executes_only_the_matching_operation() {
        let schema = Schema::parse("type Query { hello: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();
        let operations = [
            RawOperation::from(("query GetHello { hello }".to_string(), None))
                .into_operation(&schema, None, MutationMode::All, true, true, true)
                .unwrap()
                .unwrap(),
            RawOperation::from(("query GetWorld { hello }".to_string(), None))
                .into_operation(&schema, None, MutationMode::All, true, true, true)
                .unwrap()
                .unwrap(),
        ];

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_body(r#"{"data": {"hello": "world"}}"#)
            .expect(1) // Fails if called 0 or 2+ times
            .create_async()
            .await;

        let result = find_and_execute_operation(
            &operations,
            "GetHello",
            &HeaderMap::new(),
            None,
            &server.url().parse().unwrap(),
            &Arc::new(parking_lot::Mutex::new(RhaiEngine::new())),
            None,
        )
        .await;

        mock.assert();
        assert!(result.is_some());
        let call_result = result.unwrap().unwrap();
        assert!(call_result.is_error != Some(true));
    }
}
