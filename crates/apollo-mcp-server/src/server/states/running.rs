use std::ops::Deref as _;
use std::sync::Arc;

use apollo_compiler::{Schema, validation::Valid};
use headers::HeaderMapExt as _;
use opentelemetry::Context;
use opentelemetry::trace::FutureExt;
use reqwest::header::HeaderMap;
use rmcp::model::Implementation;
use rmcp::{
    Peer, RoleServer, ServerHandler, ServiceError,
    model::{
        CallToolRequestParam, CallToolResult, ErrorCode, InitializeRequestParam, InitializeResult,
        ListToolsResult, PaginatedRequestParam, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
};
use serde_json::Value;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};
use url::Url;

use crate::{
    auth::ValidToken,
    custom_scalar_map::CustomScalarMap,
    errors::{McpError, ServerError},
    explorer::{EXPLORER_TOOL_NAME, Explorer},
    graphql::{self, Executable as _},
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
    pub(super) schema: Arc<Mutex<Valid<Schema>>>,
    pub(super) operations: Arc<Mutex<Vec<Operation>>>,
    pub(super) headers: HeaderMap,
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
    pub(super) async fn update_schema(self, schema: Valid<Schema>) -> Result<Running, ServerError> {
        debug!("Schema updated:\n{}", schema);

        // Update the operations based on the new schema. This is necessary because the MCP tool
        // input schemas and description are derived from the schema.
        let operations: Vec<Operation> = self
            .operations
            .lock()
            .await
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
            serde_json::to_string_pretty(&operations)?
        );
        *self.operations.lock().await = operations;

        // Update the schema itself
        *self.schema.lock().await = schema;

        // Notify MCP clients that tools have changed
        Self::notify_tool_list_changed(self.peers.clone()).await;
        Ok(self)
    }

    pub(super) async fn update_operations(
        self,
        operations: Vec<RawOperation>,
    ) -> Result<Running, ServerError> {
        debug!("Operations updated:\n{:?}", operations);

        // Update the operations based on the current schema
        {
            let schema = &*self.schema.lock().await;
            let updated_operations: Vec<Operation> = operations
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
                .collect();

            debug!(
                "Loaded {} operations:\n{}",
                updated_operations.len(),
                serde_json::to_string_pretty(&updated_operations)?
            );
            *self.operations.lock().await = updated_operations;
        }

        // Notify MCP clients that tools have changed
        Self::notify_tool_list_changed(self.peers.clone()).await;
        Ok(self)
    }

    /// Notify any peers that tools have changed. Drops unreachable peers from the list.
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
    #[tracing::instrument(skip(self))]
    async fn initialize(
        &self,
        _request: InitializeRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        // TODO: how to remove these?
        let mut peers = self.peers.write().await;
        peers.push(context.peer);
        Ok(self.get_info())
    }

    #[tracing::instrument(skip(self, context), fields(tool_name = request.name.as_ref(), request_id = %context.id.clone()))]
    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let result = match request.name.as_ref() {
            INTROSPECT_TOOL_NAME => {
                self.introspect_tool
                    .as_ref()
                    .ok_or(tool_not_found(&request.name))?
                    .execute(convert_arguments(request)?)
                    .await
            }
            SEARCH_TOOL_NAME => {
                self.search_tool
                    .as_ref()
                    .ok_or(tool_not_found(&request.name))?
                    .execute(convert_arguments(request)?)
                    .await
            }
            EXPLORER_TOOL_NAME => {
                self.explorer_tool
                    .as_ref()
                    .ok_or(tool_not_found(&request.name))?
                    .execute(convert_arguments(request)?)
                    .await
            }
            EXECUTE_TOOL_NAME => {
                let mut headers = self.headers.clone();
                if let Some(axum_parts) = context.extensions.get::<axum::http::request::Parts>() {
                    // Optionally extract the validated token and propagate it to upstream servers if present
                    if !self.disable_auth_token_passthrough
                        && let Some(token) = axum_parts.extensions.get::<ValidToken>()
                    {
                        headers.typed_insert(token.deref().clone());
                    }

                    // Forward the mcp-session-id header if present
                    if let Some(session_id) = axum_parts.headers.get("mcp-session-id") {
                        headers.insert("mcp-session-id", session_id.clone());
                    }
                }

                self.execute_tool
                    .as_ref()
                    .ok_or(tool_not_found(&request.name))?
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
                    .ok_or(tool_not_found(&request.name))?
                    .execute(convert_arguments(request)?)
                    .await
            }
            _ => {
                let mut headers = self.headers.clone();
                if let Some(axum_parts) = context.extensions.get::<axum::http::request::Parts>() {
                    // Optionally extract the validated token and propagate it to upstream servers if present
                    if !self.disable_auth_token_passthrough
                        && let Some(token) = axum_parts.extensions.get::<ValidToken>()
                    {
                        headers.typed_insert(token.deref().clone());
                    }

                    // Also forward the mcp-session-id header if present
                    if let Some(session_id) = axum_parts.headers.get("mcp-session-id") {
                        headers.insert("mcp-session-id", session_id.clone());
                    }
                }

                let graphql_request = graphql::Request {
                    input: Value::from(request.arguments.clone()),
                    endpoint: &self.endpoint,
                    headers,
                };
                self.operations
                    .lock()
                    .await
                    .iter()
                    .find(|op| op.as_ref().name == request.name)
                    .ok_or(tool_not_found(&request.name))?
                    .execute(graphql_request)
                    .with_context(Context::current())
                    .await
            }
        };

        // Track errors for health check
        if let (Err(_), Some(health_check)) = (&result, &self.health_check) {
            health_check.record_rejection();
        }

        result
    }

    #[tracing::instrument(skip_all)]
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            next_cursor: None,
            tools: self
                .operations
                .lock()
                .await
                .iter()
                .map(|op| op.as_ref().clone())
                .chain(self.execute_tool.as_ref().iter().map(|e| e.tool.clone()))
                .chain(self.introspect_tool.as_ref().iter().map(|e| e.tool.clone()))
                .chain(self.search_tool.as_ref().iter().map(|e| e.tool.clone()))
                .chain(self.explorer_tool.as_ref().iter().map(|e| e.tool.clone()))
                .chain(self.validate_tool.as_ref().iter().map(|e| e.tool.clone()))
                .collect(),
        })
    }

    fn get_info(&self) -> ServerInfo {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalid_operations_should_not_crash_server() {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let running = Running {
            schema: Arc::new(Mutex::new(schema)),
            operations: Arc::new(Mutex::new(vec![])),
            headers: HeaderMap::new(),
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

        let updated_running = running.update_operations(operations).await.unwrap();
        let updated_operations = updated_running.operations.lock().await;

        assert_eq!(updated_operations.len(), 1);
        assert_eq!(updated_operations.first().unwrap().as_ref().name, "Valid");
    }
}
