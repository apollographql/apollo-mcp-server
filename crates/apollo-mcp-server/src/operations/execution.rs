use http::HeaderMap;
use opentelemetry::Context;
use opentelemetry::trace::FutureExt;
use rmcp::model::{CallToolResult, JsonObject};
use serde_json::Value;
use url::Url;

use crate::errors::McpError;
use crate::graphql::{self, Executable};

use super::Operation;

pub(crate) async fn find_and_execute_operation(
    operations: &[Operation],
    tool_name: &str,
    headers: &HeaderMap,
    arguments: Option<&JsonObject>,
    endpoint: &Url,
) -> Option<Result<CallToolResult, McpError>> {
    let operation = operations.iter().find(|op| op.as_ref().name == tool_name)?;
    Some(execute_operation(operation, headers, arguments, endpoint).await)
}

pub(crate) async fn execute_operation(
    executable: &impl Executable,
    headers: &HeaderMap,
    arguments: Option<&JsonObject>,
    endpoint: &Url,
) -> Result<CallToolResult, McpError> {
    let graphql_request = graphql::Request {
        input: Value::from(arguments.cloned()),
        endpoint,
        headers,
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
            .into_operation(&schema, None, MutationMode::All, true, true)
            .unwrap()
            .unwrap();

        let result = find_and_execute_operation(
            &[operation],
            "NonFound",
            &HeaderMap::new(),
            None,
            &"http://localhost:4000".parse().unwrap(),
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
                .into_operation(&schema, None, MutationMode::All, true, true)
                .unwrap()
                .unwrap(),
            RawOperation::from(("query GetWorld { hello }".to_string(), None))
                .into_operation(&schema, None, MutationMode::All, true, true)
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
        )
        .await;

        mock.assert();
        assert!(result.is_some());
        let call_result = result.unwrap().unwrap();
        assert!(call_result.is_error != Some(true));
    }
}
