use crate::custom_scalar_map::CustomScalarMap;
use crate::errors::{McpError, OperationError, ServerError};
use crate::graphql;
use crate::graphql::Executable;
use crate::introspection::{EXECUTE_TOOL_NAME, Execute, INTROSPECT_TOOL_NAME, Introspect};
use crate::operations::{MutationMode, Operation, OperationPoller, OperationSource};
use apollo_compiler::ast::OperationType;
use buildstructor::buildstructor;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use rmcp::model::{
    CallToolRequestParam, CallToolResult, ErrorCode, ListToolsResult, PaginatedRequestParam,
    ServerCapabilities, ServerInfo,
};
use rmcp::serde_json::Value;
use rmcp::service::RequestContext;
use rmcp::{Peer, RoleServer, ServerHandler, ServiceError, serde_json};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tracing::{error, info};

use crate::explorer::{EXPLORER_TOOL_NAME, Explorer};
use apollo_compiler::validation::Valid;
use apollo_compiler::{Name, Schema};
use apollo_federation::{ApiSchemaOptions, Supergraph};
pub use apollo_mcp_registry::uplink::UplinkConfig;
use apollo_mcp_registry::uplink::event::Event;
pub use apollo_mcp_registry::uplink::persisted_queries::ManifestSource;
use apollo_mcp_registry::uplink::persisted_queries::{
    ManifestChanged, PersistedQueryManifestPoller,
};
pub use apollo_mcp_registry::uplink::schema::SchemaSource;
use apollo_mcp_registry::uplink::schema::SchemaState;
use futures::{FutureExt, Stream, StreamExt, future, stream};
pub use rmcp::ServiceExt;
pub use rmcp::transport::SseServer;
pub use rmcp::transport::sse_server::SseServerConfig;
pub use rmcp::transport::stdio;
use tokio::sync::{Mutex, RwLock, mpsc};
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
}

/// Types ending with Map cause incorrect assumptions by buildstructor, so use an alias
type Headers = HeaderMap;

#[buildstructor]
impl Server {
    #[builder]
    pub fn new(
        transport: Transport,
        schema_source: SchemaSource,
        operation_source: OperationSource,
        endpoint: String,
        headers: Headers,
        introspection: bool,
        explorer: bool,
        custom_scalar_map: Option<CustomScalarMap>,
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
    Starting(Starting),
    Running(Running),
    Error(ServerError),
    Stopping,
}

impl From<Starting> for State {
    fn from(starting: Starting) -> Self {
        State::Starting(starting)
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

struct Starting {
    transport: Transport,
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

impl Starting {
    /// Run the MCP server once the schema is ready
    async fn run(self, schema: Valid<Schema>) -> Result<Running, ServerError> {
        info!("Running with schema:\n{}", schema);

        let peers = Arc::new(RwLock::new(vec![]));

        let (operation_poller, change_receiver) = match self.operation_source {
            OperationSource::Files(paths) => (OperationPoller::Files(paths), None),
            OperationSource::Manifest(manifest_source) => {
                let (change_sender, change_receiver) = mpsc::channel::<ManifestChanged>(1);
                (
                    OperationPoller::Manifest(
                        PersistedQueryManifestPoller::new(manifest_source, change_sender)
                            .await
                            .map_err(|e| {
                                ServerError::Operation(OperationError::Internal(e.to_string()))
                            })?,
                    ),
                    Some(change_receiver),
                )
            }
            OperationSource::None => (OperationPoller::None, None),
        };
        let operations = operation_poller
            .operations(
                &schema,
                self.custom_scalar_map.as_ref(),
                self.mutation_mode,
                self.disable_type_description,
                self.disable_schema_description,
            )
            .await?;

        let execute_tool = self.introspection.then(|| Execute::new(self.mutation_mode));

        let root_query_type = self
            .introspection
            .then(|| {
                schema
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
                        schema
                            .root_operation(OperationType::Mutation)
                            .map(Name::as_str)
                            .map(|s| s.to_string())
                    })
                    .flatten()
            })
            .flatten();
        let schema = Arc::new(Mutex::new(schema));
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
            operation_poller,
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

        if let Some(change_receiver) = change_receiver {
            running.spawn_change_listener(change_receiver);
        }

        if let Transport::SSE { address, port } = self.transport {
            info!(port = ?port, address = ?address, "Starting MCP server in SSE mode");
            let running = running.clone();
            let listen_address = SocketAddr::new(address, port);
            SseServer::serve_with_config(SseServerConfig {
                bind: listen_address,
                sse_path: "/sse".to_string(),
                post_path: "/message".to_string(),
                ct: cancellation_token,
            })
            .await?
            .with_service(move || running.clone());
        } else {
            info!("Starting MCP server in stdio mode");
            let service = running.clone().serve(stdio()).await.inspect_err(|e| {
                error!("serving error: {:?}", e);
            })?;
            service.waiting().await.map_err(ServerError::StartupError)?;
        }

        Ok(running)
    }
}

#[derive(Clone)]
struct Running {
    schema: Arc<Mutex<Valid<Schema>>>,
    operations: Arc<Mutex<Vec<Operation>>>,
    operation_poller: OperationPoller,
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
        info!("Schema updated:\n{}", schema);

        // Update the operations based on the new schema. This is necessary because the MCP tool
        // input schemas and description are derived from the schema.
        let operations = self
            .operation_poller
            .operations(
                &schema,
                self.custom_scalar_map.as_ref(),
                self.mutation_mode,
                self.disable_type_description,
                self.disable_schema_description,
            )
            .await?;
        info!(
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

    /// Notify any peers that tools have changed. Drops unreachable peers from the list.
    async fn notify_tool_list_changed(peers: Arc<RwLock<Vec<Peer<RoleServer>>>>) {
        let mut peers = peers.write().await;
        if !peers.is_empty() {
            info!(
                "Persisted query manifest changed, notifying {} peers of tool change",
                peers.len()
            );
        }
        let mut retained_peers = Vec::new();
        for peer in peers.iter() {
            match peer.notify_tool_list_changed().await {
                Ok(_) => retained_peers.push(peer.clone()),
                Err(ServiceError::Transport(e)) if e.get_ref().is_some() => {
                    if e.to_string() == *"disconnected" {
                        // This always gets a "disconnected" error due to a bug in the SDK, but it actually works
                        retained_peers.push(peer.clone());
                    } else {
                        error!(
                            "Failed to notify peer of tool list change: {:?} - dropping peer",
                            e
                        );
                    }
                }
                Err(e) => {
                    error!("Failed to notify peer of tool list change {:?}", e);
                    retained_peers.push(peer.clone());
                }
            }
        }
        *peers = retained_peers;
    }

    /// Spawn a listener for any changes to the operation manifest.
    fn spawn_change_listener(&self, mut change_receiver: mpsc::Receiver<ManifestChanged>) {
        let peers = self.peers.clone();
        let operations = self.operations.clone();
        let operation_poller = self.operation_poller.clone();
        let custom_scalars = self.custom_scalar_map.clone();
        let schema = self.schema.clone();
        let mutation_mode = self.mutation_mode;
        let disable_type_description = self.disable_type_description;
        let disable_schema_description = self.disable_schema_description;
        tokio::spawn(async move {
            while change_receiver.recv().await.is_some() {
                match operation_poller
                    .operations(
                        &*schema.lock().await,
                        custom_scalars.as_ref(),
                        mutation_mode,
                        disable_type_description,
                        disable_schema_description,
                    )
                    .await
                {
                    Ok(new_operations) => *operations.lock().await = new_operations,
                    // TODO: ideally, we'd send the server to the error state here because it's
                    //  no longer configured correctly. To do this, we'd have to receive the PQ
                    //  updates through the same stream where schema changes are tracked. However,
                    //  the router code ported into apollo-mcp-registry does not work that way -
                    //  it has a separate PQ update mechanism. That will need to be rewritten, or
                    //  maybe there's some way to bridge the PQ changes into the same stream.
                    Err(e) => error!("Failed to update operations: {:?}", e),
                }

                Self::notify_tool_list_changed(peers.clone()).await;
            }
        });
    }
}

struct StateMachine {}

impl StateMachine {
    pub(crate) async fn start(self, server: Server) -> Result<(), ServerError> {
        let mut stream = stream::select_all(vec![
            server.schema_source.into_stream().boxed(),
            Self::ctrl_c_stream().boxed(),
        ])
        .take_while(|msg| future::ready(!matches!(msg, Event::Shutdown)))
        .chain(stream::iter(vec![Event::Shutdown]))
        .boxed();
        let mut state = State::Starting(Starting {
            transport: server.transport,
            operation_source: server.operation_source,
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
                Event::UpdateSchema(schema_state) => {
                    let schema = Self::sdl_to_api_schema(schema_state)?;
                    match state {
                        State::Starting(starting) => starting.run(schema).await.into(),
                        State::Running(running) => running.update_schema(schema).await.into(),
                        other => other,
                    }
                }
                Event::NoMoreSchema => match state {
                    State::Starting { .. } => State::Error(ServerError::NoSchema),
                    _ => state,
                },
                Event::Shutdown => match state {
                    State::Running(running) => {
                        running.cancellation_token.cancel();
                        State::Stopping
                    }
                    _ => State::Stopping,
                },
            };
            if matches!(&state, State::Error(_) | State::Stopping) {
                break;
            }
        }
        match state {
            State::Error(e) => Err(e),
            _ => Ok(()),
        }
    }

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
    fn ctrl_c_stream() -> impl Stream<Item = Event> {
        #[cfg(not(unix))]
        {
            async {
                tokio::signal::ctrl_c()
                    .await
                    .expect("Failed to install CTRL+C signal handler");
            }
            .map(|_| Event::Shutdown)
            .into_stream()
            .boxed()
        }

        #[cfg(unix)]
        future::select(
            tokio::signal::ctrl_c().map(|s| s.ok()).boxed(),
            async {
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("Failed to install SIGTERM signal handler")
                    .recv()
                    .await
            }
            .boxed(),
        )
        .map(|_| Event::Shutdown)
        .into_stream()
        .boxed()
    }
}

impl ServerHandler for Running {
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
        _request: PaginatedRequestParam,
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

    fn set_peer(&mut self, p: Peer<RoleServer>) {
        let peers = self.peers.clone();
        tokio::spawn(async move {
            let mut peers = peers.write().await;
            // TODO: we need a way to remove these! The Rust SDK seems to leek running servers
            //  forever - it never times them out or disconnects them.
            peers.push(p);
        });
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
