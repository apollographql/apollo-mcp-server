use std::sync::Arc;

use apollo_compiler::{Schema, validation::Valid};
use opentelemetry::KeyValue;
use reqwest::header::HeaderMap;
use rmcp::ErrorData;
use rmcp::model::{
    Extensions, Implementation, ListResourcesResult, ReadResourceResult, ResourcesCapability,
    ToolsCapability,
};
use rmcp::{
    Peer, RoleServer, ServerHandler, ServiceError,
    model::{
        CallToolRequestParam, CallToolResult, Content, ErrorCode, InitializeRequestParam,
        InitializeResult, ListToolsResult, PaginatedRequestParam, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
};
use serde_json::Value;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};
use url::Url;

use crate::apps::{
    attach_resource_mime_type, attach_tool_metadata, find_and_execute_app, get_app_resource,
    get_app_target, make_tool_private,
};
use crate::generated::telemetry::{TelemetryAttribute, TelemetryMetric};
use crate::meter;
use crate::operations::{execute_operation, find_and_execute_operation};
use crate::server::states::telemetry::get_parent_span;
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
    pub(super) enable_output_schema: bool,
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

    async fn list_tools_impl(&self, extensions: Extensions) -> Result<ListToolsResult, McpError> {
        let meter = &meter::METER;
        meter
            .u64_counter(TelemetryMetric::ListToolsCount.as_str())
            .build()
            .add(1, &[]);

        // Access the "app" query parameter from the HTTP request
        let app_param = extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.uri.query())
            .and_then(|query| {
                url::form_urlencoded::parse(query.as_bytes())
                    .find(|(key, _)| key == "app")
                    .map(|(_, value)| value.into_owned())
            });

        // If we get the app param, we'll run in a special "app mode" where we only expose the tools for that app (+execute)
        if let Some(app_name) = app_param {
            let app = self.apps.iter().find(|app| app.name == app_name);

            match app {
                Some(app) => {
                    return Ok(ListToolsResult {
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
                                    .map(|tool| attach_tool_metadata(app, tool))
                                    .collect::<Vec<_>>(),
                            )
                            .collect(),
                        meta: None,
                    });
                }
                None => {
                    return Err(McpError::new(
                        ErrorCode::INVALID_REQUEST,
                        format!("App {app_name} not found"),
                        None,
                    ));
                }
            }
        }

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
                .collect(),
            meta: None,
        })
    }

    fn list_resources_impl(&self, extensions: Extensions) -> Result<ListResourcesResult, McpError> {
        let app_target = get_app_target(extensions)?;

        Ok(ListResourcesResult {
            resources: self
                .apps
                .iter()
                .map(|app| attach_resource_mime_type(app.resource(), &app_target))
                .collect(),
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource_impl(
        &self,
        request: rmcp::model::ReadResourceRequestParam,
        extensions: Extensions,
    ) -> Result<ReadResourceResult, ErrorData> {
        let request_uri = Url::parse(&request.uri).map_err(|err| {
            ErrorData::resource_not_found(
                format!("Requested resource has an invalid URI: {err}"),
                None,
            )
        })?;
        let app_target = get_app_target(extensions)?;

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

    #[tracing::instrument(skip_all, parent = get_parent_span(&context), fields(apollo.mcp.tool_name = request.name.as_ref(), apollo.mcp.request_id = %context.id.clone()))]
    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let meter = &meter::METER;
        let start = std::time::Instant::now();
        let tool_name = request.name.clone();
        let result = if tool_name == INTROSPECT_TOOL_NAME
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

            // Access the "app" query parameter from the HTTP request
            let app_param = context
                .extensions
                .get::<axum::http::request::Parts>()
                .and_then(|parts| parts.uri.query())
                .and_then(|query| {
                    url::form_urlencoded::parse(query.as_bytes())
                        .find(|(key, _)| key == "app")
                        .map(|(_, value)| value.into_owned())
                });

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
            } else if let Some(app_name) = app_param
                && let Some(res) = find_and_execute_app(
                    &self.apps,
                    &app_name,
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

        result
    }

    #[tracing::instrument(skip_all, parent = get_parent_span(&context))]
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.list_tools_impl(context.extensions).await
    }

    #[tracing::instrument(skip_all)]
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        self.list_resources_impl(context.extensions)
    }

    #[tracing::instrument(skip_all, fields(apollo.mcp.resource_uri = request.uri.as_str(), apollo.mcp.request_id = %context.id.clone()))]
    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        self.read_resource_impl(request, context.extensions).await
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
            capabilities,
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

#[cfg(test)]
mod tests {
    use rmcp::model::{JsonObject, ReadResourceRequestParam, ResourceContents, Tool};

    use crate::apps::{App, AppLabels, AppResource, AppTool, CSPSettings, WidgetSettings};

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
            schema: Arc::new(RwLock::new(schema)),
            operations: Arc::new(RwLock::new(vec![])),
            apps: vec![app],
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
            health_check: None,
        }
    }

    #[tokio::test]
    async fn resource_list_includes_app_resources() {
        let resources = running_with_apps(AppResource::Local("abcdef".to_string()), None, None)
            .list_resources_impl(Extensions::new())
            .unwrap()
            .resources;

        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].uri, RESOURCE_URI);
    }

    #[tokio::test]
    async fn resource_list_attaches_openai_mime_type_when_target_not_specified() {
        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?app=MyApp")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let resources = running_with_apps(AppResource::Local("abcdef".to_string()), None, None)
            .list_resources_impl(extensions)
            .unwrap()
            .resources;

        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].mime_type, Some("text/html+skybridge".into()));
    }

    #[tokio::test]
    async fn resource_list_attaches_openai_mime_type_when_target_is_openai() {
        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?app=MyApp&appTarget=openai")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let resources = running_with_apps(AppResource::Local("abcdef".to_string()), None, None)
            .list_resources_impl(extensions)
            .unwrap()
            .resources;

        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].mime_type, Some("text/html+skybridge".into()));
    }

    #[tokio::test]
    async fn resource_list_attaches_mcp_apps_mime_type_when_target_is_mcp() {
        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?app=MyApp&appTarget=mcp")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let resources = running_with_apps(AppResource::Local("abcdef".to_string()), None, None)
            .list_resources_impl(extensions)
            .unwrap()
            .resources;

        assert_eq!(resources.len(), 1);
        assert_eq!(
            resources[0].mime_type,
            Some("text/html;profile=mcp-app".into())
        );
    }

    #[tokio::test]
    async fn resource_list_errors_when_target_is_unrecognized() {
        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?app=MyApp&appTarget=lol")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let result = running_with_apps(AppResource::Local("abcdef".to_string()), None, None)
            .list_resources_impl(extensions);

        assert!(result.is_err());
        assert_eq!(
            result.err().unwrap().message,
            "App target lol not recognized. Valid values are 'openai' or 'mcp'."
        )
    }

    #[tokio::test]
    async fn getting_resource_from_running() {
        let resource_content = "This is a test resource";
        let running =
            running_with_apps(AppResource::Local(resource_content.to_string()), None, None);
        let mut resource = running
            .read_resource_impl(
                ReadResourceRequestParam {
                    uri: "http://localhost:4000/resource#a_different_fragment"
                        .parse()
                        .unwrap(),
                },
                Extensions::new(),
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
        assert_eq!(mime_type.unwrap(), "text/html+skybridge");
        assert_eq!(meta, None);
        assert_eq!(uri, "http://localhost:4000/resource#a_different_fragment");
    }

    #[tokio::test]
    async fn getting_resource_that_does_not_exist() {
        let running = running_with_apps(AppResource::Local("abcdef".to_string()), None, None);
        let result = running
            .read_resource_impl(
                ReadResourceRequestParam {
                    uri: "http://localhost:4000/invalid_resource".parse().unwrap(),
                },
                Extensions::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn getting_resource_from_running_with_invalid_uri() {
        let running = running_with_apps(AppResource::Local("abcdef".to_string()), None, None);
        let result = running
            .read_resource_impl(
                ReadResourceRequestParam {
                    uri: "not a uri".parse().unwrap(),
                },
                Extensions::new(),
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
        let running = running_with_apps(AppResource::Remote(url), None, None);

        let mut resource = running
            .read_resource_impl(
                ReadResourceRequestParam {
                    uri: RESOURCE_URI.to_string(),
                },
                Extensions::new(),
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
            AppResource::Local(resource_content.to_string()),
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
                ReadResourceRequestParam {
                    uri: "http://localhost:4000/resource".parse().unwrap(),
                },
                Extensions::new(),
            )
            .await
            .unwrap();
        assert_eq!(resource.contents.len(), 1);
        let Some(ResourceContents::TextResourceContents { meta, .. }) = resource.contents.pop()
        else {
            panic!("Expected TextResourceContents");
        };
        let meta = meta.expect("meta is not set");
        let csp_settings = meta
            .get("openai/widgetCSP")
            .expect("csp settings not found");
        let returned_resource_domains = csp_settings
            .get("resource_domains")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(returned_resource_domains, &resource_domains);
        let returned_connect_domains = csp_settings
            .get("connect_domains")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(returned_connect_domains, &connect_domains);
        let returned_frame_domains = csp_settings
            .get("frame_domains")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(returned_frame_domains, &frame_domains);
        let returned_redirect_domains = csp_settings
            .get("redirect_domains")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(returned_redirect_domains, &redirect_domains);
    }

    #[tokio::test]
    async fn list_tools_without_app_parameter() {
        let running = running_with_apps(AppResource::Local("test".to_string()), None, None);

        let result = running.list_tools_impl(Extensions::new()).await.unwrap();

        assert_eq!(result.tools.len(), 0);
        assert_eq!(result.next_cursor, None);
    }

    #[tokio::test]
    async fn list_tools_with_valid_app_parameter() {
        let running = running_with_apps(AppResource::Local("test".to_string()), None, None);

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?app=MyApp")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let result = running.list_tools_impl(extensions).await.unwrap();

        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].name, "GetId");
        assert_eq!(result.next_cursor, None);
    }

    #[tokio::test]
    async fn list_tools_with_nonexistent_app_parameter() {
        let running = running_with_apps(AppResource::Local("test".to_string()), None, None);

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?app=NonExistent")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let result = running.list_tools_impl(extensions).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn widget_settings_description_is_set_in_meta() {
        let resource_content = "This is a test resource";
        let running = running_with_apps(
            AppResource::Local(resource_content.to_string()),
            None,
            Some(WidgetSettings {
                description: Some("A custom description".to_string()),
                domain: None,
                prefers_border: None,
            }),
        );
        let mut resource = running
            .read_resource_impl(
                ReadResourceRequestParam {
                    uri: "http://localhost:4000/resource".parse().unwrap(),
                },
                Extensions::new(),
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
            AppResource::Local(resource_content.to_string()),
            None,
            Some(WidgetSettings {
                description: None,
                domain: Some("example.com".to_string()),
                prefers_border: None,
            }),
        );
        let mut resource = running
            .read_resource_impl(
                ReadResourceRequestParam {
                    uri: "http://localhost:4000/resource".parse().unwrap(),
                },
                Extensions::new(),
            )
            .await
            .unwrap();
        let Some(ResourceContents::TextResourceContents { meta, .. }) = resource.contents.pop()
        else {
            panic!("Expected TextResourceContents");
        };
        let meta = meta.expect("meta should be set");
        let domain = meta
            .get("openai/widgetDomain")
            .expect("widgetDomain not found");
        assert_eq!(domain.as_str().unwrap(), "example.com");
    }

    #[tokio::test]
    async fn widget_settings_prefers_border_is_set_in_meta() {
        let resource_content = "This is a test resource";
        let running = running_with_apps(
            AppResource::Local(resource_content.to_string()),
            None,
            Some(WidgetSettings {
                description: None,
                domain: None,
                prefers_border: Some(true),
            }),
        );
        let mut resource = running
            .read_resource_impl(
                ReadResourceRequestParam {
                    uri: "http://localhost:4000/resource".parse().unwrap(),
                },
                Extensions::new(),
            )
            .await
            .unwrap();
        let Some(ResourceContents::TextResourceContents { meta, .. }) = resource.contents.pop()
        else {
            panic!("Expected TextResourceContents");
        };
        let meta = meta.expect("meta should be set");
        let prefers_border = meta
            .get("openai/widgetPrefersBorder")
            .expect("widgetPrefersBorder not found");
        assert!(prefers_border.as_bool().unwrap());
    }

    #[tokio::test]
    async fn read_resource_impl_returns_mcp_format_when_target_is_mcp() {
        let running = running_with_apps(
            AppResource::Local("test content".to_string()),
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
                ReadResourceRequestParam {
                    uri: "http://localhost:4000/resource".parse().unwrap(),
                },
                extensions,
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
        let running = running_with_apps(AppResource::Local("test content".to_string()), None, None);

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?appTarget=invalid")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);

        let result = running
            .read_resource_impl(
                ReadResourceRequestParam {
                    uri: "http://localhost:4000/resource".parse().unwrap(),
                },
                extensions,
            )
            .await;

        assert!(result.is_err());
    }
}
