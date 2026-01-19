use std::path::Path;
use std::{net::SocketAddr, sync::Arc};

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
                        self.config.enable_output_schema,
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
        let apps = if cfg!(feature = "apps") {
            crate::apps::load_from_path(
                Path::new("apps"),
                &self.schema,
                self.config.custom_scalar_map.as_ref(),
                self.config.mutation_mode,
                self.config.disable_type_description,
                self.config.disable_schema_description,
                self.config.enable_output_schema,
            )
            .map_err(ServerError::Apps)?
        } else {
            Vec::new()
        };
        let schema = Arc::new(RwLock::new(self.schema));
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
            operations: Arc::new(RwLock::new(operations)),
            apps,
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
            health_check: health_check.clone(),
        };

        // Helper to enable auth
        macro_rules! with_auth {
            ($router:expr, $auth:ident) => {{
                let mut router = $router;
                if let Some(auth) = $auth {
                    match auth.enable_middleware(router) {
                        Ok(r) => router = r,
                        Err(e) => {
                            error!("Failed to enable auth middleware: {}", e);
                            return Err(e.into());
                        }
                    }
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
                .layer(axum::middleware::from_fn(otel_context_middleware));

                // Add health check endpoint if configured
                if let Some(health_check) = health_check.filter(|h| h.config().enabled) {
                    router = with_cors!(health_check.enable_router(router), self.config.cors);
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
                auth: _,
                address: _,
                port: _,
            } => {
                // SSE transport has been removed in rmcp 0.11+
                // Users should migrate to streamable_http transport
                return Err(ServerError::UnsupportedTransport(
                    "SSE transport is no longer supported. Please use streamable_http transport instead. \
                     Update your config to use `transport: { type: streamable_http }`.".to_string()
                ));
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
                enable_output_schema: false,
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

    #[tokio::test]
    async fn start_sse_server_returns_unsupported_error() {
        let starting = Starting {
            config: Config {
                transport: Transport::SSE {
                    auth: None,
                    address: "127.0.0.1".parse().unwrap(),
                    port: 7798,
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
                enable_output_schema: false,
                disable_auth_token_passthrough: false,
                search_leaf_depth: 5,
                index_memory_bytes: 1024 * 1024 * 1024,
                health_check: HealthCheckConfig::default(),
                cors: Default::default(),
            },
            schema: Schema::parse_and_validate("type Query { hello: String }", "test.graphql")
                .expect("Valid schema"),
            operations: vec![],
        };
        let result = starting.start().await;
        match result {
            Err(ServerError::UnsupportedTransport(msg)) => {
                assert!(msg.contains("SSE transport is no longer supported"));
            }
            Err(e) => panic!("Expected UnsupportedTransport error, got: {:?}", e),
            Ok(_) => panic!("Expected error, got Ok"),
        }
    }
}
