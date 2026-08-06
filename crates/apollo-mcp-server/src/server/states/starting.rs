use std::{future::Future, net::SocketAddr, path::Path, sync::Arc};

use apollo_compiler::{Name, Schema, ast::OperationType, validation::Valid};
use axum_otel_metrics::HttpMetricsLayerBuilder;
use axum_tracing_opentelemetry::middleware::OtelInResponseLayer;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ServiceExt as _, transport::stdio};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::server::states::telemetry::otel_context_middleware;
use crate::{
    cors::CorsConfig,
    errors::ServerError,
    explorer::Explorer,
    health::HealthCheck,
    introspection::tools::{
        execute::Execute, introspect::Introspect, search::Search, validate::Validate,
    },
    operations::{MutationMode, RawOperation},
    server::Transport,
};
use apollo_mcp_rhai::{RhaiEngine, checkpoints};

use super::{Config, Running, shutdown_signal};

pub(super) struct Starting {
    pub(super) config: Config,
    pub(super) schema: Valid<Schema>,
    pub(super) operations: Vec<RawOperation>,
}

impl Starting {
    pub(super) async fn start(mut self) -> Result<Running, ServerError> {
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
                        self.config.enable_output_schema,
                        &self.config.annotations,
                        &self.config.descriptions,
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

        let execute_tool = self.config.execute_introspection.then(|| {
            Execute::new(
                self.config.mutation_mode,
                self.config.execute_tool_hint.as_deref(),
            )
        });

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
        let apps = crate::apps::load_from_path(
            Path::new("apps"),
            &self.schema,
            self.config.custom_scalar_map.as_ref(),
            self.config.mutation_mode,
            self.config.disable_type_description,
            self.config.disable_schema_description,
            self.config.enable_output_schema,
        )
        .map_err(ServerError::Apps)?;
        let prompts =
            crate::prompts::load_from_path(Path::new("prompts")).map_err(ServerError::Prompts)?;
        let schema = Arc::new(RwLock::new(self.schema));
        let introspect_tool = self.config.introspect_introspection.then(|| {
            Introspect::new(
                schema.clone(),
                root_query_type,
                root_mutation_type,
                self.config.introspect_minify,
                self.config.introspect_tool_hint.as_deref(),
            )
        });
        let validate_tool = self
            .config
            .validate_introspection
            .then(|| Validate::new(schema.clone(), self.config.validate_tool_hint.as_deref()));
        let search_tool = if self.config.search_introspection {
            Some(Search::new(
                schema.clone(),
                matches!(self.config.mutation_mode, MutationMode::All),
                self.config.search_leaf_depth,
                self.config.index_memory_bytes,
                self.config.search_minify,
                self.config.search_tool_hint.as_deref(),
            )?)
        } else {
            None
        };

        let explorer_tool = self.config.explorer_graph_ref.map(Explorer::new);

        let cancellation_token = CancellationToken::new();

        // Create health checks only when StreamableHttp transport is enabled.
        let health_check = match (&self.config.transport, self.config.health_check.enabled) {
            (Transport::StreamableHttp { .. }, true) => {
                Some(HealthCheck::new(self.config.health_check.clone()))
            }
            _ => None, // No health checks for Stdio or when disabled.
        };

        let mut engine = RhaiEngine::new(&self.config.rhai_dir);
        engine.load_from_path().map_err(|err| {
            error!("Error loading Rhai scripts: {err}");
            ServerError::RhaiError
        })?;
        let engine = Arc::new(parking_lot::Mutex::new(engine));

        if cfg!(feature = "experimental_rhai") {
            checkpoints::on_startup(&engine).map_err(|err| {
                error!("Error when executing on_startup hook: {err}");
                ServerError::RhaiError
            })?;
        }

        // Move into `Running` so we do not clone the full string (`config.instructions` is not read afterward).
        let instructions = std::mem::take(&mut self.config.instructions);

        let running = Running {
            schema,
            operations: Arc::new(RwLock::new(operations)),
            apps,
            prompts,
            headers: self.config.headers,
            forward_headers: self.config.forward_headers.clone(),
            endpoint: self.config.endpoint,
            execute_tool,
            introspect_tool,
            search_tool,
            explorer_tool,
            validate_tool,
            custom_scalar_map: self.config.custom_scalar_map,
            peers,
            cancellation_token: cancellation_token.clone(),
            mutation_mode: self.config.mutation_mode,
            disable_type_description: self.config.disable_type_description,
            disable_schema_description: self.config.disable_schema_description,
            enable_output_schema: self.config.enable_output_schema,
            disable_auth_token_passthrough: self.config.disable_auth_token_passthrough,
            descriptions: self.config.descriptions,
            annotations: self.config.annotations,
            health_check: health_check.clone(),
            server_info: self.config.server_info.clone(),
            instructions,
            rhai_engine: engine,
        };

        match self.config.transport {
            Transport::StreamableHttp {
                auth,
                address,
                port,
                stateful_mode,
                host_validation,
            } => {
                info!(port = ?port, address = ?address, "Starting MCP server in Streamable HTTP mode");
                let running = running.clone();
                let listen_address = SocketAddr::new(address, port);
                let http_config = host_validation.apply_to(
                    StreamableHttpServerConfig::default().with_stateful_mode(stateful_mode),
                );
                let service = StreamableHttpService::new(
                    move || Ok(running.clone()),
                    LocalSessionManager::default().into(),
                    http_config,
                );
                let mut router = axum::Router::new().nest_service("/mcp", service);
                if let Some(auth) = auth {
                    router = auth
                        .enable_middleware(router, self.config.required_scopes.clone())
                        .inspect_err(|e| {
                            error!("Failed to enable auth middleware: {}", e);
                        })?;
                }
                let mut router = with_cors(router, &self.config.cors)?
                    .layer(HttpMetricsLayerBuilder::new().build())
                    // include trace context as header into the response
                    .layer(OtelInResponseLayer)
                    // start OpenTelemetry trace on incoming request
                    .layer(axum::middleware::from_fn(otel_context_middleware));

                // Add health check endpoint if configured
                if let Some(health_check) = health_check.filter(|h| h.config().enabled) {
                    router = with_cors(health_check.enable_router(router), &self.config.cors)?;
                }

                let tcp_listener = tokio::net::TcpListener::bind(listen_address).await?;
                let shutdown_token = cancellation_token.clone();
                tokio::spawn(async move {
                    // Shut down when either a signal (CTRL+C/SIGTERM) is received
                    // or the cancellation token is cancelled (e.g., config change restart).
                    let graceful_shutdown = async move {
                        tokio::select! {
                            _ = shutdown_signal() => {},
                            _ = shutdown_token.cancelled() => {},
                        }
                    };
                    // Health check is already active from creation
                    // Expose the peer socket address so middleware (e.g. the
                    // peer-IP rate limiter) can key on the connection address.
                    if let Err(e) = serve_http(tcp_listener, router, graceful_shutdown).await {
                        // This can never really happen
                        error!("Failed to start MCP server: {e:?}");
                    }
                });
            }
            Transport::Stdio {} => {
                info!("Starting MCP server in stdio mode");
                let service = running
                    .clone()
                    .serve(stdio())
                    .await
                    .inspect_err(|e| {
                        error!("serving error: {:?}", e);
                    })
                    .map_err(Box::new)?;
                service.waiting().await.map_err(ServerError::StartupError)?;
            }
        }

        Ok(running)
    }
}

async fn serve_http(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    graceful_shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(graceful_shutdown)
    .await
}

fn with_cors(router: axum::Router, config: &CorsConfig) -> Result<axum::Router, ServerError> {
    if config.enabled {
        let cors_layer = config.build_cors_layer().inspect_err(|e| {
            error!("Failed to build CORS layer: {}", e);
        })?;
        Ok(router.layer(cors_layer))
    } else {
        Ok(router)
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use axum::{extract::ConnectInfo, routing::get};

    use super::*;

    #[tokio::test]
    async fn streamable_http_supplies_connect_info() {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let cancellation = CancellationToken::new();
        let router = axum::Router::new().route(
            "/",
            get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
        );
        let server = tokio::spawn(serve_http(
            listener,
            router,
            cancellation.clone().cancelled_owned(),
        ));

        let body = reqwest::get(format!("http://{address}"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        cancellation.cancel();
        server.await.unwrap().unwrap();

        assert_eq!(body, Ipv4Addr::LOCALHOST.to_string());
    }
}
