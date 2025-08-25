//! Execute GraphQL operations from an MCP tool

use crate::errors::McpError;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest_middleware::{ClientBuilder, Extension};
use reqwest_tracing::{OtelName, TracingMiddleware};
use rmcp::model::{CallToolResult, Content, ErrorCode};
use serde_json::{Map, Value};
use url::Url;

#[derive(Debug)]
pub struct Request<'a> {
    pub input: Value,
    pub endpoint: &'a Url,
    pub headers: HeaderMap,
}

#[derive(Debug, PartialEq)]
pub struct OperationDetails {
    pub query: String,
    pub operation_name: Option<String>,
}

/// Able to be executed as a GraphQL operation
pub trait Executable {
    /// Get the persisted query ID to be executed, if any
    fn persisted_query_id(&self) -> Option<String>;

    /// Get the operation to execute and its name
    fn operation(&self, input: Value) -> Result<OperationDetails, McpError>;

    /// Get the variables to execute the operation with
    fn variables(&self, input: Value) -> Result<Value, McpError>;

    /// Get the headers to execute the operation with
    fn headers(&self, default_headers: &HeaderMap<HeaderValue>) -> HeaderMap<HeaderValue>;

    /// Execute as a GraphQL operation using the endpoint and headers
    #[tracing::instrument(skip(self))]
    async fn execute(&self, request: Request<'_>) -> Result<CallToolResult, McpError> {
        let client_metadata = serde_json::json!({
            "name": "mcp",
            "version": std::env!("CARGO_PKG_VERSION")
        });

        let mut request_body = Map::from_iter([(
            String::from("variables"),
            self.variables(request.input.clone())?,
        )]);

        if let Some(id) = self.persisted_query_id() {
            request_body.insert(
                String::from("extensions"),
                serde_json::json!({
                    "persistedQuery": {
                        "version": 1,
                        "sha256Hash": id,
                    },
                    "clientLibrary": client_metadata,
                }),
            );
        } else {
            let OperationDetails {
                query,
                operation_name,
            } = self.operation(request.input)?;

            request_body.insert(String::from("query"), Value::String(query));
            request_body.insert(
                String::from("extensions"),
                serde_json::json!({
                    "clientLibrary": client_metadata,
                }),
            );

            if let Some(op_name) = operation_name {
                request_body.insert(String::from("operationName"), Value::String(op_name));
            }
        }

        let client = ClientBuilder::new(reqwest::Client::new())
            .with_init(Extension(OtelName("mcp-graphql-client".into())))
            .with(TracingMiddleware::default())
            .build();

        client
            .post(request.endpoint.as_str())
            .headers(self.headers(&request.headers))
            .body(Value::Object(request_body).to_string())
            .send()
            .await
            .map_err(|reqwest_error| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to send GraphQL request: {reqwest_error}"),
                    None,
                )
            })?
            .json::<Value>()
            .await
            .map_err(|reqwest_error| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to read GraphQL response body: {reqwest_error}"),
                    None,
                )
            })
            .map(|json| CallToolResult {
                content: vec![Content::json(&json).unwrap_or(Content::text(json.to_string()))],
                is_error: Some(
                    json.get("errors")
                        .filter(|value| !matches!(value, Value::Null))
                        .is_some()
                        && json
                            .get("data")
                            .filter(|value| !matches!(value, Value::Null))
                            .is_none(),
                ),
            })
    }
}

#[cfg(test)]
mod test {
    use crate::errors::McpError;
    use crate::graphql::{Executable, OperationDetails, Request};
    use http::{HeaderMap, HeaderValue};
    use serde_json::{Map, Value, json};
    use url::Url;

    struct TestExecutableWithoutPersistedQueryId;

    impl Executable for TestExecutableWithoutPersistedQueryId {
        fn persisted_query_id(&self) -> Option<String> {
            None
        }

        fn operation(&self, _input: Value) -> Result<OperationDetails, McpError> {
            Ok(OperationDetails {
                query: "query MockOp { mockOp { id } }".to_string(),
                operation_name: Some("mock_operation".to_string()),
            })
        }

        fn variables(&self, _input: Value) -> Result<Value, McpError> {
            let json = r#"{ "arg1": "foobar" }"#;
            let parsed_json = serde_json::from_str(json).expect("Failed to parse json");
            let json_map: Map<String, Value> = match parsed_json {
                Value::Object(map) => map,
                _ => panic!("Expected a JSON object, but received a different type"),
            };
            Ok(Value::from(json_map))
        }

        fn headers(&self, _default_headers: &HeaderMap<HeaderValue>) -> HeaderMap<HeaderValue> {
            HeaderMap::new()
        }
    }

    struct TestExecutableWithPersistedQueryId;

    impl Executable for TestExecutableWithPersistedQueryId {
        fn persisted_query_id(&self) -> Option<String> {
            Some("4f059505-fe13-4043-819a-461dd82dd5ed".to_string())
        }

        fn operation(&self, _input: Value) -> Result<OperationDetails, McpError> {
            Ok(OperationDetails {
                query: "query MockOp { mockOp { id } }".to_string(),
                operation_name: Some("mock_operation".to_string()),
            })
        }

        fn variables(&self, _input: Value) -> Result<Value, McpError> {
            Ok(Value::String("mock_variables".to_string()))
        }

        fn headers(&self, _default_headers: &HeaderMap<HeaderValue>) -> HeaderMap<HeaderValue> {
            HeaderMap::new()
        }
    }

    #[tokio::test]
    async fn calls_graphql_endpoint_with_expected_body_without_pq_extensions() {
        // given
        let mut server = mockito::Server::new_async().await;
        let url = Url::parse(server.url().as_str()).unwrap();
        let mock_request = Request {
            input: json!({}),
            endpoint: &url,
            headers: HeaderMap::new(),
        };
        let expected_request_body = json!({
            "variables": { "arg1": "foobar" },
            "query": "query MockOp { mockOp { id } }",
            "extensions": {
                "clientLibrary": {
                    "name":"mcp",
                    "version":"0.7.1"
                }
            },
            "operationName":"mock_operation"
        })
        .to_string();

        let mock = server
            .mock("POST", "/")
            .match_body(expected_request_body.as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "data": {}  }).to_string())
            .expect(1)
            .create_async()
            .await;

        // when
        let test_executable = TestExecutableWithoutPersistedQueryId {};
        let result = test_executable.execute(mock_request).await.unwrap();

        // then
        mock.assert(); // verify that the mock http server route was invoked
        assert!(!result.content.is_empty());
        assert!(!result.is_error.unwrap());
    }

    #[tokio::test]
    async fn calls_graphql_endpoint_with_expected_pq_extensions_in_request_body() {
        // given
        let mut server = mockito::Server::new_async().await;
        let url = Url::parse(server.url().as_str()).unwrap();
        let mock_request = Request {
            input: json!({}),
            endpoint: &url,
            headers: HeaderMap::new(),
        };
        let expected_request_body = json!({
            "variables": "mock_variables",
            "extensions": {
                "persistedQuery": {
                    "version": 1,
                    "sha256Hash": "4f059505-fe13-4043-819a-461dd82dd5ed",
                },
                "clientLibrary": {
                    "name":"mcp",
                    "version":"0.7.1"
                }
            },
        })
        .to_string();

        let mock = server
            .mock("POST", "/")
            .match_body(expected_request_body.as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "data": {},  }).to_string())
            .expect(1)
            .create_async()
            .await;

        // when
        let test_executable = TestExecutableWithPersistedQueryId {};
        let result = test_executable.execute(mock_request).await.unwrap();

        // then
        mock.assert(); // verify that the mock http server route was invoked
        assert!(!result.content.is_empty());
        assert!(!result.is_error.unwrap());
    }

    #[tokio::test]
    async fn results_in_mcp_error_when_gql_server_cannot_be_reached() {
        // given
        let url = Url::parse("http://localhost/no-server").unwrap();
        let mock_request = Request {
            input: json!({}),
            endpoint: &url,
            headers: HeaderMap::new(),
        };

        // when
        let test_executable = TestExecutableWithPersistedQueryId {};
        let result = test_executable.execute(mock_request).await;

        // then
        match result {
            Err(e) => {
                assert!(
                    e.message
                        .to_string()
                        .starts_with("Failed to send GraphQL request")
                );
            }
            _ => {
                panic!("Expected MCP error");
            }
        }
    }

    #[tokio::test]
    async fn results_in_mcp_error_when_json_body_cannot_be_parsed() {
        // given
        let mut server = mockito::Server::new_async().await;
        let url = Url::parse(server.url().as_str()).unwrap();
        let mock_request = Request {
            input: json!({}),
            endpoint: &url,
            headers: HeaderMap::new(),
        };

        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{ \"invalid_json\": 'foo' }")
            .expect(1)
            .create_async()
            .await;

        // when
        let test_executable = TestExecutableWithPersistedQueryId {};
        let result = test_executable.execute(mock_request).await;

        // then
        match result {
            Err(e) => {
                assert!(
                    e.message
                        .to_string()
                        .starts_with("Failed to read GraphQL response body")
                );
            }
            _ => {
                panic!("Expected MCP error");
            }
        }
    }

    #[tokio::test]
    async fn gql_response_error_are_found_in_call_tool_result() {
        // given
        let mut server = mockito::Server::new_async().await;
        let url = Url::parse(server.url().as_str()).unwrap();
        let mock_request = Request {
            input: json!({}),
            endpoint: &url,
            headers: HeaderMap::new(),
        };

        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "data": null, "errors": ["an error"] }).to_string())
            .expect(1)
            .create_async()
            .await;

        // when
        let test_executable = TestExecutableWithPersistedQueryId {};
        let result = test_executable.execute(mock_request).await.unwrap();

        // then
        assert!(result.is_error.is_some());
        assert!(result.is_error.unwrap());
    }
}
