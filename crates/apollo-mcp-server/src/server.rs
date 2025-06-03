use crate::custom_scalar_map::CustomScalarMap;
use crate::errors::{McpError, OperationError, ServerError};
use crate::event::Event as ServerEvent;
use crate::graphql;
use crate::graphql::Executable;
use crate::introspection::{EXECUTE_TOOL_NAME, Execute, INTROSPECT_TOOL_NAME, Introspect};
use crate::operations::{MutationMode, Operation, OperationSource, RawOperation};
use apollo_compiler::ast::OperationType;
use bon::bon;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use rmcp::model::{
    CallToolRequestParam, CallToolResult, ErrorCode, InitializeRequestParam, InitializeResult,
    ListToolsResult, PaginatedRequestParam, ServerCapabilities, ServerInfo,
};
use rmcp::serde_json::Value;
use rmcp::service::RequestContext;
use rmcp::{Peer, RoleServer, ServerHandler, ServiceError, serde_json};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tracing::{debug, error, info};

use crate::explorer::{EXPLORER_TOOL_NAME, Explorer};
use apollo_compiler::validation::Valid;
use apollo_compiler::{Name, Schema};
use apollo_federation::{ApiSchemaOptions, Supergraph};
pub use apollo_mcp_registry::uplink::UplinkConfig;
pub use apollo_mcp_registry::uplink::persisted_queries::ManifestSource;
pub use apollo_mcp_registry::uplink::schema::SchemaSource;
use apollo_mcp_registry::uplink::schema::SchemaState;
use apollo_mcp_registry::uplink::schema::event::Event as SchemaEvent;
use futures::{FutureExt, Stream, StreamExt, stream};
pub use rmcp::ServiceExt;
pub use rmcp::transport::SseServer;
pub use rmcp::transport::sse_server::SseServerConfig;
pub use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

/// An Apollo MCP Server
pub struct Server {
    transport: Transport,
    schema_source: SchemaSource,
    operation_source: OperationSource,
    endpoint: String,
    headers: HeaderMap,
    introspection: bool,
    explorer: bool,
    custom_scalar_map: Option<CustomScalarMap>,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
}

#[derive(Clone)]
pub enum Transport {
    Stdio,
    SSE { address: IpAddr, port: u16 },
    StreamableHttp { address: IpAddr, port: u16 },
}

#[bon]
impl Server {
    #[builder]
    pub fn new(
        transport: Transport,
        schema_source: SchemaSource,
        operation_source: OperationSource,
        endpoint: String,
        headers: HeaderMap,
        introspection: bool,
        explorer: bool,
        #[builder(required)] custom_scalar_map: Option<CustomScalarMap>,
        mutation_mode: MutationMode,
        disable_type_description: bool,
        disable_schema_description: bool,
    ) -> Self {
        let headers = {
            let mut headers = headers.clone();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            headers
        };
        Self {
            transport,
            schema_source,
            operation_source,
            endpoint,
            headers,
            introspection,
            explorer,
            custom_scalar_map,
            mutation_mode,
            disable_type_description,
            disable_schema_description,
        }
    }

    pub async fn start(self) -> Result<(), ServerError> {
        StateMachine {}.start(self).await
    }
}

#[allow(clippy::large_enum_variant)]
enum State {
    Configuring(Configuring),
    SchemaConfigured(SchemaConfigured),
    OperationsConfigured(OperationsConfigured),
    Starting(Starting),
    Running(Running),
    Error(ServerError),
    Stopping,
}

impl From<Configuring> for State {
    fn from(starting: Configuring) -> Self {
        State::Configuring(starting)
    }
}

impl From<SchemaConfigured> for State {
    fn from(schema_configured: SchemaConfigured) -> Self {
        State::SchemaConfigured(schema_configured)
    }
}

impl From<Result<SchemaConfigured, ServerError>> for State {
    fn from(result: Result<SchemaConfigured, ServerError>) -> Self {
        match result {
            Ok(schema_configured) => State::SchemaConfigured(schema_configured),
            Err(error) => State::Error(error),
        }
    }
}

impl From<OperationsConfigured> for State {
    fn from(operations_configured: OperationsConfigured) -> Self {
        State::OperationsConfigured(operations_configured)
    }
}

impl From<Result<OperationsConfigured, ServerError>> for State {
    fn from(result: Result<OperationsConfigured, ServerError>) -> Self {
        match result {
            Ok(operations_configured) => State::OperationsConfigured(operations_configured),
            Err(error) => State::Error(error),
        }
    }
}

impl From<Starting> for State {
    fn from(starting: Starting) -> Self {
        State::Starting(starting)
    }
}

impl From<Result<Starting, ServerError>> for State {
    fn from(result: Result<Starting, ServerError>) -> Self {
        match result {
            Ok(starting) => State::Starting(starting),
            Err(error) => State::Error(error),
        }
    }
}

impl From<Running> for State {
    fn from(running: Running) -> Self {
        State::Running(running)
    }
}

impl From<Result<Running, ServerError>> for State {
    fn from(result: Result<Running, ServerError>) -> Self {
        match result {
            Ok(running) => State::Running(running),
            Err(error) => State::Error(error),
        }
    }
}

impl From<ServerError> for State {
    fn from(error: ServerError) -> Self {
        State::Error(error)
    }
}

struct Configuring {
    transport: Transport,
    endpoint: String,
    headers: HeaderMap,
    introspection: bool,
    explorer: bool,
    custom_scalar_map: Option<CustomScalarMap>,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
}

impl Configuring {
    async fn set_schema(self, schema: Valid<Schema>) -> Result<SchemaConfigured, ServerError> {
        debug!("Received schema:\n{}", schema);
        Ok(SchemaConfigured {
            transport: self.transport,
            schema,
            endpoint: self.endpoint,
            headers: self.headers,
            introspection: self.introspection,
            explorer: self.explorer,
            custom_scalar_map: self.custom_scalar_map,
            mutation_mode: self.mutation_mode,
            disable_type_description: self.disable_type_description,
            disable_schema_description: self.disable_schema_description,
        })
    }

    async fn set_operations(
        self,
        operations: Vec<RawOperation>,
    ) -> Result<OperationsConfigured, ServerError> {
        debug!(
            "Received {} operations:\n{}",
            operations.len(),
            serde_json::to_string_pretty(&operations)?
        );
        Ok(OperationsConfigured {
            transport: self.transport,
            operations,
            endpoint: self.endpoint,
            headers: self.headers,
            introspection: self.introspection,
            explorer: self.explorer,
            custom_scalar_map: self.custom_scalar_map,
            mutation_mode: self.mutation_mode,
            disable_type_description: self.disable_type_description,
            disable_schema_description: self.disable_schema_description,
        })
    }
}

struct SchemaConfigured {
    transport: Transport,
    schema: Valid<Schema>,
    endpoint: String,
    headers: HeaderMap,
    introspection: bool,
    explorer: bool,
    custom_scalar_map: Option<CustomScalarMap>,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
}

impl SchemaConfigured {
    async fn set_schema(self, schema: Valid<Schema>) -> Result<SchemaConfigured, ServerError> {
        debug!("Received schema:\n{}", schema);
        Ok(SchemaConfigured { schema, ..self })
    }

    async fn set_operations(self, operations: Vec<RawOperation>) -> Result<Starting, ServerError> {
        debug!(
            "Received {} operations:\n{}",
            operations.len(),
            serde_json::to_string_pretty(&operations)?
        );
        Ok(Starting {
            transport: self.transport,
            schema: self.schema,
            operations,
            endpoint: self.endpoint,
            headers: self.headers,
            introspection: self.introspection,
            explorer: self.explorer,
            custom_scalar_map: self.custom_scalar_map,
            mutation_mode: self.mutation_mode,
            disable_type_description: self.disable_type_description,
            disable_schema_description: self.disable_schema_description,
        })
    }
}

struct OperationsConfigured {
    transport: Transport,
    operations: Vec<RawOperation>,
    endpoint: String,
    headers: HeaderMap,
    introspection: bool,
    explorer: bool,
    custom_scalar_map: Option<CustomScalarMap>,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
}

impl OperationsConfigured {
    async fn set_schema(self, schema: Valid<Schema>) -> Result<Starting, ServerError> {
        debug!("Received schema:\n{}", schema);
        Ok(Starting {
            transport: self.transport,
            schema,
            operations: self.operations,
            endpoint: self.endpoint,
            headers: self.headers,
            introspection: self.introspection,
            explorer: self.explorer,
            custom_scalar_map: self.custom_scalar_map,
            mutation_mode: self.mutation_mode,
            disable_type_description: self.disable_type_description,
            disable_schema_description: self.disable_schema_description,
        })
    }

    async fn set_operations(
        self,
        operations: Vec<RawOperation>,
    ) -> Result<OperationsConfigured, ServerError> {
        debug!(
            "Received {} operations:\n{}",
            operations.len(),
            serde_json::to_string_pretty(&operations)?
        );
        Ok(OperationsConfigured { operations, ..self })
    }
}

struct Starting {
    transport: Transport,
    schema: Valid<Schema>,
    operations: Vec<RawOperation>,
    endpoint: String,
    headers: HeaderMap,
    introspection: bool,
    explorer: bool,
    custom_scalar_map: Option<CustomScalarMap>,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
}

impl Starting {
    async fn start(self) -> Result<Running, ServerError> {
        let peers = Arc::new(RwLock::new(Vec::new()));

        let operations: Vec<_> = self
            .operations
            .into_iter()
            .map(|operation| {
                operation.into_operation(
                    &self.schema,
                    self.custom_scalar_map.as_ref(),
                    self.mutation_mode,
                    self.disable_type_description,
                    self.disable_schema_description,
                )
            })
            .collect::<Result<Vec<Option<Operation>>, OperationError>>()?
            .into_iter()
            .flatten()
            .collect();

        debug!(
            "Loaded {} operations:\n{}",
            operations.len(),
            serde_json::to_string_pretty(&operations)?
        );

        let execute_tool = self.introspection.then(|| Execute::new(self.mutation_mode));

        let root_query_type = self
            .introspection
            .then(|| {
                self.schema
                    .root_operation(OperationType::Query)
                    .map(Name::as_str)
                    .map(|s| s.to_string())
            })
            .flatten();
        let root_mutation_type = self
            .introspection
            .then(|| {
                matches!(self.mutation_mode, MutationMode::All)
                    .then(|| {
                        self.schema
                            .root_operation(OperationType::Mutation)
                            .map(Name::as_str)
                            .map(|s| s.to_string())
                    })
                    .flatten()
            })
            .flatten();
        let schema = Arc::new(Mutex::new(self.schema));
        let introspect_tool = self
            .introspection
            .then(|| Introspect::new(schema.clone(), root_query_type, root_mutation_type));

        let explorer_tool = self
            .explorer
            .then(|| std::env::var("APOLLO_GRAPH_REF").ok())
            .flatten()
            .map(Explorer::new);

        let cancellation_token = CancellationToken::new();

        let running = Running {
            schema,
            operations: Arc::new(Mutex::new(operations)),
            headers: self.headers,
            endpoint: self.endpoint,
            execute_tool,
            introspect_tool,
            explorer_tool,
            custom_scalar_map: self.custom_scalar_map,
            peers,
            cancellation_token: cancellation_token.clone(),
            mutation_mode: self.mutation_mode,
            disable_type_description: self.disable_type_description,
            disable_schema_description: self.disable_schema_description,
        };

        match self.transport {
            Transport::StreamableHttp { address, port } => {
                info!(port = ?port, address = ?address, "Starting MCP server in Streamable HTTP mode");
                let running = running.clone();
                let listen_address = SocketAddr::new(address, port);
                let service = StreamableHttpService::new(
                    move || running.clone(),
                    LocalSessionManager::default().into(),
                    StreamableHttpServerConfig {
                        sse_keep_alive: None,
                        stateful_mode: true,
                    },
                );
                let router = axum::Router::new().nest_service("/mcp", service);
                let tcp_listener = tokio::net::TcpListener::bind(listen_address).await?;
                axum::serve(tcp_listener, router)
                    .with_graceful_shutdown(shutdown_signal())
                    .await?;
            }
            Transport::SSE { address, port } => {
                info!(port = ?port, address = ?address, "Starting MCP server in SSE mode");
                let running = running.clone();
                let listen_address = SocketAddr::new(address, port);
                SseServer::serve_with_config(SseServerConfig {
                    bind: listen_address,
                    sse_path: "/sse".to_string(),
                    post_path: "/message".to_string(),
                    ct: cancellation_token,
                    sse_keep_alive: None,
                })
                .await?
                .with_service(move || running.clone());
            }
            Transport::Stdio => {
                info!("Starting MCP server in stdio mode");
                let service = running.clone().serve(stdio()).await.inspect_err(|e| {
                    error!("serving error: {:?}", e);
                })?;
                service.waiting().await.map_err(ServerError::StartupError)?;
            }
        }

        Ok(running)
    }
}

#[derive(Clone)]
struct Running {
    schema: Arc<Mutex<Valid<Schema>>>,
    operations: Arc<Mutex<Vec<Operation>>>,
    headers: HeaderMap,
    endpoint: String,
    execute_tool: Option<Execute>,
    introspect_tool: Option<Introspect>,
    explorer_tool: Option<Explorer>,
    custom_scalar_map: Option<CustomScalarMap>,
    peers: Arc<RwLock<Vec<Peer<RoleServer>>>>,
    cancellation_token: CancellationToken,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
}

impl Running {
    /// Update a running server with a new schema.
    async fn update_schema(self, schema: Valid<Schema>) -> Result<Running, ServerError> {
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
            .map(|operation| {
                operation.into_operation(
                    &schema,
                    self.custom_scalar_map.as_ref(),
                    self.mutation_mode,
                    self.disable_type_description,
                    self.disable_schema_description,
                )
            })
            .collect::<Result<Vec<Option<Operation>>, OperationError>>()?
            .into_iter()
            .flatten()
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

    async fn update_operations(
        self,
        operations: Vec<RawOperation>,
    ) -> Result<Running, ServerError> {
        // Update the operations based on the current schema
        {
            let schema = &*self.schema.lock().await;
            let updated_operations: Vec<Operation> = operations
                .into_iter()
                .map(|operation| {
                    operation.into_operation(
                        schema,
                        self.custom_scalar_map.as_ref(),
                        self.mutation_mode,
                        self.disable_type_description,
                        self.disable_schema_description,
                    )
                })
                .collect::<Result<Vec<Option<Operation>>, OperationError>>()?
                .into_iter()
                .flatten()
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
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if request.name == INTROSPECT_TOOL_NAME {
            self.introspect_tool
                .as_ref()
                .ok_or(tool_not_found(&request.name))?
                .execute(convert_arguments(request)?)
                .await
        } else if request.name == EXPLORER_TOOL_NAME {
            self.explorer_tool
                .as_ref()
                .ok_or(tool_not_found(&request.name))?
                .execute(convert_arguments(request)?)
                .await
        } else {
            let graphql_request = graphql::Request {
                input: Value::from(request.arguments.clone()),
                endpoint: &self.endpoint,
                headers: self.headers.clone(),
            };
            if request.name == EXECUTE_TOOL_NAME {
                self.execute_tool
                    .as_ref()
                    .ok_or(tool_not_found(&request.name))?
                    .execute(graphql_request)
                    .await
            } else {
                self.operations
                    .lock()
                    .await
                    .iter()
                    .find(|op| op.as_ref().name == request.name)
                    .ok_or(tool_not_found(&request.name))?
                    .execute(graphql_request)
                    .await
            }
        }
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
                .chain(
                    self.execute_tool
                        .as_ref()
                        .iter()
                        .clone()
                        .map(|e| e.tool.clone()),
                )
                .chain(
                    self.introspect_tool
                        .as_ref()
                        .iter()
                        .clone()
                        .map(|e| e.tool.clone()),
                )
                .chain(
                    self.explorer_tool
                        .as_ref()
                        .iter()
                        .clone()
                        .map(|e| e.tool.clone()),
                )
                .collect(),
        })
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo {
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
        format!("Tool {} not found", name),
        None,
    )
}

fn convert_arguments<T: serde::de::DeserializeOwned>(
    arguments: CallToolRequestParam,
) -> Result<T, McpError> {
    serde_json::from_value(Value::from(arguments.arguments))
        .map_err(|_| McpError::new(ErrorCode::INVALID_PARAMS, "Invalid input".to_string(), None))
}

struct StateMachine {}

impl StateMachine {
    pub(crate) async fn start(self, server: Server) -> Result<(), ServerError> {
        let schema_stream = server
            .schema_source
            .into_stream()
            .map(ServerEvent::SchemaUpdated)
            .boxed();
        let operation_stream = server.operation_source.into_stream().await.boxed();
        let ctrl_c_stream = Self::ctrl_c_stream().boxed();
        let mut stream = stream::select_all(vec![schema_stream, operation_stream, ctrl_c_stream]);

        let mut state = State::Configuring(Configuring {
            transport: server.transport,
            endpoint: server.endpoint,
            headers: server.headers,
            introspection: server.introspection,
            explorer: server.explorer,
            custom_scalar_map: server.custom_scalar_map,
            mutation_mode: server.mutation_mode,
            disable_type_description: server.disable_type_description,
            disable_schema_description: server.disable_schema_description,
        });

        while let Some(event) = stream.next().await {
            state = match event {
                ServerEvent::SchemaUpdated(registry_event) => match registry_event {
                    SchemaEvent::UpdateSchema(schema_state) => {
                        let schema = Self::sdl_to_api_schema(schema_state)?;
                        match state {
                            State::Configuring(configuring) => {
                                configuring.set_schema(schema).await.into()
                            }
                            State::SchemaConfigured(schema_configured) => {
                                schema_configured.set_schema(schema).await.into()
                            }
                            State::OperationsConfigured(operations_configured) => {
                                operations_configured.set_schema(schema).await.into()
                            }
                            State::Running(running) => running.update_schema(schema).await.into(),
                            other => other,
                        }
                    }
                    SchemaEvent::NoMoreSchema => match state {
                        State::Configuring(_) | State::OperationsConfigured(_) => {
                            State::Error(ServerError::NoSchema)
                        }
                        _ => state,
                    },
                },
                ServerEvent::OperationsUpdated(operations) => match state {
                    State::Configuring(configuring) => {
                        configuring.set_operations(operations).await.into()
                    }
                    State::SchemaConfigured(schema_configured) => {
                        schema_configured.set_operations(operations).await.into()
                    }
                    State::OperationsConfigured(operations_configured) => operations_configured
                        .set_operations(operations)
                        .await
                        .into(),
                    State::Running(running) => running.update_operations(operations).await.into(),
                    other => other,
                },
                ServerEvent::OperationError(e) => {
                    State::Error(ServerError::Operation(OperationError::File(e)))
                }
                ServerEvent::Shutdown => match state {
                    State::Running(running) => {
                        running.cancellation_token.cancel();
                        State::Stopping
                    }
                    _ => State::Stopping,
                },
            };
            if let State::Starting(starting) = state {
                state = starting.start().await.into();
            }
            if matches!(&state, State::Error(_) | State::Stopping) {
                break;
            }
        }
        match state {
            State::Error(e) => Err(e),
            _ => Ok(()),
        }
    }

    #[allow(clippy::result_large_err)]
    fn sdl_to_api_schema(schema_state: SchemaState) -> Result<Valid<Schema>, ServerError> {
        match Supergraph::new(&schema_state.sdl) {
            Ok(supergraph) => Ok(supergraph
                .to_api_schema(ApiSchemaOptions::default())
                .map_err(ServerError::Federation)?
                .schema()
                .clone()),
            Err(_) => Schema::parse_and_validate(schema_state.sdl, "schema.graphql")
                .map_err(|e| ServerError::GraphQLSchema(e.into())),
        }
    }

    #[allow(clippy::expect_used)]
    fn ctrl_c_stream() -> impl Stream<Item = ServerEvent> {
        shutdown_signal()
            .map(|_| ServerEvent::Shutdown)
            .into_stream()
            .boxed()
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install CTRL+C signal handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
