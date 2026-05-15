//! Execute GraphQL operations from an MCP tool

use std::sync::LazyLock;

use crate::errors::McpError;
use crate::generated::telemetry::{TelemetryAttribute, TelemetryMetric};
use crate::meter;
use crate::operations::private_fields::{PrivateFieldTree, filter_private_fields};
use opentelemetry::KeyValue;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware, Extension};
use reqwest_tracing::{OtelName, TracingMiddleware};
use rmcp::model::{CallToolResult, Content, Meta};
use serde_json::{Map, Value};
use url::Url;

#[derive(Debug)]
pub struct Request<'a> {
    pub input: Value,
    pub endpoint: &'a Url,
    pub headers: &'a HeaderMap,
}

#[derive(Debug, PartialEq)]
pub struct OperationDetails {
    pub query: String,
    pub operation_name: Option<String>,
    /// When present, the query has `@private` fields. The query text should already
    /// be stripped of `@private` directives, and this tree is used to filter the response.
    pub private_fields: Option<PrivateFieldTree>,
}

static GRAPHQL_CLIENT: LazyLock<ClientWithMiddleware> = LazyLock::new(|| {
    // reqwest-middleware 0.5+ uses reqwest 0.13 with rustls-no-provider, so we must install
    // the ring crypto provider before creating the client.
    let _ = rustls::crypto::ring::default_provider().install_default();
    ClientBuilder::new(reqwest_middleware::reqwest::Client::new())
        .with_init(Extension(OtelName("mcp-graphql-client".into())))
        .with(TracingMiddleware::default())
        .build()
});

#[derive(Debug, PartialEq)]
pub struct ValidationError(pub String);

/// Able to be executed as a GraphQL operation
pub trait Executable {
    /// Get the operation to execute and its name
    fn operation(&self, input: Value) -> Result<OperationDetails, ValidationError>;

    /// Get the variables to execute the operation with
    fn variables(&self, input: Value) -> Result<Value, ValidationError>;

    /// Get the headers to execute the operation with
    fn headers(&self, default_headers: &HeaderMap<HeaderValue>) -> HeaderMap<HeaderValue>;

    /// Execute as a GraphQL operation using the endpoint and headers
    #[tracing::instrument(skip(self, request), fields(apollo.mcp.graphql_query = tracing::field::Empty, apollo.mcp.graphql_response = tracing::field::Empty))]
    async fn execute(&self, request: Request<'_>) -> Result<CallToolResult, McpError> {
        let meter = &meter::METER;
        let start = std::time::Instant::now();
        let client_metadata = serde_json::json!({
            "name": "mcp",
            "version": std::env!("CARGO_PKG_VERSION")
        });

        let variables = match self.variables(request.input.clone()) {
            Ok(v) => v,
            Err(ValidationError(msg)) => {
                return Ok(CallToolResult::error(vec![Content::text(msg)]));
            }
        };

        let mut request_body = Map::from_iter([(String::from("variables"), variables)]);

        let OperationDetails {
            query,
            operation_name,
            private_fields,
        } = match self.operation(request.input) {
            Ok(details) => details,
            Err(ValidationError(msg)) => {
                return Ok(CallToolResult::error(vec![Content::text(msg)]));
            }
        };

        tracing::Span::current().record("apollo.mcp.graphql_query", query.as_str());
        request_body.insert(String::from("query"), Value::String(query));
        request_body.insert(
            String::from("extensions"),
            serde_json::json!({
                "clientLibrary": client_metadata,
            }),
        );

        let op_id = operation_name.clone();
        if let Some(op_name) = operation_name {
            request_body.insert(String::from("operationName"), Value::String(op_name));
        }

        let response = match GRAPHQL_CLIENT
            .post(request.endpoint.as_str())
            .headers(self.headers(request.headers))
            .body(Value::Object(request_body).to_string())
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Failed to send GraphQL request: {e}"
                ))]));
            }
        };

        let result = match response.json::<Value>().await {
            Ok(json) => {
                let is_error = Some(
                    json.get("errors")
                        .filter(|value| !matches!(value, Value::Null))
                        .is_some(),
                );

                // When the operation has @private fields, split the response:
                // - restricted (without @private fields) goes to structured_content
                // - full response is preserved in meta for the client to access
                let (structured_content, meta) = if let Some(tree) = private_fields.as_ref() {
                    let restricted = filter_private_fields(&json, tree);
                    let mut meta = Meta::new();
                    meta.insert("structuredContent".into(), json);
                    (restricted, Some(meta))
                } else {
                    (json, None)
                };

                // Record the filtered view so @private fields never appear in spans.
                if let Ok(s) = serde_json::to_string(&structured_content) {
                    tracing::Span::current().record("apollo.mcp.graphql_response", s.as_str());
                }

                let result = if is_error == Some(true) {
                    CallToolResult::structured_error(structured_content)
                } else {
                    CallToolResult::structured(structured_content)
                };
                Ok(result.with_meta(meta))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to read GraphQL response body: {e}"
            ))])),
        };

        // Record response metrics
        let attributes = vec![
            KeyValue::new(
                TelemetryAttribute::Success.to_key(),
                result.as_ref().is_ok_and(|r| r.is_error != Some(true)),
            ),
            KeyValue::new(
                TelemetryAttribute::OperationId.to_key(),
                op_id.unwrap_or_default(),
            ),
            KeyValue::new(TelemetryAttribute::OperationSource.to_key(), "operation"),
        ];
        meter
            .f64_histogram(TelemetryMetric::OperationDuration.as_str())
            .build()
            .record(start.elapsed().as_millis() as f64, &attributes);
        meter
            .u64_counter(TelemetryMetric::OperationCount.as_str())
            .build()
            .add(1, &attributes);

        result
    }
}

#[cfg(test)]
mod test {
    use crate::generated::telemetry::TelemetryMetric;
    use crate::graphql::{Executable, OperationDetails, Request, ValidationError};
    use crate::operations::private_fields::process_private_directives;
    use http::{HeaderMap, HeaderValue};
    use opentelemetry::global;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{
        InMemoryMetricExporter, MeterProviderBuilder, PeriodicReader,
    };
    use rmcp::model::RawContent;
    use serde_json::{Map, Value, json};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tracing::Subscriber;
    use tracing::field::{Field, Visit};
    use tracing::span::{Id, Record};
    use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
    use tracing_subscriber::registry::LookupSpan;
    use url::Url;

    type CapturedFields = Arc<Mutex<HashMap<String, String>>>;

    struct CaptureVisitor<'a>(&'a Mutex<HashMap<String, String>>);
    impl Visit for CaptureVisitor<'_> {
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0
                .lock()
                .unwrap()
                .insert(field.name().to_string(), value.to_string());
        }
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0
                .lock()
                .unwrap()
                .insert(field.name().to_string(), format!("{value:?}"));
        }
    }

    struct CaptureLayer(CapturedFields);
    impl<S> Layer<S> for CaptureLayer
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_record(&self, _id: &Id, values: &Record<'_>, _ctx: Context<'_, S>) {
            values.record(&mut CaptureVisitor(&self.0));
        }
    }

    struct TestExecutable;

    impl Executable for TestExecutable {
        fn operation(&self, _input: Value) -> Result<OperationDetails, ValidationError> {
            Ok(OperationDetails {
                query: "query MockOp { mockOp { id } }".to_string(),
                operation_name: Some("mock_operation".to_string()),
                private_fields: None,
            })
        }

        fn variables(&self, _input: Value) -> Result<Value, ValidationError> {
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

    #[tokio::test]
    async fn calls_graphql_endpoint_with_expected_body_without_pq_extensions() {
        // given
        let mut server = mockito::Server::new_async().await;
        let url = Url::parse(server.url().as_str()).unwrap();
        let mock_request = Request {
            input: json!({}),
            endpoint: &url,
            headers: &HeaderMap::new(),
        };
        let expected_request_body = json!({
            "variables": { "arg1": "foobar" },
            "query": "query MockOp { mockOp { id } }",
            "extensions": {
                "clientLibrary": {
                    "name":"mcp",
                    "version": std::env!("CARGO_PKG_VERSION")
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
        let test_executable = TestExecutable {};
        let result = test_executable.execute(mock_request).await.unwrap();

        // then
        mock.assert(); // verify that the mock http server route was invoked
        assert!(!result.content.is_empty());
        assert!(!result.is_error.unwrap());
    }

    #[tokio::test]
    async fn returns_tool_error_when_gql_server_cannot_be_reached() {
        // given
        let url = Url::parse("http://localhost/no-server").unwrap();
        let mock_request = Request {
            input: json!({}),
            endpoint: &url,
            headers: &HeaderMap::new(),
        };

        // when
        let test_executable = TestExecutable {};
        let result = test_executable.execute(mock_request).await.unwrap();

        // then
        assert_eq!(result.is_error, Some(true));
        let content = &result.content[0];
        if let RawContent::Text(text) = &content.raw {
            assert!(text.text.starts_with("Failed to send GraphQL request"));
        } else {
            panic!("Expected text content");
        }
    }

    #[tokio::test]
    async fn returns_tool_error_when_json_body_cannot_be_parsed() {
        // given
        let mut server = mockito::Server::new_async().await;
        let url = Url::parse(server.url().as_str()).unwrap();
        let mock_request = Request {
            input: json!({}),
            endpoint: &url,
            headers: &HeaderMap::new(),
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
        let test_executable = TestExecutable {};
        let result = test_executable.execute(mock_request).await.unwrap();

        // then
        assert_eq!(result.is_error, Some(true));
        let content = &result.content[0];
        if let RawContent::Text(text) = &content.raw {
            assert!(
                text.text
                    .starts_with("Failed to read GraphQL response body")
            );
        } else {
            panic!("Expected text content");
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
            headers: &HeaderMap::new(),
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
        let test_executable = TestExecutable {};
        let result = test_executable.execute(mock_request).await.unwrap();

        // then
        assert!(result.is_error.is_some());
        assert!(result.is_error.unwrap());
    }

    #[tokio::test]
    async fn gql_response_with_errors_and_partial_data_is_flagged_as_error() {
        // given
        let mut server = mockito::Server::new_async().await;
        let url = Url::parse(server.url().as_str()).unwrap();
        let mock_request = Request {
            input: json!({}),
            endpoint: &url,
            headers: &HeaderMap::new(),
        };

        // Partial success: resolver failed but `data` is a non-null object.
        // This is the common shape for runtime GraphQL errors (e.g. constraint violations).
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                json!({ "data": { "createUsers": null }, "errors": ["a resolver error"] })
                    .to_string(),
            )
            .expect(1)
            .create_async()
            .await;

        // when
        let test_executable = TestExecutable {};
        let result = test_executable.execute(mock_request).await.unwrap();

        // then
        assert!(result.is_error == Some(true));
    }

    #[tokio::test]
    async fn span_does_not_record_private_fields_in_graphql_response() {
        use crate::operations::private_fields::PrivateFieldTree;

        struct PrivateExecutable(PrivateFieldTree);
        impl Executable for PrivateExecutable {
            fn operation(&self, _input: Value) -> Result<OperationDetails, ValidationError> {
                Ok(OperationDetails {
                    query: "query Q { secret }".to_string(),
                    operation_name: Some("Q".to_string()),
                    private_fields: Some(self.0.clone()),
                })
            }
            fn variables(&self, _input: Value) -> Result<Value, ValidationError> {
                Ok(json!({}))
            }
            fn headers(&self, _: &HeaderMap<HeaderValue>) -> HeaderMap<HeaderValue> {
                HeaderMap::new()
            }
        }

        let (_stripped, tree) = process_private_directives("query Q { secret @private }")
            .expect("query should have @private fields");

        let mut server = mockito::Server::new_async().await;
        let url = Url::parse(server.url().as_str()).unwrap();
        server
            .mock("POST", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(json!({ "data": { "secret": "supersecret-pii" } }).to_string())
            .create_async()
            .await;

        let captured: CapturedFields = Arc::new(Mutex::new(HashMap::new()));
        let subscriber = tracing_subscriber::registry().with(CaptureLayer(captured.clone()));
        let _guard = tracing::subscriber::set_default(subscriber);

        PrivateExecutable(tree)
            .execute(Request {
                input: json!({}),
                endpoint: &url,
                headers: &HeaderMap::new(),
            })
            .await
            .unwrap();

        let fields = captured.lock().unwrap();
        let response = fields
            .get("apollo.mcp.graphql_response")
            .expect("apollo.mcp.graphql_response should be recorded");
        assert!(
            !response.contains("supersecret-pii"),
            "span leaked @private value: {response}"
        );
    }

    #[tokio::test]
    async fn validate_metric_attributes_success_false() {
        // given
        let exporter = InMemoryMetricExporter::default();
        let meter_provider = MeterProviderBuilder::default()
            .with_reader(PeriodicReader::builder(exporter.clone()).build())
            .build();
        global::set_meter_provider(meter_provider.clone());

        let mut server = mockito::Server::new_async().await;
        let url = Url::parse(server.url().as_str()).unwrap();
        let mock_request = Request {
            input: json!({}),
            endpoint: &url,
            headers: &HeaderMap::new(),
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
        let test_executable = TestExecutable {};
        let result = test_executable.execute(mock_request).await.unwrap();

        // then
        assert!(result.is_error.is_some());
        assert!(result.is_error.unwrap());

        // Retrieve the finished metrics from the exporter
        let finished_metrics = exporter.get_finished_metrics().unwrap();

        // validate the attributes of the apollo.mcp.operation.count counter
        for resource_metrics in finished_metrics {
            if let Some(scope_metrics) = resource_metrics
                .scope_metrics()
                .find(|scope_metrics| scope_metrics.scope().name() == "apollo.mcp")
            {
                for metric in scope_metrics.metrics() {
                    if metric.name() == TelemetryMetric::OperationCount.as_str()
                        && let AggregatedMetrics::U64(MetricData::Sum(data)) = metric.data()
                    {
                        for point in data.data_points() {
                            let attributes = point.attributes();
                            let mut attr_map = std::collections::HashMap::new();
                            for kv in attributes {
                                attr_map.insert(kv.key.as_str(), kv.value.as_str());
                            }
                            assert_eq!(
                                attr_map.get("operation.id").map(|s| s.as_ref()),
                                Some("mock_operation")
                            );
                            assert_eq!(
                                attr_map.get("operation.type").map(|s| s.as_ref()),
                                Some("operation")
                            );
                            assert_eq!(
                                attr_map.get("success"),
                                Some(&std::borrow::Cow::Borrowed("false"))
                            );
                        }
                    }
                }
            }
        }
    }
}
