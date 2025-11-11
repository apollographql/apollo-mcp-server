use std::sync::Arc;

use apollo_compiler::{Schema, validation::Valid};
use opentelemetry::trace::FutureExt;
use opentelemetry::{Context, KeyValue};
use reqwest::header::HeaderMap;
use rmcp::ErrorData;
use rmcp::model::{Implementation, ListResourcesResult, ReadResourceResult, ResourceContents};
use rmcp::{
    Peer, RoleServer, ServerHandler, ServiceError,
    model::{
        CallToolRequestParam, CallToolResult, ErrorCode, InitializeRequestParam, InitializeResult,
        ListToolsResult, PaginatedRequestParam, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
};
use serde_json::Value;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};
use url::Url;

use crate::generated::telemetry::{TelemetryAttribute, TelemetryMetric};
use crate::meter;
use crate::{
    apps::AppResource,
    custom_scalar_map::CustomScalarMap,
    errors::McpError,
    explorer::{EXPLORER_TOOL_NAME, Explorer},
    graphql::{self, Executable as _},
    headers::{ForwardHeaders, build_request_headers},
    health::HealthCheck,
    introspection::tools::{
        execute::{EXECUTE_TOOL_NAME, Execute},
        introspect::{INTROSPECT_TOOL_NAME, Introspect},
        search::{SEARCH_TOOL_NAME, Search},
        validate::{VALIDATE_TOOL_NAME, Validate},
    },
    operations::{MutationMode, Operation, RawOperation},
};

#[derive(Clone)]
pub(super) struct Running {
    pub(super) schema: Arc<RwLock<Valid<Schema>>>,
    pub(super) operations: Arc<RwLock<Vec<Operation>>>,
    pub(super) apps: Vec<crate::apps::App>,
    pub(super) headers: HeaderMap,
    pub(super) forward_headers: ForwardHeaders,
    pub(super) endpoint: Url,
    pub(super) execute_tool: Option<Execute>,
    pub(super) introspect_tool: Option<Introspect>,
    pub(super) search_tool: Option<Search>,
    pub(super) explorer_tool: Option<Explorer>,
    pub(super) validate_tool: Option<Validate>,
    pub(super) custom_scalar_map: Option<CustomScalarMap>,
    pub(super) peers: Arc<RwLock<Vec<Peer<RoleServer>>>>,
    pub(super) cancellation_token: CancellationToken,
    pub(super) mutation_mode: MutationMode,
    pub(super) disable_type_description: bool,
    pub(super) disable_schema_description: bool,
    pub(super) disable_auth_token_passthrough: bool,
    pub(super) health_check: Option<HealthCheck>,
}

impl Running {
    /// Update a running server with a new schema.
    ///
    /// Note: It's important that this takes an immutable reference to ensure we're only updating things that are shared with the server (`RwLock`s)
    pub(super) async fn update_schema(&self, schema: Valid<Schema>) {
        debug!("Schema updated:\n{}", schema);

        // We hold this lock for the entire update process to make sure there are no race conditions with simultaneous updates
        let mut operations_lock = self.operations.write().await;

        // Update the operations based on the new schema. This is necessary because the MCP tool
        // input schemas and description are derived from the schema.
        let operations: Vec<Operation> = operations_lock
            .iter()
            .cloned()
            .map(|operation| operation.into_inner())
            .filter_map(|operation| {
                operation
                    .into_operation(
                        &schema,
                        self.custom_scalar_map.as_ref(),
                        self.mutation_mode,
                        self.disable_type_description,
                        self.disable_schema_description,
                    )
                    .unwrap_or_else(|error| {
                        error!("Invalid operation: {}", error);
                        None
                    })
            })
            .collect();

        debug!(
            "Updated {} operations:\n{}",
            operations.len(),
            serde_json::to_string_pretty(&operations).unwrap_or_default()
        );
        // Update the schema itself
        *self.schema.write().await = schema;

        *operations_lock = operations;

        // Notify MCP clients that tools have changed
        Self::notify_tool_list_changed(self.peers.clone()).await;

        // Now that clients have been notified, drop the lock so they can get the updated operations
        drop(operations_lock);
    }

    /// Update a running server with new operations.
    ///
    /// Note: It's important that this takes an immutable reference to ensure we're only updating things that are shared with the server (`RwLock`s)
    #[tracing::instrument(skip_all)]
    pub(super) async fn update_operations(&self, operations: Vec<RawOperation>) {
        debug!("Operations updated:\n{:?}", operations);

        // We hold this lock for the entire update process to make sure there are no race conditions with simultaneous updates
        let mut operations_lock = self.operations.write().await;

        // Update the operations based on the current schema
        let updated_operations: Vec<Operation> = {
            let schema = &*self.schema.read().await;
            operations
                .into_iter()
                .filter_map(|operation| {
                    operation
                        .into_operation(
                            schema,
                            self.custom_scalar_map.as_ref(),
                            self.mutation_mode,
                            self.disable_type_description,
                            self.disable_schema_description,
                        )
                        .unwrap_or_else(|error| {
                            error!("Invalid operation: {}", error);
                            None
                        })
                })
                .collect()
        };

        debug!(
            "Loaded {} operations:\n{}",
            updated_operations.len(),
            serde_json::to_string_pretty(&updated_operations).unwrap_or_default()
        );
        *operations_lock = updated_operations;

        // Notify MCP clients that tools have changed
        Self::notify_tool_list_changed(self.peers.clone()).await;

        // Now that clients have been notified, drop the lock so they can get the updated operations
        drop(operations_lock);
    }

    /// Notify any peers that tools have changed. Drops unreachable peers from the list.
    #[tracing::instrument(skip_all)]
    async fn notify_tool_list_changed(peers: Arc<RwLock<Vec<Peer<RoleServer>>>>) {
        let mut peers = peers.write().await;
        if !peers.is_empty() {
            debug!(
                "Operations changed, notifying {} peers of tool change",
                peers.len()
            );
        }
        let mut retained_peers = Vec::new();
        for peer in peers.iter() {
            if !peer.is_transport_closed() {
                match peer.notify_tool_list_changed().await {
                    Ok(_) => retained_peers.push(peer.clone()),
                    Err(ServiceError::TransportSend(_) | ServiceError::TransportClosed) => {
                        error!("Failed to notify peer of tool list change - dropping peer",);
                    }
                    Err(e) => {
                        error!("Failed to notify peer of tool list change {:?}", e);
                        retained_peers.push(peer.clone());
                    }
                }
            }
        }
        *peers = retained_peers;
    }
}

impl ServerHandler for Running {
    #[tracing::instrument(skip_all, fields(apollo.mcp.client_name = request.client_info.name, apollo.mcp.client_version = request.client_info.version))]
    async fn initialize(
        &self,
        request: InitializeRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        let meter = &meter::METER;
        let attributes = vec![
            KeyValue::new(
                TelemetryAttribute::ClientName.to_key(),
                request.client_info.name.clone(),
            ),
            KeyValue::new(
                TelemetryAttribute::ClientVersion.to_key(),
                request.client_info.version.clone(),
            ),
        ];
        meter
            .u64_counter(TelemetryMetric::InitializeCount.as_str())
            .build()
            .add(1, &attributes);
        // TODO: how to remove these?
        let mut peers = self.peers.write().await;
        peers.push(context.peer);
        Ok(self.get_info())
    }

    #[tracing::instrument(skip_all, fields(apollo.mcp.tool_name = request.name.as_ref(), apollo.mcp.request_id = %context.id.clone()))]
    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let meter = &meter::METER;
        let start = std::time::Instant::now();
        let tool_name = request.name.clone();
        let result = match tool_name.as_ref() {
            INTROSPECT_TOOL_NAME => {
                self.introspect_tool
                    .as_ref()
                    .ok_or(tool_not_found(&tool_name))?
                    .execute(convert_arguments(request)?)
                    .await
            }
            SEARCH_TOOL_NAME => {
                self.search_tool
                    .as_ref()
                    .ok_or(tool_not_found(&tool_name))?
                    .execute(convert_arguments(request)?)
                    .await
            }
            EXPLORER_TOOL_NAME => {
                self.explorer_tool
                    .as_ref()
                    .ok_or(tool_not_found(&tool_name))?
                    .execute(convert_arguments(request)?)
                    .await
            }
            EXECUTE_TOOL_NAME => {
                let headers = if let Some(axum_parts) =
                    context.extensions.get::<axum::http::request::Parts>()
                {
                    build_request_headers(
                        &self.headers,
                        &self.forward_headers,
                        &axum_parts.headers,
                        &axum_parts.extensions,
                        self.disable_auth_token_passthrough,
                    )
                } else {
                    self.headers.clone()
                };

                self.execute_tool
                    .as_ref()
                    .ok_or(tool_not_found(&tool_name))?
                    .execute(graphql::Request {
                        input: Value::from(request.arguments.clone()),
                        endpoint: &self.endpoint,
                        headers,
                    })
                    .await
            }
            VALIDATE_TOOL_NAME => {
                self.validate_tool
                    .as_ref()
                    .ok_or(tool_not_found(&tool_name))?
                    .execute(convert_arguments(request)?)
                    .await
            }
            _ => {
                let headers = if let Some(axum_parts) =
                    context.extensions.get::<axum::http::request::Parts>()
                {
                    build_request_headers(
                        &self.headers,
                        &self.forward_headers,
                        &axum_parts.headers,
                        &axum_parts.extensions,
                        self.disable_auth_token_passthrough,
                    )
                } else {
                    self.headers.clone()
                };

                let graphql_request = graphql::Request {
                    input: Value::from(request.arguments.clone()),
                    endpoint: &self.endpoint,
                    headers,
                };
                self.operations
                    .read()
                    .await
                    .iter()
                    .chain(self.apps.iter().map(|app| &app.operation))
                    .find(|op| op.as_ref().name == tool_name)
                    .ok_or(tool_not_found(&tool_name))?
                    .execute(graphql_request)
                    .with_context(Context::current())
                    .await
            }
        };

        // Track errors for health check
        if let (Err(_), Some(health_check)) = (&result, &self.health_check) {
            health_check.record_rejection();
        }

        let attributes = vec![
            KeyValue::new(
                TelemetryAttribute::Success.to_key(),
                result.as_ref().is_ok_and(|r| r.is_error != Some(true)),
            ),
            KeyValue::new(TelemetryAttribute::ToolName.to_key(), tool_name),
        ];
        // Record response time and status
        meter
            .f64_histogram(TelemetryMetric::ToolDuration.as_str())
            .build()
            .record(start.elapsed().as_millis() as f64, &attributes);
        meter
            .u64_counter(TelemetryMetric::ToolCount.as_str())
            .build()
            .add(1, &attributes);

        result
    }

    #[tracing::instrument(skip_all)]
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let meter = &meter::METER;
        meter
            .u64_counter(TelemetryMetric::ListToolsCount.as_str())
            .build()
            .add(1, &[]);

        Ok(ListToolsResult {
            next_cursor: None,
            tools: self
                .operations
                .read()
                .await
                .iter()
                .map(|op| op.as_ref().clone())
                .chain(self.execute_tool.as_ref().iter().map(|e| e.tool.clone()))
                .chain(self.introspect_tool.as_ref().iter().map(|e| e.tool.clone()))
                .chain(self.search_tool.as_ref().iter().map(|e| e.tool.clone()))
                .chain(self.explorer_tool.as_ref().iter().map(|e| e.tool.clone()))
                .chain(self.validate_tool.as_ref().iter().map(|e| e.tool.clone()))
                .chain(self.apps.iter().map(|app| app.operation.as_ref().clone()))
                .collect(),
        })
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult {
            resources: self.apps.iter().map(|app| app.resource()).collect(),
            next_cursor: None,
        })
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let request_uri = Url::parse(&request.uri).map_err(|err| {
            ErrorData::resource_not_found(
                format!("Requested resource has an invalid URI: {err}"),
                None,
            )
        })?;
        let Some(app) = self
            .apps
            .iter()
            .find(|app| app.uri.path() == request_uri.path())
        else {
            return Err(ErrorData::resource_not_found(
                format!("Resource not found for URI: {}", request.uri),
                None,
            ));
        };
        let text = match &app.resource {
            AppResource::Local(contents) => contents.clone(),
            AppResource::Remote(url) => fetch_remote_resource(url).await?,
        };

        Ok(ReadResourceResult {
            contents: vec![ResourceContents::TextResourceContents {
                uri: request.uri,
                mime_type: Some("text/html+skybridge".to_string()),
                text,
                meta: None,
            }],
        })
    }

    fn get_info(&self) -> ServerInfo {
        let meter = &meter::METER;
        meter
            .u64_counter(TelemetryMetric::GetInfoCount.as_str())
            .build()
            .add(1, &[]);
        ServerInfo {
            server_info: Implementation {
                name: "Apollo MCP Server".to_string(),
                icons: None,
                title: Some("Apollo MCP Server".to_string()),
                version: env!("CARGO_PKG_VERSION").to_string(),
                website_url: Some(
                    "https://www.apollographql.com/docs/apollo-mcp-server".to_string(),
                ),
            },
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .enable_resources()
                .build(),
            ..Default::default()
        }
    }
}

fn tool_not_found(name: &str) -> McpError {
    McpError::new(
        ErrorCode::METHOD_NOT_FOUND,
        format!("Tool {name} not found"),
        None,
    )
}

fn convert_arguments<T: serde::de::DeserializeOwned>(
    arguments: CallToolRequestParam,
) -> Result<T, McpError> {
    serde_json::from_value(Value::from(arguments.arguments))
        .map_err(|_| McpError::new(ErrorCode::INVALID_PARAMS, "Invalid input".to_string(), None))
}

async fn fetch_remote_resource(url: &Url) -> Result<String, ErrorData> {
    let response = reqwest::Client::new()
        .get(url.clone())
        .send()
        .await
        .map_err(|err| {
            ErrorData::resource_not_found(
                format!("Failed to fetch resource from {}: {err}", url),
                None,
            )
        })?;

    if !response.status().is_success() {
        return Err(ErrorData::resource_not_found(
            format!(
                "Failed to fetch resource from {}: received status {}",
                url,
                response.status()
            ),
            None,
        ));
    }

    response.text().await.map_err(|err| {
        ErrorData::resource_not_found(
            format!("Failed to read resource body from {}: {err}", url),
            None,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalid_operations_should_not_crash_server() {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let operations = Arc::new(RwLock::new(vec![]));

        let running = Running {
            schema: Arc::new(RwLock::new(schema)),
            operations: operations.clone(),
            #[cfg(feature = "apps")]
            apps: vec![],
            headers: HeaderMap::new(),
            forward_headers: vec![],
            endpoint: "http://localhost:4000".parse().unwrap(),
            execute_tool: None,
            introspect_tool: None,
            search_tool: None,
            explorer_tool: None,
            validate_tool: None,
            custom_scalar_map: None,
            peers: Arc::new(RwLock::new(vec![])),
            cancellation_token: CancellationToken::new(),
            mutation_mode: MutationMode::None,
            disable_type_description: false,
            disable_schema_description: false,
            disable_auth_token_passthrough: false,
            health_check: None,
        };

        let new_operations = vec![
            RawOperation::from((
                "query Valid { id }".to_string(),
                Some("valid.graphql".to_string()),
            )),
            RawOperation::from((
                "query Invalid {{ id }".to_string(),
                Some("invalid.graphql".to_string()),
            )),
            RawOperation::from((
                "query { id }".to_string(),
                Some("unnamed.graphql".to_string()),
            )),
        ];

        running.update_operations(new_operations.clone()).await;

        // Check that our local copy of operations is updated, representing what the server sees
        let updated_operations = operations.read().await;

        assert_eq!(updated_operations.len(), 1);
        assert_eq!(updated_operations.first().unwrap().as_ref().name, "Valid");
    }

    #[tokio::test]
    async fn changing_schema_invalidates_outdated_operations() {
        let schema = Arc::new(RwLock::new(
            Schema::parse(
                "type Query { data: String, something: String }",
                "schema.graphql",
            )
            .unwrap()
            .validate()
            .unwrap(),
        ));

        let running = Running {
            schema: schema.clone(),
            operations: Arc::new(RwLock::new(vec![])),
            #[cfg(feature = "apps")]
            apps: vec![],
            headers: HeaderMap::new(),
            forward_headers: vec![],
            endpoint: "http://localhost:4000".parse().unwrap(),
            execute_tool: None,
            introspect_tool: None,
            search_tool: None,
            explorer_tool: None,
            validate_tool: None,
            custom_scalar_map: None,
            peers: Arc::new(RwLock::new(vec![])),
            cancellation_token: CancellationToken::new(),
            mutation_mode: MutationMode::None,
            disable_type_description: false,
            disable_schema_description: false,
            disable_auth_token_passthrough: false,
            health_check: None,
        };

        let operations = vec![
            RawOperation::from((
                "query Valid { data }".to_string(),
                Some("valid.graphql".to_string()),
            )),
            RawOperation::from((
                "query WillBeStale { something }".to_string(),
                Some("invalid.graphql".to_string()),
            )),
        ];

        running.update_operations(operations).await;

        let new_schema = Schema::parse("type Query { data: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();
        running.update_schema(new_schema.clone()).await;

        assert_eq!(*schema.read().await, new_schema);
    }

    #[tokio::test]
    async fn fetch_remote_resource_downloads_content() {
        let mut server = mockito::Server::new_async().await;
        let body = "<html>remote</html>";
        let mock = server
            .mock("GET", "/widget")
            .with_status(200)
            .with_body(body)
            .expect(1)
            .create_async()
            .await;

        let url = Url::parse(&format!("{}/widget", server.url())).unwrap();
        let contents = fetch_remote_resource(&url)
            .await
            .expect("resource fetch failed");

        mock.assert();
        assert_eq!(contents, body);
    }
}
