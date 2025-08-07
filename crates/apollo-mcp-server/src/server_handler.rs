use crate::auth::ValidToken;
use crate::errors::{McpError, ServerError};
use crate::explorer::{EXPLORER_TOOL_NAME, Explorer};
use crate::graphql;
use crate::graphql::Executable;
use crate::introspection::tools::execute::{EXECUTE_TOOL_NAME, Execute};
use crate::introspection::tools::introspect::{INTROSPECT_TOOL_NAME, Introspect};
use crate::introspection::tools::search::{SEARCH_TOOL_NAME, Search};
use crate::introspection::tools::validate::{VALIDATE_TOOL_NAME, Validate};
use crate::operations::{MutationMode, Operation};
use crate::server_config::ServerConfig;
use crate::telemetry::Telemetry;
use apollo_compiler::ast::OperationType;
use apollo_compiler::validation::Valid;
use apollo_compiler::{Name, Schema};
use headers::HeaderMapExt;
use http::HeaderMap;
use rmcp::model::{
    CallToolRequestParam, CallToolResult, ErrorCode, Implementation, InitializeRequestParam,
    InitializeResult, ListToolsResult, PaginatedRequestParam, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{Peer, RoleServer, ServerHandler, ServiceError};
use serde_json::Value;
use std::ops::Deref;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error};
use url::Url;

#[derive(Clone)]
pub struct ApolloMcpServerHandler {
    pub(super) operations: Arc<Mutex<Vec<Operation>>>,
    pub(super) headers: HeaderMap,
    pub(super) endpoint: Url,
    pub(super) execute_tool: Option<Execute>,
    pub(super) introspect_tool: Option<Introspect>,
    pub(super) search_tool: Option<Search>,
    pub(super) explorer_tool: Option<Explorer>,
    pub(super) validate_tool: Option<Validate>,
    pub(super) peers: Arc<RwLock<Vec<Peer<RoleServer>>>>,
    pub(super) telemetry: Option<Arc<dyn Telemetry>>,
}

impl ApolloMcpServerHandler {
    pub fn new(
        headers: HeaderMap,
        endpoint: Url,
        telemetry: Option<Arc<dyn Telemetry>>,
    ) -> ApolloMcpServerHandler {
        Self {
            operations: Arc::new(Mutex::new(Vec::new())),
            headers,
            endpoint,
            execute_tool: None,
            introspect_tool: None,
            search_tool: None,
            explorer_tool: None,
            validate_tool: None,
            peers: Arc::new(RwLock::new(Vec::new())),
            telemetry,
        }
    }

    pub(crate) fn configure(
        &mut self,
        config: &ServerConfig,
        schema: Valid<Schema>,
    ) -> Result<(), ServerError> {
        let root_query_type = config
            .introspect_introspection
            .then(|| {
                schema
                    .root_operation(OperationType::Query)
                    .map(Name::as_str)
                    .map(|s| s.to_string())
            })
            .flatten();
        let root_mutation_type = config
            .introspect_introspection
            .then(|| {
                matches!(config.mutation_mode, MutationMode::All)
                    .then(|| {
                        schema
                            .root_operation(OperationType::Mutation)
                            .map(Name::as_str)
                            .map(|s| s.to_string())
                    })
                    .flatten()
            })
            .flatten();

        let schema = Arc::new(Mutex::new(schema));
        self.execute_tool = config
            .execute_introspection
            .then(|| Execute::new(config.mutation_mode));

        self.introspect_tool = config.introspect_introspection.then(|| {
            Introspect::new(
                schema.clone(),
                root_query_type,
                root_mutation_type,
                config.introspect_minify,
            )
        });
        self.validate_tool = config
            .validate_introspection
            .then(|| Validate::new(schema.clone()));

        self.search_tool = if config.search_introspection {
            Some(Search::new(
                schema.clone(),
                matches!(config.mutation_mode, MutationMode::All),
                config.search_leaf_depth,
                config.index_memory_bytes,
                config.search_minify,
            )?)
        } else {
            None
        };

        self.explorer_tool = config.explorer_graph_ref.clone().map(Explorer::new);

        self.peers = Arc::new(RwLock::new(Vec::new()));

        Ok(())
    }

    pub(crate) fn peers(&self) -> Arc<RwLock<Vec<Peer<RoleServer>>>> {
        Arc::clone(&self.peers)
    }

    pub(crate) fn operations(&self) -> Arc<Mutex<Vec<Operation>>> {
        Arc::clone(&self.operations)
    }

    pub(crate) async fn notify_tool_list_changed(&self, peers: Arc<RwLock<Vec<Peer<RoleServer>>>>) {
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

impl ServerHandler for ApolloMcpServerHandler {
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
                self.execute_tool
                    .as_ref()
                    .ok_or(tool_not_found(&request.name))?
                    .execute(graphql::Request {
                        input: Value::from(request.arguments.clone()),
                        endpoint: &self.endpoint,
                        headers: self.headers.clone(),
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
                // Optionally extract the validated token and propagate it to upstream servers
                // if found
                let mut headers = self.headers.clone();
                if let Some(token) = context.extensions.get::<ValidToken>() {
                    headers.typed_insert(token.deref().clone());
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
                    .await
            }
        };

        // Track errors for health check
        if let (Err(_), Some(telemetry)) = (&result, &self.telemetry) {
            telemetry.record_error()
        }

        result
    }

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
                version: env!("CARGO_PKG_VERSION").to_string(),
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
