use std::{net::SocketAddr, sync::Arc};

use apollo_compiler::{Name, Schema, ast::OperationType, validation::Valid};
use rmcp::transport::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::{
    ServiceExt as _,
    transport::{SseServer, sse_server::SseServerConfig, stdio},
};
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::{
    errors::ServerError,
    explorer::Explorer,
    introspection::tools::{execute::Execute, introspect::Introspect, search::Search},
    operations::{MutationMode, RawOperation},
    server::Transport,
};

use super::{Config, Running, shutdown_signal};

pub(super) struct Starting {
    pub(super) config: Config,
    pub(super) schema: Valid<Schema>,
    pub(super) operations: Vec<RawOperation>,
}

impl Starting {
    pub(super) async fn start(self) -> Result<Running, ServerError> {
        let peers = Arc::new(RwLock::new(Vec::new()));

        let operations: Vec<_> = self
            .operations
            .into_iter()
            .filter_map(|operation| {
                operation
                    .into_operation(
                        &self.schema,
                        self.config.custom_scalar_map.as_ref(),
                        self.config.mutation_mode,
                        self.config.disable_type_description,
                        self.config.disable_schema_description,
                    )
                    .unwrap_or_else(|error| {
                        error!("Invalid operation: {}", error);
                        None
                    })
            })
            .collect();

        debug!(
            "Loaded {} operations:\n{}",
            operations.len(),
            serde_json::to_string_pretty(&operations)?
        );

        let execute_tool = self
            .config
            .execute_introspection
            .then(|| Execute::new(self.config.mutation_mode));

        let root_query_type = self
            .config
            .introspect_introspection
            .then(|| {
                self.schema
                    .root_operation(OperationType::Query)
                    .map(Name::as_str)
                    .map(|s| s.to_string())
            })
            .flatten();
        let root_mutation_type = self
            .config
            .introspect_introspection
            .then(|| {
                matches!(self.config.mutation_mode, MutationMode::All)
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
            .config
            .introspect_introspection
            .then(|| Introspect::new(schema.clone(), root_query_type, root_mutation_type));
        let search_tool = if self.config.search_introspection {
            Some(Search::new(
                schema.clone(),
                matches!(self.config.mutation_mode, MutationMode::All),
                self.config.index_memory_bytes,
            )?)
        } else {
            None
        };

        let explorer_tool = self.config.explorer_graph_ref.map(Explorer::new);

        let cancellation_token = CancellationToken::new();

        let running = Running {
            schema,
            operations: Arc::new(Mutex::new(operations)),
            headers: self.config.headers,
            endpoint: self.config.endpoint,
            execute_tool,
            introspect_tool,
            search_tool,
            explorer_tool,
            custom_scalar_map: self.config.custom_scalar_map,
            peers,
            cancellation_token: cancellation_token.clone(),
            mutation_mode: self.config.mutation_mode,
            disable_type_description: self.config.disable_type_description,
            disable_schema_description: self.config.disable_schema_description,
        };

        match self.config.transport {
            Transport::StreamableHttp { address, port } => {
                info!(port = ?port, address = ?address, "Starting MCP server in Streamable HTTP mode");
                let running = running.clone();
                let listen_address = SocketAddr::new(address, port);
                let service = StreamableHttpService::new(
                    move || Ok(running.clone()),
                    LocalSessionManager::default().into(),
                    Default::default(),
                );
                let router = axum::Router::new().nest_service("/mcp", service);
                let tcp_listener = tokio::net::TcpListener::bind(listen_address).await?;
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(tcp_listener, router)
                        .with_graceful_shutdown(shutdown_signal())
                        .await
                    {
                        // This can never really happen
                        error!("Failed to start MCP server: {e:?}");
                    }
                });
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
