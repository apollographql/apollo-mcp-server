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
use apollo_compiler::ast::OperationType;
use apollo_compiler::validation::Valid;
use apollo_compiler::{Name, Schema};
use headers::HeaderMapExt;
use http::HeaderMap;
use rmcp::model::{
    CallToolRequestParam, CallToolResult, ErrorCode, Implementation, InitializeRequestParam,
    InitializeResult, ListToolsResult, PaginatedRequestParam, ServerCapabilities, ServerInfo,
};
use rmcp::service::{RequestContext, ServiceRole};
use rmcp::{Peer, RoleServer, ServerHandler, ServiceError};
use serde_json::Value;
use std::ops::Deref;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, error};
use url::Url;

pub trait McpServerHandler: ServerHandler {
    async fn configure(
        &mut self,
        config: &ServerConfig,
        schema: Valid<Schema>,
    ) -> Result<(), ServerError>;
    async fn operations(&self) -> Vec<Operation>;
    async fn set_operations(&mut self, ops: Vec<Operation>);
    async fn headers(&self) -> HeaderMap;
    async fn endpoint(&self) -> Url;
    async fn execute_tool(&self) -> Option<Execute>;
    async fn introspect_tool(&self) -> Option<Introspect>;
    async fn search_tool(&self) -> Option<Search>;
    async fn explorer_tool(&self) -> Option<Explorer>;
    async fn validate_tool(&self) -> Option<Validate>;
    async fn peers(&self) -> Vec<Peer<RoleServer>>;
    fn notify_tool_list_changed(
        &mut self,
        peers: Vec<Peer<RoleServer>>,
    ) -> impl Future<Output = ()>;
}

struct ApolloMcpState {
    pub(super) operations: Vec<Operation>,
    pub(super) headers: HeaderMap,
    pub(super) endpoint: Url,
    pub(super) execute_tool: Option<Execute>,
    pub(super) introspect_tool: Option<Introspect>,
    pub(super) search_tool: Option<Search>,
    pub(super) explorer_tool: Option<Explorer>,
    pub(super) validate_tool: Option<Validate>,
    pub(super) peers: Vec<Peer<RoleServer>>,
}

#[derive(Clone)]
pub struct ApolloMcpServerHandler(Arc<RwLock<ApolloMcpState>>);

impl ApolloMcpServerHandler {
    pub fn new(headers: HeaderMap, endpoint: Url) -> ApolloMcpServerHandler {
        Self(
            Arc::new(
                RwLock::new(
                    ApolloMcpState {
                            operations: Vec::new(),
                            headers,
                            endpoint,
                            execute_tool: None,
                            introspect_tool: None,
                            search_tool: None,
                            explorer_tool: None,
                            validate_tool: None,
                            peers: Vec::new(),
                        }
                )
            )
        )
    }
}

impl McpServerHandler for ApolloMcpServerHandler {
    async fn configure(
        &mut self,
        config: &ServerConfig,
        schema: Valid<Schema>,
    ) -> Result<(), ServerError> {
        let root_query_type = config
            .introspect_enabled
            .then(|| {
                schema
                    .root_operation(OperationType::Query)
                    .map(Name::as_str)
                    .map(|s| s.to_string())
            })
            .flatten();
        let root_mutation_type = config
            .introspect_enabled
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
        let mut guard = self.0.write().await;
        guard.execute_tool = config
            .execute_enabled
            .then(|| Execute::new(config.mutation_mode));

        guard.introspect_tool = config.introspect_enabled.then(|| {
            Introspect::new(
                schema.clone(),
                root_query_type,
                root_mutation_type,
                config.introspect_minify,
            )
        });
        guard.validate_tool = config
            .validate_enabled
            .then(|| Validate::new(schema.clone()));

        guard.search_tool = if config.search_enabled {
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

        guard.explorer_tool = config.explorer_graph_ref.clone().map(Explorer::new);

        guard.peers = Vec::new();

        Ok(())
    }

    async fn operations(&self) -> Vec<Operation> {
        self.0.read().await.operations.clone()
    }

    async fn set_operations(&mut self, ops: Vec<Operation>) {
        let mut guard = self.0.write().await;
        guard.operations = ops;
    }

    async fn headers(&self) -> HeaderMap {
        self.0.read().await.headers.clone()
    }

    async fn endpoint(&self) -> Url {
        self.0.read().await.endpoint.clone()
    }

    async fn execute_tool(&self) -> Option<Execute> {
        self.0.read().await.execute_tool.clone()
    }

    async fn introspect_tool(&self) -> Option<Introspect> {
        self.0.read().await.introspect_tool.clone()
    }

    async fn search_tool(&self) -> Option<Search> {
        self.0.read().await.search_tool.clone()
    }

    async fn explorer_tool(&self) -> Option<Explorer> {
        self.0.read().await.explorer_tool.clone()
    }

    async fn validate_tool(&self) -> Option<Validate> {
        self.0.read().await.validate_tool.clone()
    }

    async fn peers(&self) -> Vec<Peer<RoleServer>> {
        self.0.read().await.peers.clone()
    }

    async fn notify_tool_list_changed(&mut self, peers: Vec<Peer<RoleServer>>) {
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
        let mut guard = self.0.write().await;
        guard.peers = retained_peers;
    }
}

impl ServerHandler for ApolloMcpServerHandler {
    async fn initialize(
        &self,
        _request: InitializeRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        // TODO: how to remove these?
        let mut guard = self.0.write().await;
        guard.peers.push(context.peer);
        Ok(self.get_info())
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.0.read().await;
        let result = match request.name.as_ref() {
            INTROSPECT_TOOL_NAME => {
                guard.introspect_tool
                    .as_ref()
                    .ok_or(tool_not_found(&request.name))?
                    .execute(convert_arguments(request)?)
                    .await
            }
            SEARCH_TOOL_NAME => {
                guard.search_tool
                    .as_ref()
                    .ok_or(tool_not_found(&request.name))?
                    .execute(convert_arguments(request)?)
                    .await
            }
            EXPLORER_TOOL_NAME => {
                guard.explorer_tool
                    .as_ref()
                    .ok_or(tool_not_found(&request.name))?
                    .execute(convert_arguments(request)?)
                    .await
            }
            EXECUTE_TOOL_NAME => {
                guard.execute_tool
                    .as_ref()
                    .ok_or(tool_not_found(&request.name))?
                    .execute(graphql::Request {
                        input: Value::from(request.arguments.clone()),
                        endpoint: &guard.endpoint,
                        headers: guard.headers.clone(),
                    })
                    .await
            }
            VALIDATE_TOOL_NAME => {
                guard.validate_tool
                    .as_ref()
                    .ok_or(tool_not_found(&request.name))?
                    .execute(convert_arguments(request)?)
                    .await
            }
            _ => {
                // Optionally extract the validated token and propagate it to upstream servers
                // if found
                let mut headers = guard.headers.clone();
                if let Some(token) = context.extensions.get::<ValidToken>() {
                    headers.typed_insert(token.deref().clone());
                }

                let graphql_request = graphql::Request {
                    input: Value::from(request.arguments.clone()),
                    endpoint: &guard.endpoint,
                    headers,
                };
                guard.operations
                    .iter()
                    .find(|op| op.as_ref().name == request.name)
                    .ok_or(tool_not_found(&request.name))?
                    .execute(graphql_request)
                    .await
            }
        };

        // Track errors for health check
        // if let (Err(_), Some(telemetry)) = (&result, &self.telemetry) {
        //     telemetry.record_error()
        // }

        result
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let guard = self.0.read().await;
        Ok(ListToolsResult {
            next_cursor: None,
            tools: guard
                .operations
                .iter()
                .cloned()
                .map(Into::into)
                .chain(guard.execute_tool.as_ref().iter().map(|e| e.tool.clone()))
                .chain(guard.introspect_tool.as_ref().iter().map(|e| e.tool.clone()))
                .chain(guard.search_tool.as_ref().iter().map(|e| e.tool.clone()))
                .chain(guard.explorer_tool.as_ref().iter().map(|e| e.tool.clone()))
                .chain(guard.validate_tool.as_ref().iter().map(|e| e.tool.clone()))
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
