use std::path::Path;
use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use apollo_compiler::{Name, Schema, ast::OperationType, validation::Valid};
use axum_otel_metrics::HttpMetricsLayerBuilder;
use axum_tracing_opentelemetry::middleware::OtelInResponseLayer;
use futures::future::try_join_all;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ServiceExt as _, transport::stdio};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use crate::server::states::telemetry::otel_context_middleware;
use crate::{
    auth,
    cors::CorsConfig,
    errors::ServerError,
    explorer::Explorer,
    health::HealthCheck,
    introspection::tools::{
        execute::Execute, introspect::Introspect, search::Search, validate::Validate,
    },
    operations::{MutationMode, RawOperation},
    scope_requirements::OperationRequiredScopes,
    server::Transport,
};
use apollo_mcp_rhai::{SharedRhaiEngine, checkpoints};

use super::{Config, Running, shutdown_signal};

pub(super) struct Starting {
    pub(super) config: Config,
    pub(super) schema: Valid<Schema>,
    pub(super) operations: Vec<RawOperation>,
}

impl Starting {
    pub(super) async fn start(mut self) -> Result<Running, ServerError> {
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

        let engine = SharedRhaiEngine::load(&self.config.rhai_dir).map_err(|err| {
            error!("Error loading Rhai scripts: {err}");
            ServerError::RhaiError
        })?;

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
            tool_list_changes: Default::default(),
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
            caching: self.config.caching,
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
                let service = build_http_service(running, stateful_mode, &host_validation);
                let health_check = health_check.filter(|h| h.config().enabled);

                let listeners =
                    ListenerBuilder::new(listen_address, service, self.config.cors.clone())
                        .with_auth(auth, self.config.required_scopes.clone(), stateful_mode)?
                        .with_cors()?
                        .with_telemetry()
                        .with_health(health_check.as_ref())?;

                // Bind every listener before spawning any serving task, so a later bind
                // failure can't leave an earlier listener running unsupervised.
                let listeners =
                    try_join_all(listeners.into_iter().map(UnboundListener::bind)).await?;

                for BoundListener {
                    socket,
                    router,
                    label,
                } in listeners
                {
                    let cancellation_token = cancellation_token.clone();
                    tokio::spawn(async move {
                        if let Err(e) =
                            serve_http(socket, router, cancellation_token, shutdown_signal()).await
                        {
                            error!("Failed to start {label}: {e:?}");
                        }
                    });
                }
            }
            Transport::Stdio {} => {
                info!("Starting MCP server in stdio mode");
                let service = running
                    .for_service()
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

/// Wrap every route registered so far in the telemetry layers.
///
/// Routes added after this call — the health check — stay untraced on purpose,
/// so probes don't show up as service entry points.
pub(super) fn with_telemetry_layers(router: axum::Router) -> axum::Router {
    router
        .layer(HttpMetricsLayerBuilder::new().build())
        // include trace context as header into the response
        .layer(OtelInResponseLayer)
        // start OpenTelemetry trace on incoming request
        .layer(axum::middleware::from_fn(otel_context_middleware))
}

/// Construct the production transport, including cancellation of open streams.
pub(super) fn build_http_service(
    running: Running,
    stateful_mode: bool,
    host_validation: &crate::host_validation::HostValidationConfig,
) -> StreamableHttpService<super::running::McpService, LocalSessionManager> {
    let config = host_validation.apply_to(
        StreamableHttpServerConfig::default()
            .with_legacy_session_mode(stateful_mode)
            .with_cancellation_token(running.cancellation_token.child_token()),
    );
    StreamableHttpService::new(
        move || Ok(running.for_service()),
        LocalSessionManager::default().into(),
        config,
    )
}

/// Both shutdown sources cancel transport streams before Axum drains connections.
pub(super) async fn serve_http(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    shutdown: CancellationToken,
    signal: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = signal => {},
                _ = shutdown.cancelled() => {},
            }
            shutdown.cancel();
        })
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

/// One HTTP listener to serve, not yet bound to a socket.
struct UnboundListener {
    address: SocketAddr,
    router: axum::Router,
    label: &'static str,
}

/// One HTTP listener to serve, bound to a live socket.
struct BoundListener {
    socket: tokio::net::TcpListener,
    router: axum::Router,
    label: &'static str,
}

impl UnboundListener {
    async fn bind(self) -> Result<BoundListener, ServerError> {
        let socket = tokio::net::TcpListener::bind(self.address)
            .await
            .map_err(|e| ServerError::Bind(self.address, e))?;
        Ok(BoundListener {
            socket,
            router: self.router,
            label: self.label,
        })
    }
}

/// Builds the main MCP router for the Streamable HTTP transport, applying auth, CORS,
/// telemetry, and the health check in turn, before resolving into the listener(s) it needs.
struct ListenerBuilder {
    router: axum::Router,
    listen_address: SocketAddr,
    cors: CorsConfig,
}

impl ListenerBuilder {
    fn new(
        listen_address: SocketAddr,
        service: StreamableHttpService<super::running::McpService, LocalSessionManager>,
        cors: CorsConfig,
    ) -> Self {
        Self {
            router: axum::Router::new().nest_service("/mcp", service),
            listen_address,
            cors,
        }
    }

    fn with_auth(
        mut self,
        auth: Option<Box<auth::Config>>,
        required_scopes: HashMap<String, OperationRequiredScopes>,
        stateful_mode: bool,
    ) -> Result<Self, ServerError> {
        if let Some(auth) = auth {
            self.router = auth
                .enable_middleware(self.router, required_scopes, stateful_mode)
                .inspect_err(|e| {
                    error!("Failed to enable auth middleware: {}", e);
                })?;
        }
        Ok(self)
    }

    fn with_cors(mut self) -> Result<Self, ServerError> {
        self.router = with_cors(self.router, &self.cors)?;
        Ok(self)
    }

    fn with_telemetry(mut self) -> Self {
        self.router = with_telemetry_layers(self.router);
        self
    }

    /// Adds the health check endpoint: merged into the main router by default, or served on
    /// its own listener when `health_check.listen` is configured, mirroring how the Apollo
    /// Router exposes a separate health-check listen address.
    fn with_health(
        self,
        health_check: Option<&HealthCheck>,
    ) -> Result<Vec<UnboundListener>, ServerError> {
        let Some(health_check) = health_check else {
            return Ok(vec![UnboundListener {
                address: self.listen_address,
                router: self.router,
                label: "MCP server",
            }]);
        };
        let listeners = match health_check.config().listen {
            None => {
                let router = with_cors(health_check.enable_router(self.router), &self.cors)?;
                vec![UnboundListener {
                    address: self.listen_address,
                    router,
                    label: "MCP server",
                }]
            }
            Some(health_listen_addr) => {
                let health_router = with_cors(health_check.router(), &self.cors)?;
                vec![
                    UnboundListener {
                        address: self.listen_address,
                        router: self.router,
                        label: "MCP server",
                    },
                    UnboundListener {
                        address: health_listen_addr,
                        router: health_router,
                        label: "MCP server health check listener",
                    },
                ]
            }
        };
        Ok(listeners)
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http::HeaderMap;
    use tower::ServiceExt;
    use url::Url;

    use crate::health::HealthCheckConfig;
    use crate::host_validation::HostValidationConfig;

    use super::*;

    fn config(port: u16, health_check: HealthCheckConfig) -> Config {
        Config {
            rhai_dir: std::path::PathBuf::from("rhai"),
            transport: Transport::StreamableHttp {
                auth: None,
                address: "127.0.0.1".parse().unwrap(),
                port,
                stateful_mode: false,
                host_validation: HostValidationConfig::default(),
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
            execute_tool_hint: None,
            introspect_tool_hint: None,
            search_tool_hint: None,
            validate_tool_hint: None,
            explorer_graph_ref: None,
            custom_scalar_map: None,
            disable_type_description: false,
            disable_schema_description: false,
            enable_output_schema: false,
            disable_auth_token_passthrough: false,
            descriptions: std::collections::HashMap::new(),
            annotations: std::collections::HashMap::new(),
            required_scopes: std::collections::HashMap::new(),
            search_leaf_depth: 5,
            index_memory_bytes: 1024 * 1024 * 1024,
            health_check,
            cors: Default::default(),
            server_info: Default::default(),
            instructions: None,
            caching: Default::default(),
        }
    }

    fn starting(config: Config) -> Starting {
        Starting {
            config,
            schema: Schema::parse_and_validate("type Query { hello: String }", "test.graphql")
                .expect("Valid schema"),
            operations: vec![],
        }
    }

    #[tokio::test]
    async fn start_basic_server() {
        let starting = starting(config(
            7799,
            HealthCheckConfig {
                enabled: true,
                ..Default::default()
            },
        ));
        let running = starting.start();
        assert!(running.await.is_ok());
    }

    #[tokio::test]
    async fn health_check_bind_failure_leaves_no_listener_running() {
        // Configuring the health check to listen on the same address as the main transport
        // guarantees the second bind fails, once the first has already claimed the port.
        let addr: SocketAddr = "127.0.0.1:7802".parse().unwrap();
        let starting = starting(config(
            7802,
            HealthCheckConfig {
                enabled: true,
                listen: Some(addr),
                ..Default::default()
            },
        ));

        let err = starting.start().await.err().unwrap();
        assert!(matches!(err, ServerError::Bind(bound_addr, _) if bound_addr == addr));

        // If the main listener had been left running (the bug this guards against), binding a
        // fresh listener on the same port here would fail with "address in use".
        assert!(tokio::net::TcpListener::bind(addr).await.is_ok());
    }

    #[tokio::test]
    async fn with_health_merges_into_main_router_when_listen_unset() {
        let health_check = HealthCheck::new(HealthCheckConfig {
            enabled: true,
            ..Default::default()
        });
        let listeners = ListenerBuilder {
            router: axum::Router::new(),
            listen_address: "127.0.0.1:0".parse().unwrap(),
            cors: CorsConfig::default(),
        }
        .with_health(Some(&health_check))
        .unwrap();

        assert_eq!(listeners.len(), 1);
        let UnboundListener { router, label, .. } = listeners.into_iter().next().unwrap();
        assert_eq!(label, "MCP server");

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn with_health_serves_separately_when_listen_set() {
        let main_addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let health_addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let health_check = HealthCheck::new(HealthCheckConfig {
            enabled: true,
            listen: Some(health_addr),
            ..Default::default()
        });

        let mut listeners = ListenerBuilder {
            router: axum::Router::new(),
            listen_address: main_addr,
            cors: CorsConfig::default(),
        }
        .with_health(Some(&health_check))
        .unwrap();
        assert_eq!(listeners.len(), 2);

        let health = listeners.pop().unwrap();
        let main = listeners.pop().unwrap();

        assert_eq!(main.address, main_addr);
        assert_eq!(main.label, "MCP server");
        assert_eq!(health.address, health_addr);
        assert_eq!(health.label, "MCP server health check listener");

        // The main router no longer serves /health once it's split out.
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let res = main.router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        // The health listener's own router serves it instead.
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let res = health.router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
