//! Integration test: a running MCP server survives when the collection
//! poller syncs an operation with invalid variables JSON from Explorer.

use std::collections::HashMap;
use std::time::Duration;

use apollo_mcp_registry::{
    platform_api::{PlatformApiConfig, operation_collections::collection_poller::CollectionSource},
    uplink::schema::SchemaSource,
};
use apollo_mcp_server::{
    cors::CorsConfig,
    health::HealthCheckConfig,
    host_validation::HostValidationConfig,
    operations::{MutationMode, OperationSource},
    server::{Server, Transport},
    server_info::ServerInfoConfig,
};
use mockito::Matcher;
use secrecy::SecretString;
use url::Url;

fn initial_collection_response() -> String {
    serde_json::json!({
        "data": {
            "operationCollection": {
                "__typename": "OperationCollection",
                "operations": [{
                    "lastUpdatedAt": "2024-01-01T00:00:00Z",
                    "id": "op-1",
                    "name": "GetUser",
                    "currentOperationRevision": {
                        "body": "query GetUser { user { name } }",
                        "headers": null,
                        "variables": null
                    }
                }]
            }
        }
    })
    .to_string()
}

fn polling_response_with_change() -> String {
    serde_json::json!({
        "data": {
            "operationCollection": {
                "__typename": "OperationCollection",
                "operations": [{
                    "lastUpdatedAt": "2024-01-02T00:00:00Z",
                    "id": "op-1"
                }]
            }
        }
    })
    .to_string()
}

fn entries_response_with_bad_variables() -> String {
    serde_json::json!({
        "data": {
            "operationCollectionEntries": [{
                "id": "op-1",
                "lastUpdatedAt": "2024-01-02T00:00:00Z",
                "name": "GetUser",
                "currentOperationRevision": {
                    "body": "query GetUser { user { name } }",
                    "headers": null,
                    "variables": "not valid json"
                }
            }]
        }
    })
    .to_string()
}

#[tokio::test]
async fn collection_sync_with_bad_variables_keeps_server_alive() {
    let mut mock_server = mockito::Server::new_async().await;
    let mock_url: Url = mock_server.url().parse().unwrap();

    let _initial_mock = mock_server
        .mock("POST", "/")
        .match_body(Matcher::Regex(
            r#""operationName":"OperationCollectionQuery""#.to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(initial_collection_response())
        .create_async()
        .await;

    let _polling_mock = mock_server
        .mock("POST", "/")
        .match_body(Matcher::Regex(
            r#""operationName":"OperationCollectionPollingQuery""#.to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(polling_response_with_change())
        .create_async()
        .await;

    let _entries_mock = mock_server
        .mock("POST", "/")
        .match_body(Matcher::Regex(
            r#""operationName":"OperationCollectionEntriesQuery""#.to_string(),
        ))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(entries_response_with_bad_variables())
        .create_async()
        .await;

    let server = Server::builder()
        .transport(Transport::StreamableHttp {
            auth: None,
            address: "127.0.0.1".parse().unwrap(),
            port: 0,
            stateful_mode: false,
            host_validation: HostValidationConfig::default(),
        })
        .schema_source(SchemaSource::from(
            "type Query { user: User } type User { name: String }",
        ))
        .operation_source(OperationSource::Collection(CollectionSource::Id(
            "test-collection".to_string(),
            PlatformApiConfig::new(
                SecretString::from("test-key"),
                Duration::from_millis(500),
                Duration::from_secs(5),
                Some(mock_url),
            ),
        )))
        .endpoint("http://localhost:4000".parse().unwrap())
        .headers(reqwest::header::HeaderMap::new())
        .forward_headers(vec![])
        .execute_introspection(false)
        .validate_introspection(false)
        .introspect_introspection(false)
        .search_introspection(false)
        .introspect_minify(false)
        .search_minify(false)
        .custom_scalar_map(None)
        .mutation_mode(MutationMode::None)
        .disable_type_description(false)
        .disable_schema_description(false)
        .enable_output_schema(false)
        .disable_auth_token_passthrough(false)
        .descriptions(HashMap::new())
        .search_leaf_depth(5)
        .index_memory_bytes(1024 * 1024)
        .health_check(HealthCheckConfig::default())
        .cors(CorsConfig::default())
        .server_info(ServerInfoConfig::default())
        .build();

    // Wait long enough for at least one poll cycle (500ms), then verify
    // the server is still alive (timeout fires because server.start() never returns).
    let result = tokio::time::timeout(Duration::from_secs(5), server.start()).await;

    // Timeout means the server is still running — it survived the bad sync
    assert!(
        result.is_err(),
        "expected server to keep running (timeout), but it exited: {result:?}"
    );
}
