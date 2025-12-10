use std::{net::SocketAddr, sync::Arc};

use apollo_compiler::{Name, Schema, ast::OperationType, validation::Valid};
use axum::{Router, extract::Query, http::StatusCode, response::Json, routing::get};
use axum_otel_metrics::HttpMetricsLayerBuilder;
use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{
    ServiceExt as _,
    transport::{SseServer, sse_server::SseServerConfig, stdio},
};
use serde_json::json;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{Instrument as _, debug, error, info, trace};

use crate::server::states::telemetry::otel_context_middleware;
use crate::{
    errors::ServerError,
    explorer::Explorer,
    health::HealthCheck,
    introspection::tools::{
        execute::Execute, introspect::Introspect, search::Search, validate::Validate,
    },
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
        let introspect_tool = self.config.introspect_introspection.then(|| {
            Introspect::new(
                schema.clone(),
                root_query_type,
                root_mutation_type,
                self.config.introspect_minify,
            )
        });
        let validate_tool = self
            .config
            .validate_introspection
            .then(|| Validate::new(schema.clone()));
        let search_tool = if self.config.search_introspection {
            Some(Search::new(
                schema.clone(),
                matches!(self.config.mutation_mode, MutationMode::All),
                self.config.search_leaf_depth,
                self.config.index_memory_bytes,
                self.config.search_minify,
            )?)
        } else {
            None
        };

        let explorer_tool = self.config.explorer_graph_ref.map(Explorer::new);

        let cancellation_token = CancellationToken::new();

        // Create health check if enabled (only for StreamableHttp transport)
        let health_check = match (&self.config.transport, self.config.health_check.enabled) {
            (
                Transport::StreamableHttp {
                    auth: _,
                    address: _,
                    port: _,
                    stateful_mode: _,
                },
                true,
            ) => Some(HealthCheck::new(self.config.health_check.clone())),
            _ => None, // No health check for SSE, Stdio, or when disabled
        };

        let running = Running {
            schema,
            operations: Arc::new(Mutex::new(operations)),
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
            disable_auth_token_passthrough: self.config.disable_auth_token_passthrough,
            health_check: health_check.clone(),
        };

        // Helper to enable auth
        macro_rules! with_auth {
            ($router:expr, $auth:ident) => {{
                let mut router = $router;
                if let Some(auth) = $auth {
                    router = auth.enable_middleware(router);
                }

                router
            }};
        }

        // Helper to enable CORS
        macro_rules! with_cors {
            ($router:expr, $config:expr) => {{
                let mut router = $router;
                if $config.enabled {
                    match $config.build_cors_layer() {
                        Ok(cors_layer) => {
                            router = router.layer(cors_layer);
                        }
                        Err(e) => {
                            error!("Failed to build CORS layer: {}", e);
                            return Err(e);
                        }
                    }
                }
                router
            }};
        }

        match self.config.transport {
            Transport::StreamableHttp {
                auth,
                address,
                port,
                stateful_mode,
            } => {
                info!(port = ?port, address = ?address, "Starting MCP server in Streamable HTTP mode");
                let running = running.clone();
                let listen_address = SocketAddr::new(address, port);
                let service = StreamableHttpService::new(
                    move || Ok(running.clone()),
                    LocalSessionManager::default().into(),
                    StreamableHttpServerConfig {
                        stateful_mode,
                        ..Default::default()
                    },
                );
                let mut router = with_cors!(
                    with_auth!(axum::Router::new().nest_service("/mcp", service), auth),
                    self.config.cors
                )
                .layer(HttpMetricsLayerBuilder::new().build())
                // include trace context as header into the response
                .layer(OtelInResponseLayer)
                // start OpenTelemetry trace on incoming request
                .layer(axum::middleware::from_fn(otel_context_middleware))
                .layer(OtelAxumLayer::default());

                // Add health check endpoint if configured
                if let Some(health_check) = health_check.filter(|h| h.config().enabled) {
                    let health_router = with_cors!(
                        Router::new()
                            .route(&health_check.config().path, get(health_endpoint))
                            .with_state(health_check.clone()),
                        self.config.cors
                    );
                    router = router.merge(health_router);
                }

                let tcp_listener = tokio::net::TcpListener::bind(listen_address).await?;
                tokio::spawn(async move {
                    // Health check is already active from creation
                    if let Err(e) = axum::serve(tcp_listener, router)
                        .with_graceful_shutdown(shutdown_signal())
                        .await
                    {
                        // This can never really happen
                        error!("Failed to start MCP server: {e:?}");
                    }
                });
            }
            Transport::SSE {
                auth,
                address,
                port,
            } => {
                info!(port = ?port, address = ?address, "Starting MCP server in SSE mode");
                let running = running.clone();
                let listen_address = SocketAddr::new(address, port);

                let (server, router) = SseServer::new(SseServerConfig {
                    bind: listen_address,
                    sse_path: "/sse".to_string(),
                    post_path: "/message".to_string(),
                    ct: cancellation_token,
                    sse_keep_alive: None,
                });

                // Optionally wrap the router with auth, if enabled
                let router = with_auth!(router, auth);

                // Start up the SSE server
                // Note: Until RMCP consolidates SSE with the same tower system as StreamableHTTP,
                // we need to basically copy the implementation of `SseServer::serve_with_config` here.
                let listener = tokio::net::TcpListener::bind(server.config.bind).await?;
                let ct = server.config.ct.child_token();
                let axum_server =
                    axum::serve(listener, router).with_graceful_shutdown(async move {
                        ct.cancelled().await;
                        tracing::info!("mcp server cancelled");
                    });

                tokio::spawn(
                    async move {
                        if let Err(e) = axum_server.await {
                            tracing::error!(error = %e, "mcp shutdown with error");
                        }
                    }
                    .instrument(
                        tracing::info_span!("mcp-server", bind_address = %server.config.bind),
                    ),
                );

                server.with_service(move || running.clone());
            }
            Transport::Stdio => {
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

/// Health check endpoint handler
async fn health_endpoint(
    axum::extract::State(health_check): axum::extract::State<HealthCheck>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    let query = params.keys().next().map(|k| k.as_str());
    let (health, status_code) = health_check.get_health_state(query);

    trace!(?health, query = ?query, "health check");

    Ok((status_code, Json(json!(health))))
}

#[cfg(test)]
mod tests {
    use http::HeaderMap;
    use url::Url;

    use crate::health::HealthCheckConfig;

    use super::*;

    #[tokio::test]
    async fn start_basic_server() {
        let starting = Starting {
            config: Config {
                transport: Transport::StreamableHttp {
                    auth: None,
                    address: "127.0.0.1".parse().unwrap(),
                    port: 7799,
                    stateful_mode: false,
                },
                endpoint: Url::parse("http://localhost:4000").expect("valid url"),
                mutation_mode: MutationMode::All,
                execute_introspection: true,
                headers: HeaderMap::new(),
                forward_headers: vec![],
                validate_introspection: true,
                introspect_introspection: true,
                search_introspection: true,
                introspect_minify: false,
                search_minify: false,
                explorer_graph_ref: None,
                custom_scalar_map: None,
                disable_type_description: false,
                disable_schema_description: false,
                disable_auth_token_passthrough: false,
                search_leaf_depth: 5,
                index_memory_bytes: 1024 * 1024 * 1024,
                health_check: HealthCheckConfig {
                    enabled: true,
                    ..Default::default()
                },
                cors: Default::default(),
            },
            schema: Schema::parse_and_validate("type Query { hello: String }", "test.graphql")
                .expect("Valid schema"),
            operations: vec![],
        };
        let running = starting.start();
        assert!(running.await.is_ok());
    }
}
