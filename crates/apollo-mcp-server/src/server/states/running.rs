use std::collections::HashMap;
use std::sync::Arc;

use apollo_compiler::{Schema, validation::Valid};
use opentelemetry::KeyValue;
use reqwest::header::HeaderMap;
use rmcp::ErrorData;
use rmcp::model::{
    ClientCapabilities, Extensions, Implementation, ListResourcesResult, ReadResourceResult,
    ResourcesCapability, ToolsCapability,
};
use rmcp::{
    Peer, RoleServer, ServerHandler, ServiceError,
    model::{
        CallToolRequestParams, CallToolResult, Content, ErrorCode, InitializeRequestParams,
        InitializeResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
};
use serde_json::Value;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};
use url::Url;

use crate::apps::app::AppTarget;
use crate::apps::resource::{attach_resource_mime_type, get_app_resource};
use crate::apps::tool::{attach_tool_metadata, find_and_execute_app_tool, make_tool_private};
use crate::generated::telemetry::{TelemetryAttribute, TelemetryMetric};
use crate::meter;
use crate::operations::{execute_operation, find_and_execute_operation};
use crate::server::states::telemetry::get_parent_span;
use crate::server_info::ServerInfoConfig;
use crate::{
    custom_scalar_map::CustomScalarMap,
    errors::McpError,
    explorer::{EXPLORER_TOOL_NAME, Explorer},
    headers::{ForwardHeaders, build_request_headers},
    health::HealthCheck,
    introspection::tools::{
        execute::{EXECUTE_TOOL_NAME, Execute},
        introspect::{INTROSPECT_TOOL_NAME, Introspect},
        search::{SEARCH_TOOL_NAME, Search},
        validate::{VALIDATE_TOOL_NAME, Validate},
    },
    operations::{MutationMode, Operation, RawOperation, apply_description_override},
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
    pub(super) enable_output_schema: bool,
    pub(super) disable_auth_token_passthrough: bool,
    pub(super) descriptions: HashMap<String, String>,
    pub(super) health_check: Option<HealthCheck>,
    pub(super) server_info: ServerInfoConfig,
}

impl Running {
    /// Returns true when `enable_output_schema` is active and the negotiated
    /// protocol version supports `outputSchema` / `structuredContent` (MCP 2025-06-18+).
    fn client_supports_output_schema(&self, protocol_version: Option<&ProtocolVersion>) -> bool {
        self.enable_output_schema
            && protocol_version.is_some_and(|v| *v >= ProtocolVersion::V_2025_06_18)
    }

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
                        self.enable_output_schema,
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
                .map(|operation| apply_description_override(operation, &self.descriptions))
                .filter_map(|operation| {
                    operation
                        .into_operation(
                            schema,
                            self.custom_scalar_map.as_ref(),
                            self.mutation_mode,
                            self.disable_type_description,
                            self.disable_schema_description,
                            self.enable_output_schema,
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

    async fn list_tools_impl(
        &self,
        extensions: Extensions,
        client_capabilities: Option<&ClientCapabilities>,
        protocol_version: Option<&ProtocolVersion>,
    ) -> Result<ListToolsResult, McpError> {
        let meter = &meter::METER;
        meter
            .u64_counter(TelemetryMetric::ListToolsCount.as_str())
            .build()
            .add(1, &[]);

        let app_param = extract_app_param(&extensions);
        let app_target = AppTarget::try_from((extensions, client_capabilities))?;

        // If we get the app param, we'll run in a special "app mode" where we only expose the tools for that app (+execute)
        let mut result = if let Some(app_name) = app_param {
            let app = self.apps.iter().find(|app| app.name == app_name);

            match app {
                Some(app) => ListToolsResult {
                    next_cursor: None,
                    tools: self
                        .operations
                        .read()
                        .await
                        .iter()
                        .map(|op| op.as_ref().clone())
                        .chain(
                            self.execute_tool
                                .as_ref()
                                .iter()
                                // When running apps, make the execute tool executable from the app but hidden from the LLM via meta entry on the tool. This prevents the LLM from using the execute tool by limiting it only to the app tools.
                                .map(|e| make_tool_private(e.tool.clone())),
                        )
                        .chain(
                            app.tools
                                .iter()
                                .map(|tool| attach_tool_metadata(app, tool, &app_target))
                                .collect::<Vec<_>>(),
                        )
                        .collect(),
                    meta: None,
                },
                None => {
                    return Err(McpError::new(
                        ErrorCode::INVALID_REQUEST,
                        format!("App {app_name} not found"),
                        None,
                    ));
                }
            }
        } else {
            ListToolsResult {
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
                    .collect(),
                meta: None,
            }
        };

        if !self.client_supports_output_schema(protocol_version) {
            for tool in &mut result.tools {
                tool.output_schema = None;
            }
        }

        Ok(result)
    }

    fn list_resources_impl(&self) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: self
                .apps
                .iter()
                .map(|app| attach_resource_mime_type(app.resource()))
                .collect(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource_impl(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        extensions: Extensions,
        client_capabilities: Option<&ClientCapabilities>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let request_uri = Url::parse(&request.uri).map_err(|err| {
            ErrorData::resource_not_found(
                format!("Requested resource has an invalid URI: {err}"),
                None,
            )
        })?;
        let app_target = AppTarget::try_from((extensions, client_capabilities))?;

        let resource = get_app_resource(&self.apps, request, request_uri, &app_target).await?;

        Ok(ReadResourceResult {
            contents: vec![resource],
        })
    }
}

impl ServerHandler for Running {
    #[tracing::instrument(skip_all, parent = get_parent_span(&context), fields(apollo.mcp.client_name = request.client_info.name, apollo.mcp.client_version = request.client_info.version))]
    async fn initialize(
        &self,
        request: InitializeRequestParams,
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

    #[tracing::instrument(skip_all, parent = get_parent_span(&context), fields(apollo.mcp.tool_name = request.name.as_ref(), apollo.mcp.request_id = %context.id.clone()))]
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let meter = &meter::METER;
        let start = std::time::Instant::now();
        let tool_name = request.name.clone();

        let app_param = extract_app_param(&context.extensions);

        let mut result = if tool_name == INTROSPECT_TOOL_NAME
            && let Some(introspect_tool) = &self.introspect_tool
        {
            match serde_json::from_value(Value::from(request.arguments)) {
                Ok(args) => introspect_tool.execute(args).await,
                Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                    "Invalid input: {e}"
                ))])),
            }
        } else if tool_name == SEARCH_TOOL_NAME
            && let Some(search_tool) = &self.search_tool
        {
            match serde_json::from_value(Value::from(request.arguments)) {
                Ok(args) => search_tool.execute(args).await,
                Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                    "Invalid input: {e}"
                ))])),
            }
        } else if tool_name == EXPLORER_TOOL_NAME
            && let Some(explorer_tool) = &self.explorer_tool
        {
            match serde_json::from_value(Value::from(request.arguments)) {
                Ok(args) => explorer_tool.execute(args).await,
                Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                    "Invalid input: {e}"
                ))])),
            }
        } else if tool_name == EXECUTE_TOOL_NAME
            && let Some(execute_tool) = &self.execute_tool
        {
            let headers =
                if let Some(axum_parts) = context.extensions.get::<axum::http::request::Parts>() {
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

            execute_operation(
                execute_tool,
                &headers,
                request.arguments.as_ref(),
                &self.endpoint,
            )
            .await
        } else if tool_name == VALIDATE_TOOL_NAME
            && let Some(validate_tool) = &self.validate_tool
        {
            match serde_json::from_value(Value::from(request.arguments)) {
                Ok(args) => Ok(validate_tool.execute(args).await),
                Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                    "Invalid input: {e}"
                ))])),
            }
        } else {
            let headers =
                if let Some(axum_parts) = context.extensions.get::<axum::http::request::Parts>() {
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

            if let Some(res) = find_and_execute_operation(
                &self.operations.read().await,
                &tool_name,
                &headers,
                request.arguments.as_ref(),
                &self.endpoint,
            )
            .await
            {
                res
            } else if let Some(app_name) = app_param.as_deref()
                && let Some(res) = find_and_execute_app_tool(
                    &self.apps,
                    app_name,
                    &tool_name,
                    &headers,
                    request.arguments.as_ref(),
                    &self.endpoint,
                )
                .await
            {
                res
            } else {
                Err(tool_not_found(&tool_name))
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

        // MCP Apps rely on structured_content; only strip for non-app calls with older protocol versions.
        let protocol_version = context.peer.peer_info().map(|info| &info.protocol_version);
        if app_param.is_none()
            && !self.client_supports_output_schema(protocol_version)
            && let Ok(r) = &mut result
        {
            r.structured_content = None;
        }

        result
    }

    #[tracing::instrument(skip_all, parent = get_parent_span(&context))]
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let peer_info = context.peer.peer_info();
        let client_capabilities = peer_info.map(|info| &info.capabilities);
        let protocol_version = peer_info.map(|info| &info.protocol_version);

        self.list_tools_impl(context.extensions, client_capabilities, protocol_version)
            .await
    }

    #[tracing::instrument(skip_all)]
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        self.list_resources_impl()
    }

    #[tracing::instrument(skip_all, fields(apollo.mcp.resource_uri = request.uri.as_str(), apollo.mcp.request_id = %context.id.clone()))]
    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let client_capabilities = context.peer.peer_info().map(|info| &info.capabilities);

        self.read_resource_impl(request, context.extensions, client_capabilities)
            .await
    }

    fn get_info(&self) -> ServerInfo {
        let meter = &meter::METER;
        meter
            .u64_counter(TelemetryMetric::GetInfoCount.as_str())
            .build()
            .add(1, &[]);

        let capabilities = ServerCapabilities {
            tools: Some(ToolsCapability {
                list_changed: Some(true),
            }),
            resources: (!self.apps.is_empty()).then(ResourcesCapability::default),
            ..Default::default()
        };

        let protocol_version = if self.enable_output_schema {
            ProtocolVersion::V_2025_06_18
        } else {
            ProtocolVersion::default()
        };

        ServerInfo {
            protocol_version,
            server_info: Implementation {
                name: self.server_info.name().to_string(),
                icons: None,
                title: self.server_info.title().map(|s| s.to_string()),
                version: self.server_info.version().to_string(),
                website_url: self.server_info.website_url().map(|s| s.to_string()),
                description: self.server_info.description().map(|s| s.to_string()),
            },
            capabilities,
            instructions: None,
        }
    }
}

fn extract_app_param(extensions: &Extensions) -> Option<String> {
    extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.uri.query())
        .and_then(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .find(|(key, _)| key == "app")
                .map(|(_, value)| value.into_owned())
        })
}

fn tool_not_found(name: &str) -> McpError {
    McpError::new(
        ErrorCode::METHOD_NOT_FOUND,
        format!("Tool {name} not found"),
        None,
    )
}

#[cfg(test)]
mod tests {
    use rmcp::model::{JsonObject, ReadResourceRequestParams, ResourceContents, Tool};

    use crate::apps::{
        App,
        app::{AppResource, AppResourceSource, AppTool},
        manifest::{AppLabels, CSPSettings, WidgetSettings},
    };

    use super::*;

    fn test_running(schema: Arc<RwLock<Valid<Schema>>>) -> Running {
        Running {
            schema,
            operations: Arc::new(RwLock::new(vec![])),
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
            enable_output_schema: false,
            disable_auth_token_passthrough: false,
            descriptions: HashMap::new(),
            health_check: None,
            server_info: ServerInfoConfig::default(),
        }
    }

    #[tokio::test]
    async fn invalid_operations_should_not_crash_server() {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let operations = Arc::new(RwLock::new(vec![]));

        let running = Running {
            operations: operations.clone(),
            ..test_running(Arc::new(RwLock::new(schema)))
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
    async fn overrides_descriptions_applied_to_operations() {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let operations = Arc::new(RwLock::new(vec![]));

        let descriptions = HashMap::from([(
            "GetId".to_string(),
            "Custom description for GetId".to_string(),
        )]);

        let running = Running {
            operations: operations.clone(),
            descriptions,
            ..test_running(Arc::new(RwLock::new(schema)))
        };

        let new_operations = vec![RawOperation::from((
            "query GetId { id }".to_string(),
            Some("get_id.graphql".to_string()),
        ))];

        running.update_operations(new_operations).await;

        let updated = operations.read().await;
        let tool: &Tool = updated.first().unwrap().as_ref();
        assert_eq!(
            tool.description.as_deref(),
            Some("Custom description for GetId"),
            "Override description should replace auto-generated one"
        );
    }

    #[tokio::test]
    async fn overrides_descriptions_do_not_affect_unmatched_operations() {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let operations = Arc::new(RwLock::new(vec![]));

        let descriptions = HashMap::from([(
            "NonExistent".to_string(),
            "This should not match anything".to_string(),
        )]);

        let running = Running {
            operations: operations.clone(),
            descriptions,
            ..test_running(Arc::new(RwLock::new(schema)))
        };

        let new_operations = vec![RawOperation::from((
            "query GetId { id }".to_string(),
            Some("get_id.graphql".to_string()),
        ))];

        running.update_operations(new_operations).await;

        let updated = operations.read().await;
        let tool: &Tool = updated.first().unwrap().as_ref();
        assert_ne!(
            tool.description.as_deref(),
            Some("This should not match anything"),
            "Unmatched override description should not be applied"
        );
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

        let running = test_running(schema.clone());

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

    const RESOURCE_URI: &str = "http://localhost:4000/resource#1234";

    fn running_with_apps(
        resource: AppResource,
        csp_settings: Option<CSPSettings>,
        widget_settings: Option<WidgetSettings>,
    ) -> Running {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let app = App {
            name: "MyApp".to_string(),
            description: None,
            tools: vec![AppTool {
                operation: Arc::new(
                    RawOperation::from(("query GetId { id }".to_string(), None))
                        .into_operation(&schema, None, MutationMode::All, false, false, true)
                        .unwrap()
                        .unwrap(),
                ),
                labels: AppLabels::default(),
                tool: Tool::new("GetId", "a description", JsonObject::new()),
            }],
            resource,
            uri: RESOURCE_URI.parse().unwrap(),
            prefetch_operations: vec![],
            csp_settings,
            widget_settings,
        };

        Running {
            apps: vec![app],
            ..test_running(Arc::new(RwLock::new(schema)))
        }
    }

    #[tokio::test]
    async fn resource_list_includes_app_resources() {
        let resources = running_with_apps(
            AppResource::Single(AppResourceSource::Local("abcdef".to_string())),
            None,
            None,
        )
        .list_resources_impl()
        .unwrap()
        .resources;

        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].uri, RESOURCE_URI);
    }

    #[tokio::test]
    async fn resource_list_attaches_mcp_apps_mime_type() {
        let resources = running_with_apps(
            AppResource::Single(AppResourceSource::Local("abcdef".to_string())),
            None,
            None,
        )
        .list_resources_impl()
        .unwrap()
        .resources;

        assert_eq!(resources.len(), 1);
        assert_eq!(
            resources[0].mime_type,
            Some("text/html;profile=mcp-app".into())
        );
    }

    #[tokio::test]
    async fn getting_resource_from_running() {
        let resource_content = "This is a test resource";
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local(resource_content.to_string())),
            None,
            None,
        );
        let mut resource = running
            .read_resource_impl(
                ReadResourceRequestParams {
                    uri: "http://localhost:4000/resource#a_different_fragment"
                        .parse()
                        .unwrap(),
                    meta: None,
                },
                Extensions::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(resource.contents.len(), 1);
        let Some(ResourceContents::TextResourceContents {
            uri,
            mime_type,
            text,
            meta,
        }) = resource.contents.pop()
        else {
            panic!("Expected TextResourceContents");
        };
        assert_eq!(text, resource_content);
        assert_eq!(mime_type.unwrap(), "text/html;profile=mcp-app");
        // Meta always contains at least the "ui" key now
        let meta = meta.expect("meta should be set");
        assert!(meta.get("ui").is_some());
        assert_eq!(uri, "http://localhost:4000/resource#a_different_fragment");
    }

    #[tokio::test]
    async fn getting_resource_that_does_not_exist() {
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local("abcdef".to_string())),
            None,
            None,
        );
        let result = running
            .read_resource_impl(
                ReadResourceRequestParams {
                    uri: "http://localhost:4000/invalid_resource".parse().unwrap(),
                    meta: None,
                },
                Extensions::new(),
                None,
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn getting_resource_from_running_with_invalid_uri() {
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local("abcdef".to_string())),
            None,
            None,
        );
        let result = running
            .read_resource_impl(
                ReadResourceRequestParams {
                    uri: "not a uri".parse().unwrap(),
                    meta: None,
                },
                Extensions::new(),
                None,
            )
            .await;
        assert!(result.is_err());
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
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Remote(url)),
            None,
            None,
        );

        let mut resource = running
            .read_resource_impl(
                ReadResourceRequestParams {
                    uri: RESOURCE_URI.to_string(),
                    meta: None,
                },
                Extensions::new(),
                None,
            )
            .await
            .expect("resource fetch failed");

        mock.assert();
        let Some(ResourceContents::TextResourceContents { text, .. }) = resource.contents.pop()
        else {
            panic!("unexpected resource contents");
        };
        assert_eq!(text, body);
    }

    #[tokio::test]
    async fn csp_settings() {
        let resource_content = "This is a test resource";
        let connect_domains = vec!["connect.example.com".to_string()];
        let resource_domains = vec!["resource.example.com".to_string()];
        let frame_domains = vec!["frame.example.com".to_string()];
        let redirect_domains = vec!["redirect.example.com".to_string()];
        let base_uri_domains = vec!["base_uri.example.com".to_string()];
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local(resource_content.to_string())),
            Some(CSPSettings {
                connect_domains: Some(connect_domains.clone()),
                resource_domains: Some(resource_domains.clone()),
                frame_domains: Some(frame_domains.clone()),
                redirect_domains: Some(redirect_domains.clone()),
                base_uri_domains: Some(base_uri_domains.clone()),
            }),
            None,
        );
        let mut resource = running
            .read_resource_impl(
                ReadResourceRequestParams {
                    uri: "http://localhost:4000/resource".parse().unwrap(),
                    meta: None,
                },
                Extensions::new(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(resource.contents.len(), 1);
        let Some(ResourceContents::TextResourceContents { meta, .. }) = resource.contents.pop()
        else {
            panic!("Expected TextResourceContents");
        };
        let meta = meta.expect("meta is not set");
        // OpenAI-specific CSP at root level should only contain redirect_domains
        let openai_csp = meta
            .get("openai/widgetCSP")
            .expect("openai csp settings not found");
        let returned_redirect_domains = openai_csp
            .get("redirect_domains")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(returned_redirect_domains, &redirect_domains);
        // Common CSP properties are under ui.csp with camelCase keys
        let ui_meta = meta.get("ui").expect("ui key not found");
        let csp_settings = ui_meta.get("csp").expect("csp settings not found");
        let returned_connect_domains = csp_settings
            .get("connectDomains")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(returned_connect_domains, &connect_domains);
        let returned_resource_domains = csp_settings
            .get("resourceDomains")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(returned_resource_domains, &resource_domains);
        let returned_frame_domains = csp_settings
            .get("frameDomains")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(returned_frame_domains, &frame_domains);
        let returned_base_uri_domains = csp_settings
            .get("baseUriDomains")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(returned_base_uri_domains, &base_uri_domains);
    }

    #[tokio::test]
    async fn list_tools_without_app_parameter() {
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local("test".to_string())),
            None,
            None,
        );

        let result = running
            .list_tools_impl(Extensions::new(), None, None)
            .await
            .unwrap();

        assert_eq!(result.tools.len(), 0);
        assert_eq!(result.next_cursor, None);
    }

    #[tokio::test]
    async fn list_tools_with_valid_app_parameter() {
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local("test".to_string())),
            None,
            None,
        );

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?app=MyApp")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let result = running
            .list_tools_impl(extensions, None, None)
            .await
            .unwrap();

        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].name, "GetId");
        assert_eq!(result.next_cursor, None);
    }

    #[tokio::test]
    async fn list_tools_with_nonexistent_app_parameter() {
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local("test".to_string())),
            None,
            None,
        );

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?app=NonExistent")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let result = running.list_tools_impl(extensions, None, None).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn list_tools_with_app_and_openai_target_has_correct_metadata() {
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local("test".to_string())),
            None,
            None,
        );

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?app=MyApp&appTarget=openai")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let result = running
            .list_tools_impl(extensions, None, None)
            .await
            .unwrap();
        let meta = result.tools[0].meta.as_ref().unwrap();

        // Should have ui nested metadata with resourceUri and visibility
        let ui = meta.get("ui").unwrap().as_object().unwrap();
        assert_eq!(ui.get("resourceUri").unwrap(), RESOURCE_URI);
        assert_eq!(
            ui.get("visibility").unwrap(),
            &serde_json::json!(["model", "app"])
        );
        // Should have deprecated root-level ui/resourceUri
        assert_eq!(meta.get("ui/resourceUri").unwrap(), RESOURCE_URI);
    }

    #[tokio::test]
    async fn list_tools_with_app_and_mcp_target_has_correct_metadata() {
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local("test".to_string())),
            None,
            None,
        );

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?app=MyApp&appTarget=mcp")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let result = running
            .list_tools_impl(extensions, None, None)
            .await
            .unwrap();
        let meta = result.tools[0].meta.as_ref().unwrap();

        // Check nested ui metadata
        let ui = meta.get("ui").unwrap().as_object().unwrap();
        assert_eq!(ui.get("resourceUri").unwrap(), RESOURCE_URI);
        assert_eq!(
            ui.get("visibility").unwrap(),
            &serde_json::json!(["model", "app"])
        );

        // Check deprecated root-level ui/resourceUri for backwards compatibility
        assert_eq!(meta.get("ui/resourceUri").unwrap(), RESOURCE_URI);

        // Ensure OpenAI-specific keys are NOT present
        assert!(meta.get("openai/outputTemplate").is_none());
        assert!(meta.get("openai/widgetAccessible").is_none());
    }

    #[tokio::test]
    async fn list_tools_with_app_defaults_to_openai_target() {
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local("test".to_string())),
            None,
            None,
        );

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?app=MyApp")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let result = running
            .list_tools_impl(extensions, None, None)
            .await
            .unwrap();
        let meta = result.tools[0].meta.as_ref().unwrap();

        // Default should still have ui nested metadata
        let ui = meta.get("ui").unwrap().as_object().unwrap();
        assert_eq!(ui.get("resourceUri").unwrap(), RESOURCE_URI);
        assert_eq!(
            ui.get("visibility").unwrap(),
            &serde_json::json!(["model", "app"])
        );
        assert_eq!(meta.get("ui/resourceUri").unwrap(), RESOURCE_URI);
    }

    #[tokio::test]
    async fn list_tools_with_app_and_mcp_app_capability_defaults_to_mcp_target() {
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local("test".to_string())),
            None,
            None,
        );

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?app=MyApp")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let mut extension_capabilities = std::collections::BTreeMap::new();
        extension_capabilities.insert(
            "io.modelcontextprotocol/ui".to_string(),
            serde_json::json!({"mimeTypes": ["text/html;profile=mcp-app"]})
                .as_object()
                .unwrap()
                .clone(),
        );
        let client_capabilities = ClientCapabilities {
            extensions: Some(extension_capabilities),
            ..Default::default()
        };

        let result = running
            .list_tools_impl(extensions, Some(&client_capabilities), None)
            .await
            .unwrap();
        let meta = result.tools[0].meta.as_ref().unwrap();

        // Should have MCP-style nested ui metadata
        let ui = meta.get("ui").unwrap().as_object().unwrap();
        assert_eq!(ui.get("resourceUri").unwrap(), RESOURCE_URI);
        assert_eq!(
            ui.get("visibility").unwrap(),
            &serde_json::json!(["model", "app"])
        );

        // Check deprecated root-level ui/resourceUri for backwards compatibility
        assert_eq!(meta.get("ui/resourceUri").unwrap(), RESOURCE_URI);

        // Ensure OpenAI-specific keys are NOT present
        assert!(meta.get("openai/outputTemplate").is_none());
        assert!(meta.get("openai/widgetAccessible").is_none());
    }

    #[tokio::test]
    async fn list_tools_with_invalid_app_target_returns_error() {
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local("test".to_string())),
            None,
            None,
        );

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?app=MyApp&appTarget=invalid")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let result = running.list_tools_impl(extensions, None, None).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn widget_settings_description_is_set_in_meta() {
        let resource_content = "This is a test resource";
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local(resource_content.to_string())),
            None,
            Some(WidgetSettings {
                description: Some("A custom description".to_string()),
                domain: None,
                prefers_border: None,
            }),
        );
        let mut resource = running
            .read_resource_impl(
                ReadResourceRequestParams {
                    uri: "http://localhost:4000/resource".parse().unwrap(),
                    meta: None,
                },
                Extensions::new(),
                None,
            )
            .await
            .unwrap();
        let Some(ResourceContents::TextResourceContents { meta, .. }) = resource.contents.pop()
        else {
            panic!("Expected TextResourceContents");
        };
        let meta = meta.expect("meta should be set");
        let description = meta
            .get("openai/widgetDescription")
            .expect("widgetDescription not found");
        assert_eq!(description.as_str().unwrap(), "A custom description");
    }

    #[tokio::test]
    async fn widget_settings_domain_is_set_in_meta() {
        let resource_content = "This is a test resource";
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local(resource_content.to_string())),
            None,
            Some(WidgetSettings {
                description: None,
                domain: Some("example.com".to_string()),
                prefers_border: None,
            }),
        );
        let mut resource = running
            .read_resource_impl(
                ReadResourceRequestParams {
                    uri: "http://localhost:4000/resource".parse().unwrap(),
                    meta: None,
                },
                Extensions::new(),
                None,
            )
            .await
            .unwrap();
        let Some(ResourceContents::TextResourceContents { meta, .. }) = resource.contents.pop()
        else {
            panic!("Expected TextResourceContents");
        };
        let meta = meta.expect("meta should be set");
        let ui_meta = meta.get("ui").expect("ui key not found");
        let domain = ui_meta.get("domain").expect("domain not found");
        assert_eq!(domain.as_str().unwrap(), "example.com");
    }

    #[tokio::test]
    async fn widget_settings_prefers_border_is_set_in_meta() {
        let resource_content = "This is a test resource";
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local(resource_content.to_string())),
            None,
            Some(WidgetSettings {
                description: None,
                domain: None,
                prefers_border: Some(true),
            }),
        );
        let mut resource = running
            .read_resource_impl(
                ReadResourceRequestParams {
                    uri: "http://localhost:4000/resource".parse().unwrap(),
                    meta: None,
                },
                Extensions::new(),
                None,
            )
            .await
            .unwrap();
        let Some(ResourceContents::TextResourceContents { meta, .. }) = resource.contents.pop()
        else {
            panic!("Expected TextResourceContents");
        };
        let meta = meta.expect("meta should be set");
        let ui_meta = meta.get("ui").expect("ui key not found");
        let prefers_border = ui_meta
            .get("prefersBorder")
            .expect("prefersBorder not found");
        assert!(prefers_border.as_bool().unwrap());
    }

    #[tokio::test]
    async fn read_resource_impl_returns_mcp_format_when_target_is_mcp() {
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local("test content".to_string())),
            Some(CSPSettings {
                connect_domains: Some(vec!["connect.example.com".to_string()]),
                resource_domains: Some(vec!["resource.example.com".to_string()]),
                frame_domains: Some(vec!["frame.example.com".to_string()]),
                redirect_domains: Some(vec!["redirect.example.com".to_string()]),
                base_uri_domains: Some(vec!["base.example.com".to_string()]),
            }),
            Some(WidgetSettings {
                description: Some("Test description".to_string()),
                domain: Some("example.com".to_string()),
                prefers_border: Some(true),
            }),
        );

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?appTarget=mcp")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let mut resource = running
            .read_resource_impl(
                ReadResourceRequestParams {
                    uri: "http://localhost:4000/resource".parse().unwrap(),
                    meta: None,
                },
                extensions,
                None,
            )
            .await
            .unwrap();

        let Some(ResourceContents::TextResourceContents {
            mime_type, meta, ..
        }) = resource.contents.pop()
        else {
            panic!("Expected TextResourceContents");
        };
        assert_eq!(mime_type.unwrap(), "text/html;profile=mcp-app");

        let meta = meta.expect("meta should be set");
        // MCPApps should have ui nesting
        let ui_meta = meta.get("ui").expect("ui key should be set");
        // MCPApps CSP uses camelCase keys and includes baseUriDomains (not redirectDomains)
        let csp = ui_meta.get("csp").expect("CSP should be set");
        assert!(csp.get("connectDomains").is_some());
        assert!(csp.get("resourceDomains").is_some());
        assert!(csp.get("frameDomains").is_some());
        assert!(csp.get("baseUriDomains").is_some());
        assert!(csp.get("redirectDomains").is_none());
        assert!(ui_meta.get("domain").is_some());
        assert!(ui_meta.get("prefersBorder").is_some());
        // MCPApps should not have description
        assert!(ui_meta.get("description").is_none());
    }

    #[tokio::test]
    async fn read_resource_impl_returns_error_for_invalid_app_target() {
        let running = running_with_apps(
            AppResource::Single(AppResourceSource::Local("test content".to_string())),
            None,
            None,
        );

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?appTarget=invalid")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let result = running
            .read_resource_impl(
                ReadResourceRequestParams {
                    uri: "http://localhost:4000/resource".parse().unwrap(),
                    meta: None,
                },
                extensions,
                None,
            )
            .await;

        assert!(result.is_err());
    }

    #[test]
    fn get_info_should_use_default_metadata_when_config_is_empty() {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let running = test_running(Arc::new(RwLock::new(schema)));

        let info = running.get_info();

        assert_eq!(info.server_info.name, "Apollo MCP Server");
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(
            info.server_info.title,
            Some("Apollo MCP Server".to_string())
        );
        assert_eq!(
            info.server_info.website_url,
            Some("https://www.apollographql.com/docs/apollo-mcp-server".to_string())
        );
        assert_eq!(
            info.server_info.description,
            Some(
                "A Model Context Protocol (MCP) server for exposing GraphQL APIs as tools."
                    .to_string()
            )
        );
        assert_eq!(info.server_info.icons, None);
    }

    #[test]
    fn get_info_should_use_custom_metadata_when_config_provided() {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let custom_config = ServerInfoConfig {
            name: Some("My Custom Server".to_string()),
            version: Some("3.0.0-beta".to_string()),
            title: Some("Custom GraphQL Server".to_string()),
            website_url: Some("https://my-server.example.com/docs".to_string()),
            description: Some("A custom MCP server for testing".to_string()),
        };

        let running = Running {
            server_info: custom_config,
            ..test_running(Arc::new(RwLock::new(schema)))
        };

        let info = running.get_info();

        assert_eq!(info.server_info.name, "My Custom Server");
        assert_eq!(info.server_info.version, "3.0.0-beta");
        assert_eq!(
            info.server_info.title,
            Some("Custom GraphQL Server".to_string())
        );
        assert_eq!(
            info.server_info.website_url,
            Some("https://my-server.example.com/docs".to_string())
        );
        assert_eq!(
            info.server_info.description,
            Some("A custom MCP server for testing".to_string())
        );
    }

    mod get_info_protocol_version {
        use super::*;
        use rmcp::model::ProtocolVersion;

        fn build_running(enable_output_schema: bool) -> Running {
            let schema = Schema::parse("type Query { id: String }", "schema.graphql")
                .unwrap()
                .validate()
                .unwrap();

            Running {
                schema: Arc::new(RwLock::new(schema)),
                operations: Arc::new(RwLock::new(vec![])),
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
                enable_output_schema,
                disable_auth_token_passthrough: false,
                descriptions: HashMap::new(),
                health_check: None,
                server_info: ServerInfoConfig::default(),
            }
        }

        #[test]
        fn advertises_default_version_when_output_schema_disabled() {
            let info = build_running(false).get_info();

            assert_eq!(info.protocol_version, ProtocolVersion::default());
        }

        #[test]
        fn advertises_v2025_06_18_when_output_schema_enabled() {
            let info = build_running(true).get_info();

            assert_eq!(info.protocol_version, ProtocolVersion::V_2025_06_18);
        }
    }

    mod output_schema_gating {
        use std::sync::Arc;

        use axum::body::Body;
        use http::{Request, StatusCode};
        use http_body_util::BodyExt;
        use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
        use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
        use serde_json::json;
        use tokio::sync::RwLock;
        use tower::ServiceExt;

        use super::*;
        use crate::operations::RawOperation;

        fn create_running_with_output_schema() -> Running {
            let schema =
                apollo_compiler::Schema::parse_and_validate("type Query { hello: String }", "test")
                    .unwrap();

            let raw_op: RawOperation = ("query Hello { hello }".to_string(), None).into();
            let operation = raw_op
                .into_operation(&schema, None, MutationMode::None, false, false, true)
                .unwrap()
                .expect("operation should be valid");

            Running {
                schema: Arc::new(RwLock::new(schema)),
                operations: Arc::new(RwLock::new(vec![operation])),
                apps: vec![],
                headers: http::HeaderMap::new(),
                forward_headers: vec![],
                endpoint: url::Url::parse("http://localhost:4000").unwrap(),
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
                enable_output_schema: true,
                disable_auth_token_passthrough: false,
                descriptions: HashMap::new(),
                health_check: None,
                server_info: Default::default(),
            }
        }

        fn create_service(
            running: Running,
            session_manager: Arc<LocalSessionManager>,
        ) -> StreamableHttpService<Running, LocalSessionManager> {
            StreamableHttpService::new(
                move || Ok(running.clone()),
                session_manager,
                StreamableHttpServerConfig {
                    stateful_mode: true,
                    ..Default::default()
                },
            )
        }

        fn build_initialize_request(protocol_version: &str) -> Request<Body> {
            let body = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": protocol_version,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "test-client",
                        "version": "1.0.0"
                    }
                }
            });
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .body(Body::from(body.to_string()))
                .unwrap()
        }

        fn build_notification_request(session_id: &str) -> Request<Body> {
            let body = json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            });
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .header("Mcp-Session-Id", session_id)
                .body(Body::from(body.to_string()))
                .unwrap()
        }

        fn build_tools_list_request(session_id: &str) -> Request<Body> {
            let body = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/list"
            });
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .header("Mcp-Session-Id", session_id)
                .body(Body::from(body.to_string()))
                .unwrap()
        }

        fn extract_session_id<B>(response: &http::Response<B>) -> String {
            response
                .headers()
                .get("mcp-session-id")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string()
        }

        async fn extract_json_body<B>(response: http::Response<B>) -> serde_json::Value
        where
            B: BodyExt,
            B::Error: std::fmt::Debug,
        {
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let body_str = String::from_utf8_lossy(&bytes);

            for line in body_str.lines() {
                if let Some(data) = line.strip_prefix("data: ")
                    && let Ok(val) = serde_json::from_str::<serde_json::Value>(data)
                {
                    return val;
                }
            }
            panic!("no JSON data found in SSE response");
        }

        async fn initialize_session(
            running: &Running,
            session_manager: &Arc<LocalSessionManager>,
            protocol_version: &str,
        ) -> String {
            let service = create_service(running.clone(), Arc::clone(session_manager));
            let response = service
                .oneshot(build_initialize_request(protocol_version))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let session_id = extract_session_id(&response);

            let service = create_service(running.clone(), Arc::clone(session_manager));
            let response = service
                .oneshot(build_notification_request(&session_id))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);

            session_id
        }

        async fn list_tools(
            running: Running,
            session_manager: Arc<LocalSessionManager>,
            session_id: &str,
        ) -> Vec<serde_json::Value> {
            let service = create_service(running, session_manager);
            let response = service
                .oneshot(build_tools_list_request(session_id))
                .await
                .unwrap();
            let body = extract_json_body(response).await;
            body["result"]["tools"]
                .as_array()
                .expect("tools/list should return a tools array")
                .clone()
        }

        #[tokio::test]
        async fn excludes_output_schema_when_protocol_predates_it() {
            let running = create_running_with_output_schema();
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();
            let session_id = initialize_session(&running, &session_manager, "2025-03-26").await;

            let tools = list_tools(running, session_manager, &session_id).await;

            assert!(!tools.is_empty());
            for tool in &tools {
                assert!(
                    tool.get("outputSchema").is_none(),
                    "tool '{}' should not have outputSchema with protocol 2025-03-26",
                    tool["name"]
                );
            }
        }

        #[tokio::test]
        async fn includes_output_schema_when_protocol_supports_it() {
            let running = create_running_with_output_schema();
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();
            let session_id = initialize_session(&running, &session_manager, "2025-06-18").await;

            let tools = list_tools(running, session_manager, &session_id).await;

            assert!(!tools.is_empty());
            for tool in &tools {
                assert!(
                    tool.get("outputSchema").is_some(),
                    "tool '{}' should have outputSchema with protocol 2025-06-18",
                    tool["name"]
                );
            }
        }
    }

    mod structured_content_gating {
        use std::sync::Arc;

        use axum::body::Body;
        use http::{Request, StatusCode};
        use http_body_util::BodyExt;
        use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
        use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
        use serde_json::json;
        use tokio::sync::RwLock;
        use tower::ServiceExt;

        use super::*;
        use crate::operations::RawOperation;

        fn create_running_with_mock_endpoint(endpoint: url::Url) -> Running {
            let schema =
                apollo_compiler::Schema::parse_and_validate("type Query { hello: String }", "test")
                    .unwrap();

            let raw_op: RawOperation = ("query Hello { hello }".to_string(), None).into();
            let operation = raw_op
                .into_operation(&schema, None, MutationMode::None, false, false, true)
                .unwrap()
                .expect("operation should be valid");

            Running {
                schema: Arc::new(RwLock::new(schema)),
                operations: Arc::new(RwLock::new(vec![operation])),
                apps: vec![],
                headers: http::HeaderMap::new(),
                forward_headers: vec![],
                endpoint,
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
                enable_output_schema: true,
                disable_auth_token_passthrough: false,
                descriptions: HashMap::new(),
                health_check: None,
                server_info: Default::default(),
            }
        }

        fn create_service(
            running: Running,
            session_manager: Arc<LocalSessionManager>,
        ) -> StreamableHttpService<Running, LocalSessionManager> {
            StreamableHttpService::new(
                move || Ok(running.clone()),
                session_manager,
                StreamableHttpServerConfig {
                    stateful_mode: true,
                    ..Default::default()
                },
            )
        }

        fn build_initialize_request(protocol_version: &str) -> Request<Body> {
            let body = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": protocol_version,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "test-client",
                        "version": "1.0.0"
                    }
                }
            });
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .body(Body::from(body.to_string()))
                .unwrap()
        }

        fn build_notification_request(session_id: &str) -> Request<Body> {
            let body = json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            });
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .header("Mcp-Session-Id", session_id)
                .body(Body::from(body.to_string()))
                .unwrap()
        }

        fn build_call_tool_request(session_id: &str, tool_name: &str) -> Request<Body> {
            let body = json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": tool_name,
                    "arguments": {}
                }
            });
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .header("Mcp-Session-Id", session_id)
                .body(Body::from(body.to_string()))
                .unwrap()
        }

        fn extract_session_id<B>(response: &http::Response<B>) -> String {
            response
                .headers()
                .get("mcp-session-id")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string()
        }

        async fn extract_json_body<B>(response: http::Response<B>) -> serde_json::Value
        where
            B: BodyExt,
            B::Error: std::fmt::Debug,
        {
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let body_str = String::from_utf8_lossy(&bytes);

            for line in body_str.lines() {
                if let Some(data) = line.strip_prefix("data: ")
                    && let Ok(val) = serde_json::from_str::<serde_json::Value>(data)
                {
                    return val;
                }
            }
            panic!("no JSON data found in SSE response");
        }

        async fn initialize_session(
            running: &Running,
            session_manager: &Arc<LocalSessionManager>,
            protocol_version: &str,
        ) -> String {
            let service = create_service(running.clone(), Arc::clone(session_manager));
            let response = service
                .oneshot(build_initialize_request(protocol_version))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let session_id = extract_session_id(&response);

            let service = create_service(running.clone(), Arc::clone(session_manager));
            let response = service
                .oneshot(build_notification_request(&session_id))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);

            session_id
        }

        async fn call_tool(
            running: Running,
            session_manager: Arc<LocalSessionManager>,
            session_id: &str,
            tool_name: &str,
        ) -> serde_json::Value {
            let service = create_service(running, session_manager);
            let response = service
                .oneshot(build_call_tool_request(session_id, tool_name))
                .await
                .unwrap();
            extract_json_body(response).await
        }

        #[tokio::test]
        async fn strips_structured_content_when_protocol_predates_it() {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", "/")
                .with_body(r#"{"data": {"hello": "world"}}"#)
                .create_async()
                .await;

            let running = create_running_with_mock_endpoint(server.url().parse().unwrap());
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();
            let session_id = initialize_session(&running, &session_manager, "2025-03-26").await;

            let body = call_tool(running, session_manager, &session_id, "Hello").await;

            mock.assert();
            let result = &body["result"];
            assert!(
                result.get("structuredContent").is_none() || result["structuredContent"].is_null(),
                "structuredContent should be stripped with protocol 2025-03-26"
            );
        }

        #[tokio::test]
        async fn preserves_structured_content_when_protocol_supports_it() {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", "/")
                .with_body(r#"{"data": {"hello": "world"}}"#)
                .create_async()
                .await;

            let running = create_running_with_mock_endpoint(server.url().parse().unwrap());
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();
            let session_id = initialize_session(&running, &session_manager, "2025-06-18").await;

            let body = call_tool(running, session_manager, &session_id, "Hello").await;

            mock.assert();
            let result = &body["result"];
            assert!(
                result
                    .get("structuredContent")
                    .is_some_and(|v| !v.is_null()),
                "structuredContent should be preserved with protocol 2025-06-18"
            );
        }
    }

    mod sse_resumability {
        use axum::body::Body;
        use http::{Request, StatusCode};
        use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
        use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
        use serde_json::json;
        use std::sync::Arc;
        use tokio::sync::RwLock;
        use tower::ServiceExt;

        use super::*;

        fn create_test_running() -> Running {
            let schema =
                apollo_compiler::Schema::parse_and_validate("type Query { hello: String }", "test")
                    .unwrap();
            Running {
                schema: Arc::new(RwLock::new(schema)),
                operations: Arc::new(RwLock::new(vec![])),
                apps: vec![],
                headers: http::HeaderMap::new(),
                forward_headers: vec![],
                endpoint: url::Url::parse("http://localhost:4000").unwrap(),
                execute_tool: None,
                introspect_tool: None,
                search_tool: None,
                explorer_tool: None,
                validate_tool: None,
                custom_scalar_map: None,
                peers: Arc::new(RwLock::new(vec![])),
                cancellation_token: CancellationToken::new(),
                mutation_mode: MutationMode::All,
                disable_type_description: false,
                disable_schema_description: false,
                enable_output_schema: false,
                disable_auth_token_passthrough: false,
                descriptions: HashMap::new(),
                health_check: None,
                server_info: Default::default(),
            }
        }

        fn create_test_service(
            stateful_mode: bool,
        ) -> StreamableHttpService<Running, LocalSessionManager> {
            let running = create_test_running();
            StreamableHttpService::new(
                move || Ok(running.clone()),
                LocalSessionManager::default().into(),
                StreamableHttpServerConfig {
                    stateful_mode,
                    ..Default::default()
                },
            )
        }

        fn create_stateful_service(
            running: Running,
            session_manager: Arc<LocalSessionManager>,
        ) -> StreamableHttpService<Running, LocalSessionManager> {
            StreamableHttpService::new(
                move || Ok(running.clone()),
                session_manager,
                StreamableHttpServerConfig {
                    stateful_mode: true,
                    ..Default::default()
                },
            )
        }

        fn build_initialize_request() -> Request<Body> {
            let body = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "test-client",
                        "version": "1.0.0"
                    }
                }
            });
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .body(Body::from(body.to_string()))
                .unwrap()
        }

        fn build_get_request(
            session_id: Option<&str>,
            last_event_id: Option<&str>,
        ) -> Request<Body> {
            let mut builder = Request::builder()
                .method("GET")
                .uri("/mcp")
                .header("Accept", "text/event-stream");
            if let Some(id) = session_id {
                builder = builder.header("Mcp-Session-Id", id);
            }
            if let Some(event_id) = last_event_id {
                builder = builder.header("Last-Event-ID", event_id);
            }
            builder.body(Body::empty()).unwrap()
        }

        fn build_delete_request(session_id: &str) -> Request<Body> {
            Request::builder()
                .method("DELETE")
                .uri("/mcp")
                .header("Mcp-Session-Id", session_id)
                .body(Body::empty())
                .unwrap()
        }

        async fn collect_sse_events<B>(response: http::Response<B>) -> Vec<String>
        where
            B: http_body_util::BodyExt,
            B::Error: std::fmt::Debug,
        {
            let body = response.into_body();
            let bytes = body.collect().await.unwrap().to_bytes();
            let body_str = String::from_utf8_lossy(&bytes);

            body_str
                .lines()
                .filter(|line| !line.is_empty())
                .map(|s| s.to_string())
                .collect()
        }

        #[tokio::test]
        async fn initialize_returns_ok() {
            let service = create_test_service(true);
            let response = service.oneshot(build_initialize_request()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn initialize_returns_event_stream() {
            let service = create_test_service(true);
            let response = service.oneshot(build_initialize_request()).await.unwrap();
            let content_type = response.headers().get("content-type").unwrap();
            assert!(content_type.to_str().unwrap().contains("text/event-stream"));
        }

        #[tokio::test]
        async fn priming_event_contains_event_id() {
            let service = create_test_service(true);
            let response = service.oneshot(build_initialize_request()).await.unwrap();
            let events = collect_sse_events(response).await;
            assert!(events.iter().any(|e| e.starts_with("id:")));
        }

        #[tokio::test]
        async fn priming_event_contains_retry_interval() {
            let service = create_test_service(true);
            let response = service.oneshot(build_initialize_request()).await.unwrap();
            let events = collect_sse_events(response).await;
            assert!(events.iter().any(|e| e.starts_with("retry:")));
        }

        #[tokio::test]
        async fn session_id_returned_on_initialize() {
            let service = create_test_service(true);
            let response = service.oneshot(build_initialize_request()).await.unwrap();
            let session_id = response.headers().get("mcp-session-id");
            assert!(!session_id.unwrap().is_empty());
        }

        #[tokio::test]
        async fn get_request_requires_session_id() {
            let service = create_test_service(true);
            let response = service
                .oneshot(build_get_request(None, None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn get_request_with_invalid_session_returns_unauthorized() {
            let service = create_test_service(true);
            let response = service
                .oneshot(build_get_request(Some("non-existent-session"), None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        async fn initialize_and_get_session_id(
            running: Running,
            session_manager: Arc<LocalSessionManager>,
        ) -> String {
            let service = create_stateful_service(running, session_manager);
            let response = service.oneshot(build_initialize_request()).await.unwrap();
            response
                .headers()
                .get("mcp-session-id")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string()
        }

        #[tokio::test]
        async fn reconnect_with_last_event_id_returns_ok() {
            let running = create_test_running();
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();
            let session_id =
                initialize_and_get_session_id(running.clone(), Arc::clone(&session_manager)).await;

            let service = create_stateful_service(running, session_manager);
            let response = service
                .oneshot(build_get_request(Some(&session_id), Some("0")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn reconnect_with_last_event_id_returns_event_stream() {
            let running = create_test_running();
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();
            let session_id =
                initialize_and_get_session_id(running.clone(), Arc::clone(&session_manager)).await;

            let service = create_stateful_service(running, session_manager);
            let response = service
                .oneshot(build_get_request(Some(&session_id), Some("0")))
                .await
                .unwrap();
            let content_type = response.headers().get("content-type").unwrap();
            assert!(content_type.to_str().unwrap().contains("text/event-stream"));
        }

        #[tokio::test]
        async fn standalone_get_stream_returns_ok() {
            let running = create_test_running();
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();
            let session_id =
                initialize_and_get_session_id(running.clone(), Arc::clone(&session_manager)).await;

            let service = create_stateful_service(running, session_manager);
            let response = service
                .oneshot(build_get_request(Some(&session_id), None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        #[tokio::test]
        async fn standalone_get_stream_returns_event_stream() {
            let running = create_test_running();
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();
            let session_id =
                initialize_and_get_session_id(running.clone(), Arc::clone(&session_manager)).await;

            let service = create_stateful_service(running, session_manager);
            let response = service
                .oneshot(build_get_request(Some(&session_id), None))
                .await
                .unwrap();
            let content_type = response.headers().get("content-type").unwrap();
            assert!(content_type.to_str().unwrap().contains("text/event-stream"));
        }

        #[tokio::test]
        async fn delete_request_returns_accepted() {
            let running = create_test_running();
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();
            let session_id =
                initialize_and_get_session_id(running.clone(), Arc::clone(&session_manager)).await;

            let service = create_stateful_service(running, session_manager);
            let response = service
                .oneshot(build_delete_request(&session_id))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
        }

        #[tokio::test]
        async fn deleted_session_rejects_subsequent_requests() {
            let running = create_test_running();
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();
            let session_id =
                initialize_and_get_session_id(running.clone(), Arc::clone(&session_manager)).await;

            let service = create_stateful_service(running.clone(), Arc::clone(&session_manager));
            service
                .oneshot(build_delete_request(&session_id))
                .await
                .unwrap();

            let service2 = create_stateful_service(running, session_manager);
            let response = service2
                .oneshot(build_get_request(Some(&session_id), None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        #[tokio::test]
        async fn stateless_mode_disables_resumability() {
            let service = create_test_service(false);
            let response = service
                .oneshot(build_get_request(None, None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        }
    }
}
