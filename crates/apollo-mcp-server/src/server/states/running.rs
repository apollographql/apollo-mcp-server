use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use apollo_compiler::{Schema, validation::Valid};
use opentelemetry::KeyValue;
use parking_lot::Mutex;
use reqwest::header::HeaderMap;
use rmcp::ErrorData;
use rmcp::model::{
    CallToolResponse, ClientCapabilities, Extensions, GetPromptRequestParams, GetPromptResponse,
    GetPromptResult, Implementation, ListPromptsResult, ListResourcesResult, PromptMessage,
    PromptsCapability, ReadResourceResponse, ReadResourceResult, ResourcesCapability, Role,
    ToolsCapability,
};
use rmcp::{
    Peer, RoleServer, ServerHandler, ServiceError,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, ErrorCode, InitializeRequestParams,
        InitializeResult, ListToolsResult, PaginatedRequestParams, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
};
use serde_json::Value;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};
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
    operations::{AnnotationOverrides, MutationMode, Operation, RawOperation},
};
use apollo_mcp_rhai::RhaiEngine;

#[derive(Clone)]
pub(super) struct Running {
    pub(super) schema: Arc<RwLock<Valid<Schema>>>,
    pub(super) operations: Arc<RwLock<Vec<Operation>>>,
    pub(super) apps: Vec<crate::apps::App>,
    pub(super) prompts: Vec<crate::prompts::PromptFile>,
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
    pub(super) annotations: HashMap<String, AnnotationOverrides>,
    pub(super) health_check: Option<HealthCheck>,
    pub(super) server_info: ServerInfoConfig,
    /// MCP initialize-response instructions (optional).
    pub(super) instructions: Option<String>,
    pub(super) rhai_engine: Arc<Mutex<RhaiEngine>>,
}

impl Running {
    /// Returns true when `enable_output_schema` is active and the negotiated
    /// protocol version supports `outputSchema` / `structuredContent` (MCP 2025-06-18+).
    fn client_supports_output_schema(&self, protocol_version: Option<&ProtocolVersion>) -> bool {
        self.enable_output_schema
            && protocol_version.is_some_and(|v| *v >= ProtocolVersion::V_2025_06_18)
    }

    /// Rebuilds the current predefined operation catalog against a new schema.
    ///
    /// Only state behind shared locks changes, so active server clones observe the update.
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
                        &self.annotations,
                        &self.descriptions,
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

        // Drop the operations lock before notifying peers. The operations are
        // already written, so clients will see the updated list when they
        // re-fetch. Holding the lock during notification can starve all
        // list_tools / call_tool / initialize requests if any peer notification
        // is slow or hangs.
        drop(operations_lock);

        // Notify MCP clients that tools have changed
        Self::notify_tool_list_changed(self.peers.clone()).await;
    }

    /// Replaces the current predefined operation catalog with the latest source update.
    ///
    /// Only state behind shared locks changes, so active server clones observe the update.
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
                            &self.annotations,
                            &self.descriptions,
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

        // Drop the operations lock before notifying peers (same rationale as update_schema).
        drop(operations_lock);

        // Notify MCP clients that tools have changed
        Self::notify_tool_list_changed(self.peers.clone()).await;
    }

    /// Reload Rhai scripts from the configured Rhai directory.
    /// On failure, logs the error and keeps the previous scripts.
    pub(super) fn reload_rhai_scripts(&self) {
        let mut engine = self.rhai_engine.lock();
        match engine.reload() {
            Ok(()) => {
                info!("Rhai scripts reloaded successfully");
            }
            Err(err) => {
                error!("Failed to reload Rhai scripts, keeping previous version: {err}");
            }
        }
    }

    /// Notify any peers that tools have changed. Drops unreachable peers from the list.
    ///
    /// Locking strategy: snapshot the peer list under a **read** lock, notify
    /// without holding any lock, then briefly take a **write** lock only to
    /// swap in the retained list. This keeps the write-lock hold time
    /// negligible regardless of how many peers need notifying.
    #[tracing::instrument(skip_all)]
    async fn notify_tool_list_changed(peers: Arc<RwLock<Vec<Peer<RoleServer>>>>) {
        const PEER_NOTIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

        // Snapshot under read lock, then release immediately so concurrent
        // initialize requests can register new peers without blocking.
        let snapshot: Vec<_> = {
            let guard = peers.read().await;
            if guard.is_empty() {
                return;
            }
            debug!(
                "Operations changed, notifying {} peers of tool change",
                guard.len()
            );
            guard.clone()
        };
        let snapshot_len = snapshot.len();

        // Notify without holding any lock.
        let mut retained_peers = Vec::new();
        for peer in &snapshot {
            if !peer.is_transport_closed() {
                match tokio::time::timeout(PEER_NOTIFY_TIMEOUT, peer.notify_tool_list_changed())
                    .await
                {
                    Ok(Ok(_)) => retained_peers.push(peer.clone()),
                    Ok(Err(ServiceError::TransportSend(_) | ServiceError::TransportClosed)) => {
                        error!("Failed to notify peer of tool list change - dropping peer");
                    }
                    Ok(Err(e)) => {
                        error!("Failed to notify peer of tool list change {:?}", e);
                        retained_peers.push(peer.clone());
                    }
                    Err(_) => {
                        error!(
                            "Timed out notifying peer of tool list change after {}s - dropping peer",
                            PEER_NOTIFY_TIMEOUT.as_secs()
                        );
                    }
                }
            }
        }

        // Brief write lock: replace the snapshot portion with retained peers,
        // preserving any peers added by concurrent initialize calls.
        let mut guard = peers.write().await;
        let new_peers: Vec<_> = if guard.len() > snapshot_len {
            guard.split_off(snapshot_len)
        } else {
            vec![]
        };
        *guard = retained_peers;
        guard.extend(new_peers);
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
                Some(app) => ListToolsResult::with_all_items(
                    self.operations
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
                ),
                None => {
                    return Err(McpError::new(
                        ErrorCode::INVALID_REQUEST,
                        format!("App {app_name} not found"),
                        None,
                    ));
                }
            }
        } else {
            ListToolsResult::with_all_items(
                self.operations
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
            )
        };

        if !self.client_supports_output_schema(protocol_version) {
            for tool in &mut result.tools {
                tool.output_schema = None;
            }
        }

        Ok(result)
    }

    async fn call_tool_impl(
        &self,
        request: CallToolRequestParams,
        extensions: &Extensions,
        protocol_version: Option<&ProtocolVersion>,
    ) -> Result<CallToolResult, McpError> {
        let meter = &meter::METER;
        let start = std::time::Instant::now();
        let tool_name = request.name;
        let app_param = extract_app_param(extensions);
        let axum_parts = extensions.get::<axum::http::request::Parts>();

        let mut result = if tool_name == INTROSPECT_TOOL_NAME
            && let Some(introspect_tool) = &self.introspect_tool
        {
            match serde_json::from_value(Value::from(request.arguments)) {
                Ok(args) => introspect_tool.execute(args).await,
                Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Invalid input: {e}"
                ))])),
            }
        } else if tool_name == SEARCH_TOOL_NAME
            && let Some(search_tool) = &self.search_tool
        {
            match serde_json::from_value(Value::from(request.arguments)) {
                Ok(args) => search_tool.execute(args).await,
                Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Invalid input: {e}"
                ))])),
            }
        } else if tool_name == EXPLORER_TOOL_NAME
            && let Some(explorer_tool) = &self.explorer_tool
        {
            match serde_json::from_value(Value::from(request.arguments)) {
                Ok(args) => explorer_tool.execute(args).await,
                Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Invalid input: {e}"
                ))])),
            }
        } else if tool_name == EXECUTE_TOOL_NAME
            && let Some(execute_tool) = &self.execute_tool
        {
            let headers = if let Some(axum_parts) = axum_parts {
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
                &self.rhai_engine,
                axum_parts,
                &tool_name,
            )
            .await
        } else if tool_name == VALIDATE_TOOL_NAME
            && let Some(validate_tool) = &self.validate_tool
        {
            match serde_json::from_value(Value::from(request.arguments)) {
                Ok(args) => Ok(validate_tool.execute(args).await),
                Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Invalid input: {e}"
                ))])),
            }
        } else {
            let headers = if let Some(axum_parts) = axum_parts {
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

            // Acquire the lock once: reused for scope check and execution.
            let ops = self.operations.read().await;

            if let Some(app_param) = &app_param {
                if let Some(res) = find_and_execute_app_tool(
                    &self.apps,
                    app_param,
                    &tool_name,
                    &headers,
                    request.arguments.as_ref(),
                    &self.endpoint,
                    &self.rhai_engine,
                    axum_parts,
                )
                .await
                {
                    res
                } else {
                    Err(tool_not_found(&tool_name))
                }
            } else if let Some(res) = find_and_execute_operation(
                &ops,
                &tool_name,
                &headers,
                request.arguments.as_ref(),
                &self.endpoint,
                &self.rhai_engine,
                axum_parts,
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
        if app_param.is_none()
            && !self.client_supports_output_schema(protocol_version)
            && let Ok(r) = &mut result
        {
            r.structured_content = None;
        }

        result
    }

    fn list_resources_impl(
        &self,
        extensions: &Extensions,
    ) -> Result<ListResourcesResult, McpError> {
        let app_param = extract_app_param(extensions);

        let resources = if let Some(app_name) = app_param {
            let app = self.apps.iter().find(|app| app.name == app_name);
            match app {
                Some(app) => vec![attach_resource_mime_type(app.resource())],
                None => {
                    return Err(McpError::new(
                        ErrorCode::INVALID_PARAMS,
                        format!("App {app_name} not found"),
                        None,
                    ));
                }
            }
        } else {
            vec![]
        };

        Ok(ListResourcesResult::with_all_items(resources))
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
        let app_param = extract_app_param(&extensions);
        let app_target = AppTarget::try_from((extensions, client_capabilities))?;

        if let Some(app_name) = app_param {
            let resource =
                get_app_resource(&self.apps, request, request_uri, &app_target, &app_name).await?;
            Ok(ReadResourceResult::new(vec![resource]))
        } else {
            Err(ErrorData::resource_not_found(
                format!("Resource not found for URI: {}", request.uri),
                None,
            ))
        }
    }

    fn list_prompts_impl(&self) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::with_all_items(
            self.prompts.iter().map(|p| p.prompt.clone()).collect(),
        ))
    }

    fn get_prompt_impl(
        &self,
        request: GetPromptRequestParams,
    ) -> Result<GetPromptResult, McpError> {
        let prompt_file = self
            .prompts
            .iter()
            .find(|p| p.prompt.name == request.name)
            .ok_or_else(|| {
                McpError::new(
                    ErrorCode::INVALID_PARAMS,
                    format!("Prompt '{}' not found", request.name),
                    None,
                )
            })?;

        // Validate required arguments are present
        if let Some(args_def) = &prompt_file.prompt.arguments {
            let provided = request.arguments.as_ref();
            for arg in args_def {
                if arg.required == Some(true)
                    && !provided.is_some_and(|a| a.contains_key(&arg.name))
                {
                    return Err(McpError::new(
                        ErrorCode::INVALID_PARAMS,
                        format!("Missing required argument: '{}'", arg.name),
                        None,
                    ));
                }
            }
        }

        let text = if let Some(arguments) = &request.arguments {
            crate::prompts::prompt_file::substitute_args(&prompt_file.template, arguments)
        } else {
            prompt_file.template.clone()
        };

        let mut result = GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)]);
        if let Some(desc) = &prompt_file.prompt.description {
            result = result.with_description(desc);
        }
        Ok(result)
    }
}

/// The newest MCP protocol version this server implements. Bumped
/// deliberately when the server adopts a new spec revision — not derived
/// from rmcp, whose `KNOWN_VERSIONS`/`LATEST` track SDK constants rather
/// than this server's capabilities.
pub(crate) const MAX_SUPPORTED_PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V_2025_11_25;

/// Echoes the client-requested protocol version when this server supports
/// it, otherwise falls back to [`MAX_SUPPORTED_PROTOCOL_VERSION`].
///
/// rmcp's own re-negotiation (which runs after `initialize` on every
/// transport, keyed off [`ServerHandler::supported_protocol_versions`]) caps
/// at the same version, so the two stay in agreement.
fn negotiate_protocol_version(client_requested: &ProtocolVersion) -> ProtocolVersion {
    if ProtocolVersion::KNOWN_VERSIONS.contains(client_requested)
        && *client_requested <= MAX_SUPPORTED_PROTOCOL_VERSION
    {
        client_requested.clone()
    } else {
        // debug rather than warn: falling back is expected, handled behavior,
        // and a pinned client would emit this on every stateless initialize.
        debug!(
            client_requested = %client_requested,
            server_fallback = %MAX_SUPPORTED_PROTOCOL_VERSION,
            "client requested unsupported protocol version; falling back to server default"
        );
        MAX_SUPPORTED_PROTOCOL_VERSION
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
        // Echo the client's requested protocol version when supported,
        // falling back to our max supported version otherwise (#794). rmcp's
        // handshake re-negotiates this after `initialize` on every
        // transport, but `supported_protocol_versions` below keeps that
        // re-negotiation capped at the same version (closes #803).
        let mut info = self.get_info();
        info.protocol_version = negotiate_protocol_version(&request.protocol_version);
        Ok(info)
    }

    /// Narrows rmcp's re-negotiation (run on every transport after
    /// `initialize`) to the versions this server implements, so it can't
    /// override our cap at [`MAX_SUPPORTED_PROTOCOL_VERSION`] with a newer
    /// version from rmcp's `KNOWN_VERSIONS` (e.g. `2026-07-28`'s SEP-2243
    /// headers and `subscriptions/listen`, which this server doesn't yet
    /// handle).
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        static SUPPORTED: LazyLock<Vec<ProtocolVersion>> = LazyLock::new(|| {
            ProtocolVersion::KNOWN_VERSIONS
                .iter()
                .filter(|version| **version <= MAX_SUPPORTED_PROTOCOL_VERSION)
                .cloned()
                .collect()
        });
        Cow::Borrowed(&SUPPORTED)
    }

    #[tracing::instrument(skip_all, parent = get_parent_span(&context), fields(apollo.mcp.tool_name = request.name.as_ref(), apollo.mcp.request_id = %context.id.clone(), apollo.mcp.tool_arguments = tracing::field::Empty, apollo.mcp.tool_result = tracing::field::Empty))]
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let span = tracing::Span::current();
        if let Some(args) = &request.arguments
            && let Ok(json) = serde_json::to_string(args)
        {
            span.record("apollo.mcp.tool_arguments", json.as_str());
        }

        let peer_info = context.peer.peer_info();
        let protocol_version = peer_info.as_ref().map(|info| &info.protocol_version);

        let result = self
            .call_tool_impl(request, &context.extensions, protocol_version)
            .await;

        // Strip meta before recording: _meta.structuredContent holds the unfiltered
        // @private payload and must not be exported to the span.
        if let Ok(r) = &result {
            let mut stripped = r.clone();
            stripped.meta = None;
            if let Ok(json) = serde_json::to_string(&stripped) {
                span.record("apollo.mcp.tool_result", json.as_str());
            }
        }

        result.map(Into::into)
    }

    #[tracing::instrument(skip_all, parent = get_parent_span(&context))]
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let peer_info = context.peer.peer_info();
        let client_capabilities = peer_info.as_ref().map(|info| &info.capabilities);
        let protocol_version = peer_info.as_ref().map(|info| &info.protocol_version);

        self.list_tools_impl(context.extensions, client_capabilities, protocol_version)
            .await
    }

    #[tracing::instrument(skip_all)]
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        self.list_resources_impl(&context.extensions)
    }

    #[tracing::instrument(skip_all, fields(apollo.mcp.resource_uri = request.uri.as_str(), apollo.mcp.request_id = %context.id.clone()))]
    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let peer_info = context.peer.peer_info();
        let client_capabilities = peer_info.as_ref().map(|info| &info.capabilities);

        self.read_resource_impl(request, context.extensions, client_capabilities)
            .await
            .map(Into::into)
    }

    #[tracing::instrument(skip_all)]
    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        self.list_prompts_impl()
    }

    #[tracing::instrument(skip_all, fields(apollo.mcp.prompt_name = request.name))]
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, McpError> {
        self.get_prompt_impl(request).map(Into::into)
    }

    // `logging` is deprecated by SEP-2577, but we still override this handler so
    // clients that send `logging/setLevel` without checking capabilities get an
    // empty success instead of `-32601`.
    #[allow(deprecated)]
    #[tracing::instrument(skip_all)]
    async fn set_level(
        &self,
        request: rmcp::model::SetLevelRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        // We do not advertise the `logging` capability and do not emit
        // `notifications/message`. This override exists only to accept
        // `logging/setLevel` from clients that send it without checking
        // capabilities, so they see an empty success instead of `-32601`.
        debug!(level = ?request.level, "received logging/setLevel; no-op");
        Ok(())
    }

    fn get_info(&self) -> ServerInfo {
        let meter = &meter::METER;
        meter
            .u64_counter(TelemetryMetric::GetInfoCount.as_str())
            .build()
            .add(1, &[]);

        let mut capabilities = ServerCapabilities::default();
        let mut tools = ToolsCapability::default();
        tools.list_changed = Some(true);
        capabilities.tools = Some(tools);
        capabilities.resources = (!self.apps.is_empty()).then(ResourcesCapability::default);
        capabilities.prompts = (!self.prompts.is_empty()).then(PromptsCapability::default);

        // Advertise the max supported version as the default. Our `initialize`
        // handler negotiates the per-client value, echoing the client's requested
        // version when supported and otherwise falling back to this one.
        let protocol_version = MAX_SUPPORTED_PROTOCOL_VERSION;

        let mut impl_ = Implementation::new(
            self.server_info.name().to_string(),
            self.server_info.version().to_string(),
        );
        if let Some(t) = self.server_info.title() {
            impl_ = impl_.with_title(t);
        }
        if let Some(d) = self.server_info.description() {
            impl_ = impl_.with_description(d);
        }
        if let Some(u) = self.server_info.website_url() {
            impl_ = impl_.with_website_url(u);
        }
        let mut result = InitializeResult::new(capabilities)
            .with_protocol_version(protocol_version)
            .with_server_info(impl_);
        if let Some(instructions) = self.instructions.as_deref() {
            result = result.with_instructions(instructions);
        }
        result
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
    use rmcp::model::{JsonObject, Tool};

    use crate::apps::{
        App,
        app::{AppResource, AppTool},
        manifest::{AppLabels, CSPSettings, WidgetSettings},
    };

    use super::*;

    fn test_running(schema: Arc<RwLock<Valid<Schema>>>) -> Running {
        Running {
            schema,
            operations: Arc::new(RwLock::new(vec![])),
            apps: vec![],
            prompts: vec![],
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
            annotations: HashMap::new(),
            health_check: None,
            server_info: ServerInfoConfig::default(),
            instructions: None,
            rhai_engine: Arc::new(parking_lot::Mutex::new(RhaiEngine::new("rhai"))),
        }
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
                        .into_operation(
                            &schema,
                            None,
                            MutationMode::All,
                            false,
                            false,
                            true,
                            &HashMap::new(),
                            &HashMap::new(),
                        )
                        .unwrap()
                        .unwrap(),
                ),
                labels: AppLabels::default(),
                tool: Tool::new("GetId", "a description", JsonObject::new()),
                extra_outputs: None,
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

    mod protocol_version_negotiation {
        use rstest::rstest;

        use super::*;

        /// Builds a version rmcp has no constant for; unknown versions are
        /// only constructible through deserialization.
        fn unknown_version(version: &str) -> ProtocolVersion {
            serde_json::from_value(serde_json::json!(version)).unwrap()
        }

        #[rstest]
        #[case::oldest_known(ProtocolVersion::V_2024_11_05)]
        #[case::intermediate_known(ProtocolVersion::V_2025_06_18)]
        #[case::max_supported(MAX_SUPPORTED_PROTOCOL_VERSION)]
        fn echoes_known_version_at_or_below_cap(#[case] requested: ProtocolVersion) {
            assert_eq!(negotiate_protocol_version(&requested), requested);
        }

        #[test]
        fn caps_known_version_above_max_supported() {
            // rmcp has a constant for 2026-07-28, but this server doesn't
            // implement that revision.
            assert_eq!(
                negotiate_protocol_version(&ProtocolVersion::V_2026_07_28),
                MAX_SUPPORTED_PROTOCOL_VERSION
            );
        }

        #[rstest]
        #[case::future_date("2999-01-01")]
        #[case::past_date("2020-01-01")]
        #[case::not_a_date("not-a-version")]
        fn falls_back_when_version_is_unknown(#[case] requested: &str) {
            assert_eq!(
                negotiate_protocol_version(&unknown_version(requested)),
                MAX_SUPPORTED_PROTOCOL_VERSION
            );
        }
    }

    mod update_operations {
        use super::*;
        use rmcp::model::Tool;

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
        async fn overrides_annotations_applied_to_operations() {
            let schema = Schema::parse("type Query { id: String }", "schema.graphql")
                .unwrap()
                .validate()
                .unwrap();

            let operations = Arc::new(RwLock::new(vec![]));

            let annotations = HashMap::from([(
                "GetId".to_string(),
                AnnotationOverrides {
                    idempotent_hint: Some(true),
                    open_world_hint: Some(false),
                    ..Default::default()
                },
            )]);

            let running = Running {
                operations: operations.clone(),
                annotations,
                ..test_running(Arc::new(RwLock::new(schema)))
            };

            let new_operations = vec![RawOperation::from((
                "query GetId { id }".to_string(),
                Some("get_id.graphql".to_string()),
            ))];

            running.update_operations(new_operations).await;

            let updated = operations.read().await;
            let tool: &Tool = updated.first().unwrap().as_ref();
            let ann = tool.annotations.as_ref().unwrap();
            assert_eq!(
                ann.idempotent_hint,
                Some(true),
                "Override annotation should be applied"
            );
            assert_eq!(
                ann.open_world_hint,
                Some(false),
                "Override annotation should be applied"
            );
            assert_eq!(
                ann.read_only_hint,
                Some(true),
                "Auto-detected hint should be preserved"
            );
        }

        #[tokio::test]
        async fn overrides_annotations_do_not_affect_unmatched_operations() {
            let schema = Schema::parse("type Query { id: String }", "schema.graphql")
                .unwrap()
                .validate()
                .unwrap();

            let operations = Arc::new(RwLock::new(vec![]));

            let annotations = HashMap::from([(
                "NonExistent".to_string(),
                AnnotationOverrides {
                    idempotent_hint: Some(false),
                    ..Default::default()
                },
            )]);

            let running = Running {
                operations: operations.clone(),
                annotations,
                ..test_running(Arc::new(RwLock::new(schema)))
            };

            let new_operations = vec![RawOperation::from((
                "query GetId { id }".to_string(),
                Some("get_id.graphql".to_string()),
            ))];

            running.update_operations(new_operations).await;

            let updated = operations.read().await;
            let tool: &Tool = updated.first().unwrap().as_ref();
            let ann = tool.annotations.as_ref().unwrap();
            assert_eq!(
                ann.idempotent_hint,
                Some(true),
                "Unmatched override (false) must not replace auto-detected default (true)"
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
    }

    mod list_resources {
        use crate::apps::app::{AppResource, AppResourceSource};

        use super::*;

        #[tokio::test]
        async fn resource_list_includes_app_resources() {
            let mut extensions = Extensions::new();
            let request = axum::http::Request::builder()
                .uri("http://localhost?app=MyApp")
                .body(())
                .unwrap();
            let (parts, _) = request.into_parts();
            extensions.insert(parts);

            let resources = running_with_apps(
                AppResource::Single(AppResourceSource::Local("abcdef".to_string())),
                None,
                None,
            )
            .list_resources_impl(&extensions)
            .unwrap()
            .resources;

            assert_eq!(resources.len(), 1);
            assert_eq!(resources[0].uri, RESOURCE_URI);
        }

        #[tokio::test]
        async fn resource_list_attaches_mcp_apps_mime_type() {
            let mut extensions = Extensions::new();
            let request = axum::http::Request::builder()
                .uri("http://localhost?app=MyApp")
                .body(())
                .unwrap();
            let (parts, _) = request.into_parts();
            extensions.insert(parts);

            let resources = running_with_apps(
                AppResource::Single(AppResourceSource::Local("abcdef".to_string())),
                None,
                None,
            )
            .list_resources_impl(&extensions)
            .unwrap()
            .resources;

            assert_eq!(resources.len(), 1);
            assert_eq!(
                resources[0].mime_type,
                Some("text/html;profile=mcp-app".into())
            );
        }

        #[tokio::test]
        async fn resource_list_empty_without_app_param() {
            let resources = running_with_apps(
                AppResource::Single(AppResourceSource::Local("abcdef".to_string())),
                None,
                None,
            )
            .list_resources_impl(&Extensions::new())
            .unwrap()
            .resources;

            assert!(resources.is_empty());
        }

        #[tokio::test]
        async fn resource_list_with_nonexistent_app() {
            let mut extensions = Extensions::new();
            let request = axum::http::Request::builder()
                .uri("http://localhost?app=NonExistent")
                .body(())
                .unwrap();
            let (parts, _) = request.into_parts();
            extensions.insert(parts);

            let result = running_with_apps(
                AppResource::Single(AppResourceSource::Local("abcdef".to_string())),
                None,
                None,
            )
            .list_resources_impl(&extensions);

            assert!(result.is_err());
        }
    }

    mod read_resource {
        use rmcp::model::{ReadResourceRequestParams, ResourceContents};

        use crate::apps::{
            app::{AppResource, AppResourceSource},
            manifest::CSPSettings,
        };

        use super::*;

        #[tokio::test]
        async fn getting_resource_from_running() {
            let resource_content = "This is a test resource";
            let running = running_with_apps(
                AppResource::Single(AppResourceSource::Local(resource_content.to_string())),
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

            let mut resource = running
                .read_resource_impl(
                    ReadResourceRequestParams::new(
                        "http://localhost:4000/resource#a_different_fragment",
                    ),
                    extensions,
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
            let mut extensions = Extensions::new();
            let request = axum::http::Request::builder()
                .uri("http://localhost?app=MyApp")
                .body(())
                .unwrap();
            let (parts, _) = request.into_parts();
            extensions.insert(parts);

            let result = running
                .read_resource_impl(
                    ReadResourceRequestParams::new("http://localhost:4000/invalid_resource"),
                    extensions,
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
            let mut extensions = Extensions::new();
            let request = axum::http::Request::builder()
                .uri("http://localhost?app=MyApp")
                .body(())
                .unwrap();
            let (parts, _) = request.into_parts();
            extensions.insert(parts);

            let result = running
                .read_resource_impl(
                    ReadResourceRequestParams::new("not a uri"),
                    extensions,
                    None,
                )
                .await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn read_resource_without_app_param_returns_error() {
            let running = running_with_apps(
                AppResource::Single(AppResourceSource::Local("abcdef".to_string())),
                None,
                None,
            );
            let result = running
                .read_resource_impl(
                    ReadResourceRequestParams::new("http://localhost:4000/resource"),
                    Extensions::new(),
                    None,
                )
                .await;
            assert!(result.is_err());
        }

        #[tokio::test]
        async fn read_resource_with_wrong_app_param_returns_error() {
            let running = running_with_apps(
                AppResource::Single(AppResourceSource::Local("abcdef".to_string())),
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

            let result = running
                .read_resource_impl(
                    ReadResourceRequestParams::new("http://localhost:4000/resource"),
                    extensions,
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

            let mut extensions = Extensions::new();
            let request = axum::http::Request::builder()
                .uri("http://localhost?app=MyApp")
                .body(())
                .unwrap();
            let (parts, _) = request.into_parts();
            extensions.insert(parts);

            let mut resource = running
                .read_resource_impl(
                    ReadResourceRequestParams::new(RESOURCE_URI),
                    extensions,
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
            let mut extensions = Extensions::new();
            let request = axum::http::Request::builder()
                .uri("http://localhost?app=MyApp")
                .body(())
                .unwrap();
            let (parts, _) = request.into_parts();
            extensions.insert(parts);

            let mut resource = running
                .read_resource_impl(
                    ReadResourceRequestParams::new("http://localhost:4000/resource"),
                    extensions,
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
            let mut extensions = Extensions::new();
            let request = axum::http::Request::builder()
                .uri("http://localhost?app=MyApp")
                .body(())
                .unwrap();
            let (parts, _) = request.into_parts();
            extensions.insert(parts);

            let mut resource = running
                .read_resource_impl(
                    ReadResourceRequestParams::new("http://localhost:4000/resource"),
                    extensions,
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
            let mut extensions = Extensions::new();
            let request = axum::http::Request::builder()
                .uri("http://localhost?app=MyApp")
                .body(())
                .unwrap();
            let (parts, _) = request.into_parts();
            extensions.insert(parts);

            let mut resource = running
                .read_resource_impl(
                    ReadResourceRequestParams::new("http://localhost:4000/resource"),
                    extensions,
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
            let mut extensions = Extensions::new();
            let request = axum::http::Request::builder()
                .uri("http://localhost?app=MyApp")
                .body(())
                .unwrap();
            let (parts, _) = request.into_parts();
            extensions.insert(parts);

            let mut resource = running
                .read_resource_impl(
                    ReadResourceRequestParams::new("http://localhost:4000/resource"),
                    extensions,
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
                .uri("http://localhost?app=MyApp&appTarget=mcp")
                .body(())
                .unwrap();
            let (parts, _) = request.into_parts();
            extensions.insert(parts);

            let mut resource = running
                .read_resource_impl(
                    ReadResourceRequestParams::new("http://localhost:4000/resource"),
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
                .uri("http://localhost?app=MyApp&appTarget=invalid")
                .body(())
                .unwrap();
            let (parts, _) = request.into_parts();
            extensions.insert(parts);

            let result = running
                .read_resource_impl(
                    ReadResourceRequestParams::new("http://localhost:4000/resource"),
                    extensions,
                    None,
                )
                .await;

            assert!(result.is_err());
        }
    }

    mod list_tools {
        use crate::apps::app::{AppResource, AppResourceSource};

        use super::*;

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
            let mut client_capabilities = ClientCapabilities::default();
            client_capabilities.extensions = Some(extension_capabilities);

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
        async fn excludes_output_schema_when_protocol_predates_it() {
            let schema =
                apollo_compiler::Schema::parse_and_validate("type Query { hello: String }", "test")
                    .unwrap();

            let raw_op: RawOperation = ("query Hello { hello }".to_string(), None).into();
            let operation = raw_op
                .into_operation(
                    &schema,
                    None,
                    MutationMode::None,
                    false,
                    false,
                    true,
                    &HashMap::new(),
                    &HashMap::new(),
                )
                .unwrap()
                .expect("operation should be valid");

            let running = Running {
                operations: Arc::new(RwLock::new(vec![operation])),
                enable_output_schema: true,
                ..test_running(Arc::new(RwLock::new(schema)))
            };

            let result = running
                .list_tools_impl(
                    Extensions::new(),
                    None,
                    Some(&ProtocolVersion::V_2025_03_26),
                )
                .await
                .unwrap();

            assert!(!result.tools.is_empty());
            for tool in &result.tools {
                assert!(
                    tool.output_schema.is_none(),
                    "tool '{}' should not have output_schema with default protocol version",
                    tool.name
                );
            }
        }

        #[tokio::test]
        async fn includes_output_schema_when_protocol_supports_it() {
            let schema =
                apollo_compiler::Schema::parse_and_validate("type Query { hello: String }", "test")
                    .unwrap();

            let raw_op: RawOperation = ("query Hello { hello }".to_string(), None).into();
            let operation = raw_op
                .into_operation(
                    &schema,
                    None,
                    MutationMode::None,
                    false,
                    false,
                    true,
                    &HashMap::new(),
                    &HashMap::new(),
                )
                .unwrap()
                .expect("operation should be valid");

            let running = Running {
                operations: Arc::new(RwLock::new(vec![operation])),
                enable_output_schema: true,
                ..test_running(Arc::new(RwLock::new(schema)))
            };

            let result = running
                .list_tools_impl(
                    Extensions::new(),
                    None,
                    Some(&ProtocolVersion::V_2025_06_18),
                )
                .await
                .unwrap();

            assert!(!result.tools.is_empty());
            for tool in &result.tools {
                assert!(
                    tool.output_schema.is_some(),
                    "tool '{}' should have output_schema with protocol 2025-06-18",
                    tool.name
                );
            }
        }
    }

    mod get_info {
        use super::*;
        use rstest::rstest;

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
            assert_eq!(info.instructions, None);
        }

        #[test]
        fn get_info_includes_instructions_when_configured() {
            let schema = Schema::parse("type Query { id: String }", "schema.graphql")
                .unwrap()
                .validate()
                .unwrap();

            let running = Running {
                instructions: Some("Prefer search before list.".to_string()),
                ..test_running(Arc::new(RwLock::new(schema)))
            };

            let info = running.get_info();
            assert_eq!(
                info.instructions.as_deref(),
                Some("Prefer search before list.")
            );
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

        #[rstest]
        #[case::output_schema_disabled(false)]
        #[case::output_schema_enabled(true)]
        fn advertises_max_supported_version_regardless_of_output_schema(
            #[case] enable_output_schema: bool,
        ) {
            let schema = Arc::new(RwLock::new(
                Schema::parse("type Query { id: String }", "schema.graphql")
                    .unwrap()
                    .validate()
                    .unwrap(),
            ));

            // The advertised version no longer depends on `enable_output_schema`;
            // output schema fields are gated separately by the negotiated version.
            let running = Running {
                enable_output_schema,
                ..test_running(Arc::clone(&schema))
            };

            let info = running.get_info();

            assert_eq!(info.protocol_version, MAX_SUPPORTED_PROTOCOL_VERSION);
        }
    }

    mod prompts {
        use super::*;
        use rmcp::model::{GetPromptRequestParams, Prompt, PromptArgument, Role};

        fn running_with_prompts(prompts: Vec<crate::prompts::PromptFile>) -> Running {
            let schema = Schema::parse("type Query { id: String }", "schema.graphql")
                .unwrap()
                .validate()
                .unwrap();
            Running {
                prompts,
                ..test_running(Arc::new(RwLock::new(schema)))
            }
        }

        #[test]
        fn list_prompts_empty() {
            let running = running_with_prompts(vec![]);
            let result = running.list_prompts_impl().unwrap();
            assert!(result.prompts.is_empty());
        }

        #[test]
        fn list_prompts_returns_loaded_prompts() {
            let prompts = vec![crate::prompts::PromptFile {
                prompt: Prompt::new("greeting", Some("A greeting"), None),
                template: "Hello {{name}}!".to_string(),
            }];
            let running = running_with_prompts(prompts);
            let result = running.list_prompts_impl().unwrap();
            assert_eq!(result.prompts.len(), 1);
            assert_eq!(result.prompts[0].name, "greeting");
            assert_eq!(result.prompts[0].description.as_deref(), Some("A greeting"));
        }

        #[test]
        fn get_prompt_substitutes_arguments() {
            let prompts = vec![crate::prompts::PromptFile {
                prompt: Prompt::new(
                    "greet",
                    Some("Greet someone"),
                    Some(vec![PromptArgument::new("name").with_required(true)]),
                ),
                template: "Hello {{name}}!".to_string(),
            }];
            let running = running_with_prompts(prompts);
            let mut args = serde_json::Map::new();
            args.insert("name".to_string(), serde_json::json!("Alice"));
            let result = running
                .get_prompt_impl(GetPromptRequestParams::new("greet").with_arguments(args))
                .unwrap();
            assert_eq!(result.messages.len(), 1);
            assert_eq!(result.messages[0].role, Role::User);
            assert!(
                matches!(
                    &result.messages[0].content,
                    rmcp::model::ContentBlock::Text(text) if text.text == "Hello Alice!"
                ),
                "Expected Text content with 'Hello Alice!', got {:?}",
                result.messages[0].content
            );
        }

        #[test]
        fn get_prompt_not_found() {
            let running = running_with_prompts(vec![]);
            let err = running
                .get_prompt_impl(GetPromptRequestParams::new("nonexistent"))
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
            assert!(err.message.contains("not found"));
        }

        #[test]
        fn get_prompt_missing_required_argument() {
            let prompts = vec![crate::prompts::PromptFile {
                prompt: Prompt::new(
                    "greet",
                    None::<String>,
                    Some(vec![PromptArgument::new("name").with_required(true)]),
                ),
                template: "Hello {{name}}!".to_string(),
            }];
            let running = running_with_prompts(prompts);
            let err = running
                .get_prompt_impl(GetPromptRequestParams::new("greet"))
                .unwrap_err();
            assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
            assert!(err.message.contains("Missing required argument"));
        }

        #[test]
        fn get_info_includes_prompts_capability_when_prompts_exist() {
            let prompts = vec![crate::prompts::PromptFile {
                prompt: Prompt::new("test", None::<String>, None),
                template: "test".to_string(),
            }];
            let running = running_with_prompts(prompts);
            let info = running.get_info();
            assert!(info.capabilities.prompts.is_some());
        }

        #[test]
        fn get_info_no_prompts_capability_when_empty() {
            let running = running_with_prompts(vec![]);
            let info = running.get_info();
            assert!(info.capabilities.prompts.is_none());
        }
    }

    mod call_tool {
        use super::*;
        use crate::apps::app::{AppResource, AppResourceSource};
        use crate::operations::RawOperation;

        #[tokio::test]
        async fn strips_structured_content_when_protocol_predates_it() {
            let mut server = mockito::Server::new_async().await;
            let mock = server
                .mock("POST", "/")
                .with_body(r#"{"data": {"hello": "world"}}"#)
                .create_async()
                .await;

            let schema =
                apollo_compiler::Schema::parse_and_validate("type Query { hello: String }", "test")
                    .unwrap();

            let raw_op: RawOperation = ("query Hello { hello }".to_string(), None).into();
            let operation = raw_op
                .into_operation(
                    &schema,
                    None,
                    MutationMode::None,
                    false,
                    false,
                    true,
                    &HashMap::new(),
                    &HashMap::new(),
                )
                .unwrap()
                .expect("operation should be valid");

            let running = Running {
                operations: Arc::new(RwLock::new(vec![operation])),
                endpoint: server.url().parse().unwrap(),
                enable_output_schema: true,
                ..test_running(Arc::new(RwLock::new(schema)))
            };

            let mut request = CallToolRequestParams::new("Hello");
            request.arguments = Some(Default::default());

            let result = running
                .call_tool_impl(
                    request,
                    &Extensions::new(),
                    Some(&ProtocolVersion::V_2025_03_26),
                )
                .await
                .unwrap();

            mock.assert();
            assert!(
                result.structured_content.is_none(),
                "structured_content should be stripped with default protocol version"
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

            let schema =
                apollo_compiler::Schema::parse_and_validate("type Query { hello: String }", "test")
                    .unwrap();

            let raw_op: RawOperation = ("query Hello { hello }".to_string(), None).into();
            let operation = raw_op
                .into_operation(
                    &schema,
                    None,
                    MutationMode::None,
                    false,
                    false,
                    true,
                    &HashMap::new(),
                    &HashMap::new(),
                )
                .unwrap()
                .expect("operation should be valid");

            let running = Running {
                operations: Arc::new(RwLock::new(vec![operation])),
                endpoint: server.url().parse().unwrap(),
                enable_output_schema: true,
                ..test_running(Arc::new(RwLock::new(schema)))
            };

            let mut request = CallToolRequestParams::new("Hello");
            request.arguments = Some(Default::default());

            let result = running
                .call_tool_impl(
                    request,
                    &Extensions::new(),
                    Some(&ProtocolVersion::V_2025_06_18),
                )
                .await
                .unwrap();

            mock.assert();
            assert!(
                result.structured_content.is_some(),
                "structured_content should be preserved with protocol 2025-06-18"
            );
        }

        #[tokio::test]
        async fn calls_app_tool_instead_of_operation_when_app_param_present() {
            let mut server = mockito::Server::new_async().await;

            // Mock for the operation "Hello" — should NOT be called
            let operation_mock = server
                .mock("POST", "/")
                .match_body(mockito::Matcher::Regex(
                    r#".*"operationName"\s*:\s*"Hello".*"#.to_string(),
                ))
                .with_body(r#"{"data": {"hello": "from operation"}}"#)
                .with_header("Content-Type", "application/json")
                .expect(0)
                .create_async()
                .await;

            // Mock for the app tool's operation "AppHello" — should be called
            let app_tool_mock = server
                .mock("POST", "/")
                .match_body(mockito::Matcher::Regex(
                    r#".*"operationName"\s*:\s*"AppHello".*"#.to_string(),
                ))
                .with_body(r#"{"data": {"hello": "from app"}}"#)
                .with_header("Content-Type", "application/json")
                .expect(1)
                .create_async()
                .await;

            let schema =
                apollo_compiler::Schema::parse_and_validate("type Query { hello: String }", "test")
                    .unwrap();

            let operation: RawOperation = ("query Hello { hello }".to_string(), None).into();
            let operation = operation
                .into_operation(
                    &schema,
                    None,
                    MutationMode::None,
                    false,
                    false,
                    true,
                    &HashMap::new(),
                    &HashMap::new(),
                )
                .unwrap()
                .expect("operation should be valid");

            let app_operation: RawOperation = ("query AppHello { hello }".to_string(), None).into();
            let app_operation = app_operation
                .into_operation(
                    &schema,
                    None,
                    MutationMode::None,
                    false,
                    false,
                    true,
                    &HashMap::new(),
                    &HashMap::new(),
                )
                .unwrap()
                .expect("app operation should be valid");

            let app = App {
                name: "MyApp".to_string(),
                description: None,
                resource: AppResource::Single(AppResourceSource::Local("test".to_string())),
                csp_settings: None,
                widget_settings: None,
                uri: "ui://MyApp".parse().unwrap(),
                tools: vec![AppTool {
                    operation: Arc::new(app_operation),
                    labels: AppLabels::default(),
                    tool: Tool::new("Hello", "app tool", JsonObject::new()),
                    extra_outputs: None,
                }],
                prefetch_operations: vec![],
            };

            let running = Running {
                operations: Arc::new(RwLock::new(vec![operation])),
                apps: vec![app],
                endpoint: server.url().parse().unwrap(),
                enable_output_schema: true,
                ..test_running(Arc::new(RwLock::new(schema)))
            };

            let mut extensions = Extensions::new();
            let request = axum::http::Request::builder()
                .uri("http://localhost?app=MyApp")
                .body(())
                .unwrap();
            let (parts, _) = request.into_parts();
            extensions.insert(parts);

            let mut request = CallToolRequestParams::new("Hello");
            request.arguments = Some(Default::default());

            let _result = running
                .call_tool_impl(request, &extensions, None)
                .await
                .unwrap();

            app_tool_mock.assert();
            operation_mock.assert();
        }
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;

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
                .into_operation(
                    &schema,
                    None,
                    MutationMode::None,
                    false,
                    false,
                    true,
                    &HashMap::new(),
                    &HashMap::new(),
                )
                .unwrap()
                .expect("operation should be valid");

            Running {
                schema: Arc::new(RwLock::new(schema)),
                operations: Arc::new(RwLock::new(vec![operation])),
                apps: vec![],
                prompts: vec![],
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
                annotations: HashMap::new(),
                health_check: None,
                server_info: Default::default(),
                instructions: None,
                rhai_engine: Arc::new(parking_lot::Mutex::new(RhaiEngine::new("rhai"))),
            }
        }

        fn create_service(
            running: Running,
            session_manager: Arc<LocalSessionManager>,
        ) -> StreamableHttpService<Running, LocalSessionManager> {
            StreamableHttpService::new(
                move || Ok(running.clone()),
                session_manager,
                StreamableHttpServerConfig::default().with_legacy_session_mode(true),
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
                .header("Host", "localhost:8000")
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
                .header("Host", "localhost:8000")
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
                .header("Host", "localhost:8000")
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

        #[tokio::test]
        async fn negotiates_down_when_client_requests_newer_version() {
            // Regression for AMS-525: a client offering a protocol version newer
            // than any rmcp implements over streamable_http must be downgraded to
            // the max supported version rather than refused.
            let running = create_running_with_output_schema();
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();
            let service = create_service(running, Arc::clone(&session_manager));

            let response = service
                .oneshot(build_initialize_request("2999-01-01"))
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let body = extract_json_body(response).await;
            assert_eq!(
                body["result"]["protocolVersion"],
                MAX_SUPPORTED_PROTOCOL_VERSION.as_str()
            );
        }

        #[tokio::test]
        async fn echoes_supported_version_when_client_requests_older_version() {
            // rmcp negotiates over streamable_http: a client offering a supported
            // version older than our latest gets that version echoed back rather
            // than being upgraded to the latest.
            let running = create_running_with_output_schema();
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();
            let service = create_service(running, Arc::clone(&session_manager));

            let response = service
                .oneshot(build_initialize_request("2025-06-18"))
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let body = extract_json_body(response).await;
            assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
        }

        #[tokio::test]
        async fn negotiates_protocol_version_over_stdio_transport() {
            // The `stdio` transport drives this same `ServerHandler` through rmcp's
            // generic `serve()` (see `Transport::Stdio` in `starting.rs`), which
            // negotiates the protocol version in its service layer just like
            // `StreamableHttpService`. This regression test proves a stdio client
            // offering a supported older version gets that version echoed back
            // rather than the server's latest, so removing the handler-side
            // negotiation does not silently regress stdio.
            use rmcp::ServiceExt as _;
            use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

            let running = create_running_with_output_schema();

            // In-memory transport standing in for stdio: the server wraps its half
            // and the test drives the client half with newline-delimited JSON-RPC.
            let (server_io, client_io) = tokio::io::duplex(4096);
            let (server_r, server_w) = tokio::io::split(server_io);
            let (client_r, mut client_w) = tokio::io::split(client_io);

            let server = tokio::spawn(async move {
                let service = running
                    .serve((server_r, server_w))
                    .await
                    .expect("stdio serve should complete the initialize handshake");
                let _ = service.waiting().await;
            });

            let mut request = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": "test-client", "version": "1.0.0" }
                }
            })
            .to_string();
            request.push('\n');
            client_w.write_all(request.as_bytes()).await.unwrap();

            let mut reader = BufReader::new(client_r);
            let mut response_line = String::new();
            reader.read_line(&mut response_line).await.unwrap();
            let body: serde_json::Value = serde_json::from_str(&response_line).unwrap();
            assert_eq!(body["result"]["protocolVersion"], "2025-03-26");

            // Close the client side so the server task shuts down cleanly.
            drop(reader);
            drop(client_w);
            let _ = server.await;
        }

        #[tokio::test]
        async fn stdio_caps_at_max_supported_when_client_requests_newer_known_version() {
            // Mirrors `stateless_caps_at_max_supported_when_client_requests_newer_known_version`
            // for the stdio transport: rmcp's generic `serve()` negotiates through
            // the same `supported_protocol_versions` hook (service/server.rs), a
            // different code path from `StreamableHttpService`'s tower layer. This
            // proves the cap holds there too, closing the one transport #803's fix
            // didn't yet have a regression test for.
            use rmcp::ServiceExt as _;
            use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

            let running = create_running_with_output_schema();

            let (server_io, client_io) = tokio::io::duplex(4096);
            let (server_r, server_w) = tokio::io::split(server_io);
            let (client_r, mut client_w) = tokio::io::split(client_io);

            let server = tokio::spawn(async move {
                let service = running
                    .serve((server_r, server_w))
                    .await
                    .expect("stdio serve should complete the initialize handshake");
                let _ = service.waiting().await;
            });

            let mut request = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2026-07-28",
                    "capabilities": {},
                    "clientInfo": { "name": "test-client", "version": "1.0.0" }
                }
            })
            .to_string();
            request.push('\n');
            client_w.write_all(request.as_bytes()).await.unwrap();

            let mut reader = BufReader::new(client_r);
            let mut response_line = String::new();
            reader.read_line(&mut response_line).await.unwrap();
            let body: serde_json::Value = serde_json::from_str(&response_line).unwrap();
            assert_eq!(
                body["result"]["protocolVersion"],
                MAX_SUPPORTED_PROTOCOL_VERSION.as_str()
            );

            drop(reader);
            drop(client_w);
            let _ = server.await;
        }

        fn create_stateless_service(
            running: Running,
            session_manager: Arc<LocalSessionManager>,
        ) -> StreamableHttpService<Running, LocalSessionManager> {
            StreamableHttpService::new(
                move || Ok(running.clone()),
                session_manager,
                StreamableHttpServerConfig::default().with_legacy_session_mode(false),
            )
        }

        #[tokio::test]
        async fn stateless_echoes_supported_version_when_client_requests_older_version() {
            // Regression for #794: a stateless client requesting a supported
            // older version must get that version echoed back, not the latest.
            let running = create_running_with_output_schema();
            let service = create_stateless_service(running, LocalSessionManager::default().into());

            let response = service
                .oneshot(build_initialize_request("2025-06-18"))
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let body = extract_json_body(response).await;
            assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
        }

        #[tokio::test]
        async fn stateless_negotiates_down_when_client_requests_unknown_version() {
            // A client offering a version rmcp doesn't know must fall back to
            // our max supported version rather than having it echoed back.
            let running = create_running_with_output_schema();
            let service = create_stateless_service(running, LocalSessionManager::default().into());

            let response = service
                .oneshot(build_initialize_request("2999-01-01"))
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let body = extract_json_body(response).await;
            assert_eq!(
                body["result"]["protocolVersion"],
                MAX_SUPPORTED_PROTOCOL_VERSION.as_str()
            );
        }

        #[tokio::test]
        async fn stateless_caps_at_max_supported_when_client_requests_newer_known_version() {
            // rmcp has a constant for 2026-07-28, but this server doesn't
            // implement that revision (SEP-2243 headers, subscriptions/listen).
            // The stateless path must cap at the max supported version rather
            // than advertise a version whose follow-up requests we can't
            // handle.
            let running = create_running_with_output_schema();
            let service = create_stateless_service(running, LocalSessionManager::default().into());

            let response = service
                .oneshot(build_initialize_request("2026-07-28"))
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let body = extract_json_body(response).await;
            assert_eq!(
                body["result"]["protocolVersion"],
                MAX_SUPPORTED_PROTOCOL_VERSION.as_str()
            );
        }

        #[tokio::test]
        async fn legacy_session_mode_enabled_still_caps_at_max_supported_when_client_requests_newer_known_version()
         {
            // Requesting protocol version 2026-07-28 always uses the
            // stateless/discover lifecycle regardless of `legacy_session_mode`
            // (SEP-2567), so this exercises the same `NegotiatingStatelessHttpService`
            // path as `stateless_caps_at_max_supported_when_client_requests_newer_known_version`,
            // not a distinct legacy-session handshake. This pins that enabling
            // `legacy_session_mode` doesn't accidentally exempt that request from
            // the cap (closes #803);
            // `stdio_caps_at_max_supported_when_client_requests_newer_known_version`
            // covers the one code path (rmcp's generic `serve()`) that isn't
            // routed through `StreamableHttpService`.
            let running = create_running_with_output_schema();
            let service = create_service(running, LocalSessionManager::default().into());

            let response = service
                .oneshot(build_initialize_request("2026-07-28"))
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let body = extract_json_body(response).await;
            assert_eq!(
                body["result"]["protocolVersion"],
                MAX_SUPPORTED_PROTOCOL_VERSION.as_str()
            );
        }

        #[test]
        fn rmcp_known_versions_audit() {
            // Canary: fails when an rmcp upgrade changes KNOWN_VERSIONS.
            // When it does, audit the new protocol version(s):
            // - If this server implements the new revision, bump
            //   MAX_SUPPORTED_PROTOCOL_VERSION.
            // - Otherwise, update this list to acknowledge the version
            //   remains capped, and confirm stateful transports (where rmcp
            //   negotiates over our heads) still behave acceptably.
            assert_eq!(
                ProtocolVersion::KNOWN_VERSIONS,
                &[
                    ProtocolVersion::V_2024_11_05,
                    ProtocolVersion::V_2025_03_26,
                    ProtocolVersion::V_2025_06_18,
                    ProtocolVersion::V_2025_11_25,
                    ProtocolVersion::V_2026_07_28,
                ],
                "rmcp's KNOWN_VERSIONS changed; audit whether MAX_SUPPORTED_PROTOCOL_VERSION should move"
            );
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
                .into_operation(
                    &schema,
                    None,
                    MutationMode::None,
                    false,
                    false,
                    true,
                    &HashMap::new(),
                    &HashMap::new(),
                )
                .unwrap()
                .expect("operation should be valid");

            Running {
                schema: Arc::new(RwLock::new(schema)),
                operations: Arc::new(RwLock::new(vec![operation])),
                apps: vec![],
                prompts: vec![],
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
                annotations: HashMap::new(),
                health_check: None,
                server_info: Default::default(),
                instructions: None,
                rhai_engine: Arc::new(parking_lot::Mutex::new(RhaiEngine::new("rhai"))),
            }
        }

        fn create_service(
            running: Running,
            session_manager: Arc<LocalSessionManager>,
        ) -> StreamableHttpService<Running, LocalSessionManager> {
            StreamableHttpService::new(
                move || Ok(running.clone()),
                session_manager,
                StreamableHttpServerConfig::default().with_legacy_session_mode(true),
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
                .header("Host", "localhost:8000")
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
                .header("Host", "localhost:8000")
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
                .header("Host", "localhost:8000")
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
                prompts: vec![],
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
                annotations: HashMap::new(),
                health_check: None,
                server_info: Default::default(),
                instructions: None,
                rhai_engine: Arc::new(parking_lot::Mutex::new(RhaiEngine::new("rhai"))),
            }
        }

        fn create_test_service(
            stateful_mode: bool,
        ) -> StreamableHttpService<Running, LocalSessionManager> {
            let running = create_test_running();
            StreamableHttpService::new(
                move || Ok(running.clone()),
                LocalSessionManager::default().into(),
                StreamableHttpServerConfig::default().with_legacy_session_mode(stateful_mode),
            )
        }

        fn create_stateful_service(
            running: Running,
            session_manager: Arc<LocalSessionManager>,
        ) -> StreamableHttpService<Running, LocalSessionManager> {
            StreamableHttpService::new(
                move || Ok(running.clone()),
                session_manager,
                StreamableHttpServerConfig::default().with_legacy_session_mode(true),
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
                .header("Host", "localhost:8000")
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
                .header("Host", "localhost:8000")
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
                .header("Host", "localhost:8000")
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
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }

        #[tokio::test]
        async fn get_request_with_invalid_session_returns_not_found() {
            let service = create_test_service(true);
            let response = service
                .oneshot(build_get_request(Some("non-existent-session"), None))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
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

    mod peer_cleanup {
        use std::sync::Arc;

        use axum::body::Body;
        use http::{Request, StatusCode};
        use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
        use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
        use serde_json::json;
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
                prompts: vec![],
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
                annotations: HashMap::new(),
                health_check: None,
                server_info: Default::default(),
                instructions: None,
                rhai_engine: Arc::new(parking_lot::Mutex::new(RhaiEngine::new("rhai"))),
            }
        }

        fn create_service(
            running: Running,
            session_manager: Arc<LocalSessionManager>,
        ) -> StreamableHttpService<Running, LocalSessionManager> {
            StreamableHttpService::new(
                move || Ok(running.clone()),
                session_manager,
                StreamableHttpServerConfig::default().with_legacy_session_mode(true),
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
                .header("Host", "localhost:8000")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .body(Body::from(body.to_string()))
                .unwrap()
        }

        fn build_delete_request(session_id: &str) -> Request<Body> {
            Request::builder()
                .method("DELETE")
                .uri("/mcp")
                .header("Host", "localhost:8000")
                .header("Mcp-Session-Id", session_id)
                .body(Body::empty())
                .unwrap()
        }

        #[tokio::test]
        async fn closed_peers_are_cleaned_up_on_operations_update() {
            let running = create_test_running();
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();

            // Initialize a session — this adds a peer to running.peers
            let service = create_service(running.clone(), Arc::clone(&session_manager));
            let response = service.oneshot(build_initialize_request()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let session_id = response
                .headers()
                .get("mcp-session-id")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();

            // Poll until the peer is registered by the async initialize handler
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                if running.peers.read().await.len() == 1 {
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "peer should be registered after initialize"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            // Delete the session — closes the session transport
            let service = create_service(running.clone(), Arc::clone(&session_manager));
            let response = service
                .oneshot(build_delete_request(&session_id))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);

            // Poll until the transport is fully closed
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                let guard = running.peers.read().await;
                if guard.first().is_some_and(|p| p.is_transport_closed()) {
                    break;
                }
                drop(guard);
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "peer transport should be closed after session delete"
                );
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            // Trigger update_operations which calls notify_tool_list_changed,
            // cleaning up the now-closed peer
            running.update_operations(vec![]).await;

            assert_eq!(
                running.peers.read().await.len(),
                0,
                "closed peer should be removed after update_operations"
            );
        }
    }

    mod logging_setlevel {
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

        fn create_test_running() -> Running {
            let schema =
                apollo_compiler::Schema::parse_and_validate("type Query { hello: String }", "test")
                    .unwrap();
            Running {
                schema: Arc::new(RwLock::new(schema)),
                operations: Arc::new(RwLock::new(vec![])),
                apps: vec![],
                prompts: vec![],
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
                enable_output_schema: false,
                disable_auth_token_passthrough: false,
                descriptions: HashMap::new(),
                annotations: HashMap::new(),
                health_check: None,
                server_info: Default::default(),
                instructions: None,
                rhai_engine: Arc::new(parking_lot::Mutex::new(RhaiEngine::new("rhai"))),
            }
        }

        fn create_service(
            running: Running,
            session_manager: Arc<LocalSessionManager>,
        ) -> StreamableHttpService<Running, LocalSessionManager> {
            StreamableHttpService::new(
                move || Ok(running.clone()),
                session_manager,
                StreamableHttpServerConfig::default().with_legacy_session_mode(true),
            )
        }

        fn build_initialize_request() -> Request<Body> {
            let body = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "test-client", "version": "1.0.0" }
                }
            });
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Host", "localhost:8000")
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
                .header("Host", "localhost:8000")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .header("Mcp-Session-Id", session_id)
                .body(Body::from(body.to_string()))
                .unwrap()
        }

        fn build_set_level_request(session_id: &str, level: &str) -> Request<Body> {
            let body = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "logging/setLevel",
                "params": { "level": level }
            });
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Host", "localhost:8000")
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
        ) -> String {
            let service = create_service(running.clone(), Arc::clone(session_manager));
            let response = service.oneshot(build_initialize_request()).await.unwrap();
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

        #[tokio::test]
        async fn returns_empty_success_for_any_level() {
            let running = create_test_running();
            let session_manager: Arc<LocalSessionManager> = LocalSessionManager::default().into();
            let session_id = initialize_session(&running, &session_manager).await;

            let service = create_service(running, session_manager);
            let response = service
                .oneshot(build_set_level_request(&session_id, "debug"))
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            let body = extract_json_body(response).await;
            assert!(
                body.get("error").is_none(),
                "set_level should not return an error: {body}"
            );
            assert_eq!(body["result"], json!({}));
        }
    }

    /// `server/discover`: called before any other request, without a session.
    mod discover {
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
        use crate::server_info::ServerInfoConfig;

        /// Discovery has no top-level `serverInfo`; it lives in `_meta`.
        const SERVER_INFO_META_KEY: &str = "io.modelcontextprotocol/serverInfo";

        fn create_test_running(server_info: ServerInfoConfig) -> Running {
            let schema =
                apollo_compiler::Schema::parse_and_validate("type Query { hello: String }", "test")
                    .unwrap();
            Running {
                schema: Arc::new(RwLock::new(schema)),
                operations: Arc::new(RwLock::new(vec![])),
                apps: vec![],
                prompts: vec![],
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
                enable_output_schema: false,
                disable_auth_token_passthrough: false,
                descriptions: HashMap::new(),
                annotations: HashMap::new(),
                health_check: None,
                server_info,
                instructions: None,
                rhai_engine: Arc::new(parking_lot::Mutex::new(RhaiEngine::new("rhai"))),
            }
        }

        fn create_service(
            running: Running,
            session_manager: Arc<LocalSessionManager>,
        ) -> StreamableHttpService<Running, LocalSessionManager> {
            StreamableHttpService::new(
                move || Ok(running.clone()),
                session_manager,
                StreamableHttpServerConfig::default().with_legacy_session_mode(true),
            )
        }

        /// Carries no session id, so `_meta` and the `MCP-Protocol-Version`
        /// header have to be self-contained.
        fn build_discover_request(protocol_version: &str) -> Request<Body> {
            let body = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "server/discover",
                "params": {
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": protocol_version,
                        "io.modelcontextprotocol/clientCapabilities": {},
                        "io.modelcontextprotocol/clientInfo": {
                            "name": "test-client",
                            "version": "1.0.0"
                        }
                    }
                }
            });
            Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("Host", "localhost:8000")
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .header("MCP-Protocol-Version", protocol_version)
                // SEP-2243 standard header.
                .header("Mcp-Method", "server/discover")
                .body(Body::from(body.to_string()))
                .unwrap()
        }

        async fn discover(running: Running) -> serde_json::Value {
            let service = create_service(running, LocalSessionManager::default().into());
            let response = service
                .oneshot(build_discover_request(
                    MAX_SUPPORTED_PROTOCOL_VERSION.as_str(),
                ))
                .await
                .unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let body_str = String::from_utf8_lossy(&bytes);
            let body: serde_json::Value = body_str
                .lines()
                .find_map(|line| serde_json::from_str(line.strip_prefix("data: ")?).ok())
                .or_else(|| serde_json::from_str(&body_str).ok())
                .unwrap_or_else(|| panic!("no JSON data found in response: {body_str}"));
            assert_eq!(status, StatusCode::OK, "discover failed: {body}");
            body["result"].clone()
        }

        #[tokio::test]
        async fn returns_expected_result_shape() {
            let running = create_test_running(ServerInfoConfig {
                description: Some("Custom fleet description".to_string()),
                ..Default::default()
            });
            let expected_capabilities = serde_json::to_value(running.get_info().capabilities)
                .expect("capabilities serialize");

            let result = discover(running).await;

            assert_eq!(result["resultType"], "complete");
            // `initialize` returns `get_info()` untouched but for the
            // negotiated version, so this is the capability set both report.
            assert_eq!(result["capabilities"], expected_capabilities);
            assert!(result["ttlMs"].is_u64(), "ttlMs must be a number: {result}");
            assert!(
                result["cacheScope"].is_string(),
                "cacheScope must be a string: {result}"
            );

            let server_info = &result["_meta"][SERVER_INFO_META_KEY];
            assert_eq!(server_info["description"], "Custom fleet description");
            assert_eq!(server_info["name"], "Apollo MCP Server");
            assert!(
                server_info["version"].is_string(),
                "serverInfo must carry a version: {server_info}"
            );
        }

        #[tokio::test]
        async fn advertises_backward_compatible_protocol_versions() {
            let result = discover(create_test_running(ServerInfoConfig::default())).await;

            let versions: Vec<&str> = result["supportedVersions"]
                .as_array()
                .expect("supportedVersions must be an array")
                .iter()
                .map(|version| version.as_str().expect("versions are strings"))
                .collect();

            assert!(
                versions.contains(&"2025-11-25"),
                "must advertise 2025-11-25 for backward compat: {versions:?}"
            );
            // rmcp reuses this list as the ceiling on `initialize`
            // negotiation, so advertising a version promises we can serve it.
            assert!(
                !versions
                    .iter()
                    .any(|version| *version > MAX_SUPPORTED_PROTOCOL_VERSION.as_str()),
                "must not advertise past our cap: {versions:?}"
            );
        }

        /// stdio has no headers to negotiate with, so discovery as the opening
        /// message is the only compatibility probe available there.
        #[tokio::test]
        async fn works_as_opener_over_stdio() {
            use rmcp::ServiceExt as _;
            use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

            let (server_io, client_io) = tokio::io::duplex(4096);
            let (server_r, server_w) = tokio::io::split(server_io);
            let (client_r, mut client_w) = tokio::io::split(client_io);

            let server = tokio::spawn(async move {
                let service = create_test_running(ServerInfoConfig::default())
                    .serve((server_r, server_w))
                    .await
                    .expect("stdio serve should accept a discover opener");
                let _ = service.waiting().await;
            });

            let mut request = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "server/discover",
                "params": {
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion":
                            MAX_SUPPORTED_PROTOCOL_VERSION.as_str(),
                        "io.modelcontextprotocol/clientCapabilities": {},
                    }
                }
            })
            .to_string();
            request.push('\n');
            client_w.write_all(request.as_bytes()).await.unwrap();

            let mut reader = BufReader::new(client_r);
            let mut response_line = String::new();
            reader.read_line(&mut response_line).await.unwrap();
            let body: serde_json::Value = serde_json::from_str(&response_line).unwrap();

            assert!(
                body.get("error").is_none(),
                "discover over stdio returned an error: {body}"
            );
            let versions = body["result"]["supportedVersions"]
                .as_array()
                .expect("supportedVersions must be an array");
            assert!(versions.iter().any(|version| version == "2025-11-25"));

            drop(reader);
            drop(client_w);
            let _ = server.await;
        }
    }
}
