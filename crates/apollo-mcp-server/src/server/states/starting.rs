use std::{net::SocketAddr, sync::Arc};

use apollo_compiler::{Name, Schema, ast::OperationType, validation::Valid};
use reqwest::header::HeaderMap;
use rmcp::{
    ServiceExt as _,
    transport::{
        SseServer, StreamableHttpServer, sse_server::SseServerConfig, stdio,
        streamable_http_server::axum::StreamableHttpServerConfig,
    },
};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::{
    custom_scalar_map::CustomScalarMap,
    errors::{OperationError, ServerError},
    explorer::Explorer,
    introspection::tools::{execute::Execute, introspect::Introspect},
    operations::{MutationMode, Operation, RawOperation},
    server::Transport,
};

use super::Running;

pub(super) struct Starting {
    pub(super) transport: Transport,
    pub(super) schema: Valid<Schema>,
    pub(super) operations: Vec<RawOperation>,
    pub(super) endpoint: String,
    pub(super) headers: HeaderMap,
    pub(super) introspection: bool,
    pub(super) explorer_graph_ref: Option<String>,
    pub(super) custom_scalar_map: Option<CustomScalarMap>,
    pub(super) mutation_mode: MutationMode,
    pub(super) disable_type_description: bool,
    pub(super) disable_schema_description: bool,
}

impl Starting {
    pub(super) async fn start(self) -> Result<Running, ServerError> {
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

        let explorer_tool = self.explorer_graph_ref.map(Explorer::new);

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
                StreamableHttpServer::serve_with_config(StreamableHttpServerConfig {
                    bind: listen_address,
                    path: "/mcp".to_string(),
                    ct: cancellation_token,
                    sse_keep_alive: None,
                })
                .await?
                .with_service(move || running.clone());
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
