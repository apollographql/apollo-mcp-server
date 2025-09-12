use rmcp::model::{
    CallToolRequestParam, CallToolResult, Content, GetPromptRequestParam, GetPromptResult,
    Implementation, InitializeRequestParam, InitializeResult, ListPromptsResult,
    ListResourceTemplatesResult, ListResourcesResult, ListToolsResult, PaginatedRequestParam,
    ReadResourceRequestParam, ReadResourceResult, ServerInfo,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{ErrorData, Peer, RoleClient, RoleServer, ServerHandler};
use std::sync::Arc;
use tracing::{debug, error, info};

pub struct ProxyServer {
    client: Arc<Peer<RoleClient>>,
    server_info: Arc<ServerInfo>,
}

impl ProxyServer {
    pub fn new(client_peer: Peer<RoleClient>, peer_info: Option<&ServerInfo>) -> Self {
        let mut server_info = ServerInfo::default();

        if let Some(info) = peer_info {
            server_info = ServerInfo {
                protocol_version: info.protocol_version.clone(),
                server_info: Implementation {
                    name: info.server_info.name.clone(),
                    title: None,
                    version: info.server_info.version.clone(),
                    icons: None,
                    website_url: None,
                },
                instructions: info.instructions.clone(),
                capabilities: info.capabilities.clone(),
            };
        }

        debug!("[Proxy]server info: {:?}", server_info);

        Self {
            client: Arc::new(client_peer),
            server_info: Arc::new(server_info),
        }
    }
}

impl ServerHandler for ProxyServer {
    async fn initialize(
        &self,
        _request: InitializeRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        if let Some(http_request_part) = context.extensions.get::<axum::http::request::Parts>() {
            let initialize_headers = &http_request_part.headers;
            let initialize_uri = &http_request_part.uri;
            info!(?initialize_headers, %initialize_uri, "initialize from http server");
        }

        Ok(self.get_info())
    }

    async fn complete(
        &self,
        request: rmcp::model::CompleteRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::CompleteResult, ErrorData> {
        match self.client.complete(request).await {
            Ok(result) => {
                debug!("[Proxy] Proxying complete response");
                Ok(result)
            }
            Err(err) => {
                error!("[Proxy] Error completing: {:?}", err);
                Err(ErrorData::internal_error(
                    format!("Error completing: {err}"),
                    None,
                ))
            }
        }
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        if self.server_info.capabilities.prompts.is_none() {
            error!("[Proxy] Server doesn't support the prompts capability");
            return Err(ErrorData::internal_error(
                "Server doesn't support the prompts capability".to_string(),
                None,
            ));
        }

        match self.client.get_prompt(request).await {
            Ok(result) => {
                debug!("[Proxy] Proxying get_prompt response");
                Ok(result)
            }
            Err(err) => {
                error!("[Proxy] Error getting prompt: {:?}", err);
                Err(ErrorData::internal_error(
                    format!("Error getting prompt: {err}"),
                    None,
                ))
            }
        }
    }

    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        if self.server_info.capabilities.prompts.is_none() {
            error!("[Proxy] Server doesn't support the prompts capability");
            return Err(ErrorData::internal_error(
                "Server doesn't support the prompts capability".to_string(),
                None,
            ));
        }

        match self.client.list_prompts(request).await {
            Ok(result) => {
                debug!("[Proxy] Proxying list_prompts response");
                Ok(result)
            }
            Err(err) => {
                error!("[Proxy] Error listing prompts: {:?}", err);
                Ok(ListPromptsResult::default())
            }
        }
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParam>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        if self.server_info.capabilities.resources.is_none() {
            error!("[Proxy] Server doesn't support the resources capability");
            return Err(ErrorData::internal_error(
                "Server doesn't support the resources capability".to_string(),
                None,
            ));
        }

        match self.client.list_resources(request).await {
            Ok(list_resources_result) => {
                debug!(
                    "Proxying list_resources response: {:?}",
                    list_resources_result
                );
                Ok(list_resources_result)
            }
            Err(e) => {
                error!("[Proxy] Error listing resources: {:?}", e);
                Ok(ListResourcesResult::default())
            }
        }
    }

    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        if self.server_info.capabilities.resources.is_none() {
            error!("[Proxy] Server doesn't support the resources capability");
            return Err(ErrorData::internal_error(
                "Server doesn't support the resources capability".to_string(),
                None,
            ));
        }

        match self.client.list_resource_templates(request).await {
            Ok(list_resource_templates_result) => {
                debug!(
                    "Proxying list_resource_templates response: {:?}",
                    list_resource_templates_result
                );
                Ok(list_resource_templates_result)
            }
            Err(err) => {
                error!("[Proxy] Error listing resource templates: {:?}", err);
                Ok(ListResourceTemplatesResult::default())
            }
        }
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        if self.server_info.capabilities.resources.is_none() {
            error!("[Proxy] Server doesn't support the resources capability");
            return Err(ErrorData::internal_error(
                "Server doesn't support the resources capability".to_string(),
                None,
            ));
        }

        match self
            .client
            .read_resource(ReadResourceRequestParam {
                uri: request.uri.clone(),
            })
            .await
        {
            Ok(result) => {
                debug!(
                    "Proxying read_resource response for {}: {:?}",
                    request.uri, result
                );
                Ok(result)
            }
            Err(err) => {
                error!("[Proxy] Error reading resource: {:?}", err);
                Err(ErrorData::internal_error(
                    format!("Error reading resource: {err}"),
                    None,
                ))
            }
        }
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if self.server_info.capabilities.tools.is_none() {
            error!("[Proxy] Server doesn't support the tools capability");
            return Err(ErrorData::internal_error(
                "Server doesn't support the tools capability".to_string(),
                None,
            ));
        }

        match self.client.call_tool(request.clone()).await {
            Ok(result) => {
                debug!("[Proxy] Tool call succeeded: {:?}", result);
                Ok(result)
            }
            Err(err) => {
                error!("[Proxy] Error calling tool: {:?}", err);
                Ok(CallToolResult::error(vec![Content::text(format!(
                    "Error: {err}"
                ))]))
            }
        }
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if self.server_info.capabilities.tools.is_none() {
            error!("[Proxy] Server doesn't support the tools capability");
            return Err(ErrorData::internal_error(
                "Server doesn't support the tools capability".to_string(),
                None,
            ));
        }

        match self.client.list_tools(request).await {
            Ok(result) => {
                debug!(
                    "Proxying list_tools response with {} tools: {:?}",
                    result.tools.len(),
                    result
                );
                Ok(result)
            }
            Err(err) => {
                error!("[Proxy] Error listing tools: {:?}", err);
                Ok(ListToolsResult::default())
            }
        }
    }

    async fn on_cancelled(
        &self,
        notification: rmcp::model::CancelledNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) {
        match self.client.notify_cancelled(notification).await {
            Ok(_) => {
                debug!("[Proxy] Proxying cancelled notification");
            }
            Err(err) => {
                error!("[Proxy] Error notifying cancelled: {:?}", err);
            }
        }
    }

    async fn on_progress(
        &self,
        notification: rmcp::model::ProgressNotificationParam,
        _context: NotificationContext<RoleServer>,
    ) {
        match self.client.notify_progress(notification).await {
            Ok(_) => {
                debug!("[Proxy] Proxying progress notification");
            }
            Err(err) => {
                error!("[Proxy] Error notifying progress: {:?}", err);
            }
        }
    }

    fn get_info(&self) -> ServerInfo {
        self.server_info.as_ref().clone()
    }
}
