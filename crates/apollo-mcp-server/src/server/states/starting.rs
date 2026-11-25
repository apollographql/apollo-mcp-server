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

use crate::server::states::telemetry::{expose_client_address, otel_context_middleware};
use crate::{
    auth,
    cors::CorsConfig,
    errors::ServerError,
    explorer::Explorer,
    health::HealthCheck,
    introspection::tools::{
        execute::Execute, introspect::Introspect, search::Search, validate::Validate,
    },
    operations::{MutationMode, RawOperation, warn_app_tool_collisions, warn_unmatched_operations},
    scope_requirements::OperationRequiredScopes,
    server::{EndpointPath, Transport},
    trusted_proxies::TrustedProxies,
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
                        &self.config.param_headers,
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
        warn_unmatched_operations(&self.config.param_headers, &operations);

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
            &self.config.apps_dir,
            &self.schema,
            self.config.custom_scalar_map.as_ref(),
            self.config.mutation_mode,
            self.config.disable_type_description,
            self.config.disable_schema_description,
            self.config.enable_output_schema,
        )
        .map_err(ServerError::Apps)?;
        let exports = crate::apps::AppExports::resolve(&apps, &self.config.expose_on_connect)
            .map_err(ServerError::Apps)?;
        exports.warn_shadowed(&operations);
        warn_app_tool_collisions(&self.config.param_headers, &apps);
        let prompts = crate::prompts::load_from_path(&self.config.prompts_dir)
            .map_err(ServerError::Prompts)?;
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
            exports,
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
            gen_ai_content: self.config.gen_ai_content,
            disable_auth_token_passthrough: self.config.disable_auth_token_passthrough,
            descriptions: self.config.descriptions,
            annotations: self.config.annotations,
            param_headers: self.config.param_headers,
            health_check: health_check.clone(),
            server_info: self.config.server_info.clone(),
            instructions,
            rhai_engine: engine,
            caching: self.config.caching,
            tasks: self.config.tasks.enabled.then(|| {
                // With auth, a task belongs to the token identity that created it.
                let require_owner = matches!(
                    &self.config.transport,
                    Transport::StreamableHttp { auth: Some(_), .. }
                );
                super::tasks::TaskRuntime::new(
                    self.config.tasks,
                    cancellation_token.clone(),
                    require_owner,
                )
            }),
        };

        running.validate_exports().map_err(ServerError::Apps)?;

        match self.config.transport {
            Transport::StreamableHttp {
                mut auth,
                address,
                port,
                path,
                stateful_mode,
                host_validation,
                trusted_proxies,
            } => {
                info!(port = ?port, address = ?address, path = %path, "Starting MCP server in Streamable HTTP mode");
                if let Some(auth) = &mut auth {
                    let shadowed = auth
                        .skip_token_validation
                        .tools
                        .extract_if(.., |name| running.exports.get(name).is_some());
                    for tool in shadowed.collect::<std::collections::BTreeSet<_>>() {
                        tracing::warn!(%tool, "Tokenless exception in skip_token_validation.tools no longer applies to this exported app tool");
                    }
                }
                let running = running.clone();
                let per_client_rate_limit = auth
                    .as_ref()
                    .is_some_and(|auth| auth.rate_limit.limits_per_client());
                trusted_proxies
                    .warn_if_misconfigured(self.config.client_address || per_client_rate_limit);
                let listen_address = SocketAddr::new(address, port);
                let service = build_http_service(running, stateful_mode, &host_validation);
                let health_check = health_check.filter(|h| h.config().enabled);

                let listeners =
                    ListenerBuilder::new(listen_address, path, service, self.config.cors.clone())
                        .with_auth(
                            auth,
                            self.config.required_scopes.clone(),
                            stateful_mode,
                            &trusted_proxies,
                        )?
                        .with_cors()?
                        .with_telemetry(self.config.client_address, trusted_proxies)
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
                let result = service.waiting().await;
                if let Some(tasks) = &running.tasks {
                    tasks.shutdown().await;
                }
                result.map_err(ServerError::StartupError)?;
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

/// Expose who sent each request to the telemetry layers, which must already
/// wrap `router`, so spans record it as the client: the TCP peer, or the client
/// a trusted proxy names in `X-Forwarded-For`.
pub(super) fn with_client_address(
    router: axum::Router,
    trusted_proxies: TrustedProxies,
) -> axum::Router {
    router.layer(axum::middleware::from_fn_with_state(
        Arc::new(trusted_proxies),
        expose_client_address,
    ))
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
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
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

/// Mounts the MCP service at `path`. Only the root is an exact match; any other path also
/// routes its subpaths to the service.
pub(super) fn mount_endpoint(
    path: &EndpointPath,
    service: StreamableHttpService<super::running::McpService, LocalSessionManager>,
) -> axum::Router {
    if path.is_root() {
        // axum panics when nesting at the root. Streamable HTTP serves one URL, so an
        // exact route loses nothing.
        axum::Router::new().route_service("/", service)
    } else {
        axum::Router::new().nest_service(path.as_str(), service)
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
    path: EndpointPath,
    cors: CorsConfig,
}

impl ListenerBuilder {
    fn new(
        listen_address: SocketAddr,
        path: EndpointPath,
        service: StreamableHttpService<super::running::McpService, LocalSessionManager>,
        cors: CorsConfig,
    ) -> Self {
        Self {
            router: mount_endpoint(&path, service),
            listen_address,
            path,
            cors,
        }
    }

    fn with_auth(
        mut self,
        auth: Option<Box<auth::Config>>,
        required_scopes: HashMap<String, OperationRequiredScopes>,
        stateful_mode: bool,
        trusted_proxies: &TrustedProxies,
    ) -> Result<Self, ServerError> {
        if let Some(auth) = auth {
            self.router = auth
                .enable_middleware(self.router, required_scopes, stateful_mode, trusted_proxies)
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

    fn with_telemetry(mut self, client_address: bool, trusted_proxies: TrustedProxies) -> Self {
        self.router = with_telemetry_layers(self.router);
        if client_address {
            self.router = with_client_address(self.router, trusted_proxies);
        }
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
                let health_path = &health_check.config().path;
                if self.path.as_str() == health_path {
                    return Err(ServerError::HealthPathConflict {
                        health_path: health_path.clone(),
                        endpoint_path: self.path,
                    });
                }
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
            expose_on_connect: Default::default(),
            apps_dir: std::path::PathBuf::from("apps"),
            prompts_dir: std::path::PathBuf::from("prompts"),
            transport: Transport::StreamableHttp {
                auth: None,
                address: "127.0.0.1".parse().unwrap(),
                port,
                path: Default::default(),
                stateful_mode: false,
                host_validation: HostValidationConfig::default(),
                trusted_proxies: Default::default(),
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
            gen_ai_content: false,
            client_address: false,
            disable_auth_token_passthrough: false,
            descriptions: std::collections::HashMap::new(),
            annotations: std::collections::HashMap::new(),
            param_headers: std::collections::HashMap::new(),
            required_scopes: std::collections::HashMap::new(),
            search_leaf_depth: 5,
            index_memory_bytes: 1024 * 1024 * 1024,
            health_check,
            cors: Default::default(),
            server_info: Default::default(),
            instructions: None,
            caching: Default::default(),
            tasks: Default::default(),
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

    fn write_app(dir: &assert_fs::TempDir, name: &str, tools: &[&str]) {
        use assert_fs::prelude::*;
        use serde_json::json;

        let tools: Vec<_> = tools
            .iter()
            .map(|name| json!({"name": name, "description": name}))
            .collect();
        let manifest = json!({
            "format": "apollo-ai-app-manifest", "version": "1", "hash": "test", "resource": "index.html",
            "operations": [{"body": "query Lookup { hello }", "tools": tools}]
        });
        let app = dir.child(name);
        app.child(".application-manifest.json")
            .write_str(&manifest.to_string())
            .unwrap();
        app.child("index.html").write_str(name).unwrap();
    }

    #[rstest::rstest]
    #[case::one_selected(vec![])]
    #[case::both_selected(vec!["archive".into()])]
    #[tokio::test]
    async fn only_selected_apps_must_have_distinct_resource_uris(
        #[case] archive_tools: Vec<String>,
    ) {
        let dir = assert_fs::TempDir::new().unwrap();
        write_app(&dir, "records", &["lookup", "search_records"]);
        write_app(&dir, "records#archive", &["archive"]);
        let both_selected = !archive_tools.is_empty();
        let mut config = config(0, HealthCheckConfig::default());
        config.apps_dir = dir.path().into();
        config.expose_on_connect = [
            (
                "records".into(),
                vec!["lookup".into(), "search_records".into()],
            ),
            ("records#archive".into(), archive_tools),
        ]
        .into();

        let result = starting(config).start().await;
        if both_selected {
            let Some(ServerError::Apps(err)) = result.err() else {
                panic!("expected conflicting app resource URIs")
            };
            assert!(
                err.contains("records and records#archive")
                    || err.contains("records#archive and records")
            );
            assert!(err.contains("ui://widget/records"), "{err}");
        } else {
            result.unwrap().cancellation_token.cancel();
        }
    }

    #[tokio::test]
    async fn only_ordinary_tools_keep_their_tokenless_exceptions() {
        use serde_json::json;

        let dir = assert_fs::TempDir::new().unwrap();
        write_app(&dir, "records", &["lookup"]);
        let mut upstream = mockito::Server::new_async().await;
        let public_call = upstream
            .mock("POST", "/")
            .match_body(mockito::Matcher::PartialJson(
                json!({"operationName": "Public"}),
            ))
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":{"hello":"world"}}"#)
            .create_async()
            .await;
        let port = 7803;
        let mut config = config(port, HealthCheckConfig::default());
        let Transport::StreamableHttp { auth, .. } = &mut config.transport else {
            unreachable!()
        };
        *auth = Some(
            serde_json::from_value(json!({
                "servers": ["https://auth.example.com"], "resource": "http://localhost/mcp",
                "skip_token_validation": {"tools": ["lookup", "Public"]}
            }))
            .unwrap(),
        );
        config.endpoint = upstream.url().parse().unwrap();
        config.apps_dir = dir.path().into();
        config.expose_on_connect = [("records".into(), vec!["lookup".into()])].into();
        let mut server = starting(config);
        server.operations = vec![("query Public { hello }".to_owned(), None).into()];
        let running = server.start().await.unwrap();
        let _shutdown = running.cancellation_token.drop_guard();

        let client = reqwest::Client::new();
        for (name, expected) in [
            ("lookup", StatusCode::UNAUTHORIZED),
            ("Public", StatusCode::OK),
        ] {
            let response = client.post(format!("http://127.0.0.1:{port}/mcp"))
                .header("Accept", "application/json, text/event-stream")
                .header("Mcp-Protocol-Version", "2025-11-25")
                .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": {}}}))
                .send().await.unwrap();
            assert_eq!(response.status(), expected, "{name}");
            if name == "Public" {
                assert!(response.text().await.unwrap().contains("world"));
            }
        }
        public_call.assert_async().await;
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

    fn listener_builder(path: &str) -> ListenerBuilder {
        let running = super::super::running::test_support::create_test_running();
        let service = build_http_service(running, false, &Default::default());
        ListenerBuilder::new(
            "127.0.0.1:0".parse().unwrap(),
            EndpointPath::try_from(path.to_owned()).unwrap(),
            service,
            CorsConfig::default(),
        )
    }

    fn get(uri: &str) -> Request<Body> {
        Request::get(uri).body(Body::empty()).unwrap()
    }

    #[rstest::rstest]
    #[case::nested("/v1/agents")]
    #[case::root("/")]
    #[tokio::test]
    async fn serves_mcp_at_configured_path(#[case] path: &str) {
        let router = listener_builder(path).router;

        let res = router.oneshot(get(path)).await.unwrap();

        assert_ne!(res.status(), StatusCode::NOT_FOUND);
    }

    #[rstest::rstest]
    #[case::nested_default_path("/v1/agents", "/mcp")]
    #[case::nested_parent("/v1/agents", "/v1")]
    #[case::root_default_path("/", "/mcp")]
    #[case::root_scanner_probe("/", "/scanner-probe")]
    #[case::root_subpath("/", "/mcp/extra")]
    #[tokio::test]
    async fn returns_not_found_outside_configured_path(#[case] path: &str, #[case] uri: &str) {
        let router = listener_builder(path).router;

        let res = router.oneshot(get(uri)).await.unwrap();

        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[rstest::rstest]
    #[case::default_path("/mcp")]
    #[case::root("/")]
    #[tokio::test]
    async fn with_health_rejects_endpoint_path(#[case] path: &str) {
        let health_check = HealthCheck::new(HealthCheckConfig {
            enabled: true,
            path: path.to_owned(),
            ..Default::default()
        });
        let result = ListenerBuilder {
            router: axum::Router::new(),
            listen_address: "127.0.0.1:0".parse().unwrap(),
            path: EndpointPath::try_from(path.to_owned()).unwrap(),
            cors: CorsConfig::default(),
        }
        .with_health(Some(&health_check));

        assert!(matches!(
            result,
            Err(ServerError::HealthPathConflict { health_path, .. }) if health_path == path
        ));
    }

    #[tokio::test]
    async fn with_health_serves_beside_root_endpoint() {
        let health_check = HealthCheck::new(HealthCheckConfig {
            enabled: true,
            ..Default::default()
        });
        let listeners = listener_builder("/")
            .with_health(Some(&health_check))
            .unwrap();
        let UnboundListener { router, .. } = listeners.into_iter().next().unwrap();

        let res = router.clone().oneshot(get("/health")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = router.oneshot(get("/")).await.unwrap();
        assert_ne!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn with_auth_serves_metadata_beside_root_endpoint() {
        let auth: auth::Config = serde_json::from_value(serde_json::json!({
            "servers": ["https://auth.example.com"],
            "resource": "https://mcp.example.com/mcp",
        }))
        .unwrap();
        let router = listener_builder("/")
            .with_auth(
                Some(Box::new(auth)),
                HashMap::new(),
                true,
                &TrustedProxies::default(),
            )
            .unwrap()
            .router;

        let res = router
            .clone()
            .oneshot(get("/.well-known/oauth-protected-resource/mcp"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let res = router.oneshot(get("/")).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn with_health_serves_path_below_endpoint() {
        let running = super::super::running::test_support::create_test_running();
        let service = build_http_service(running, false, &Default::default());
        let health_check = HealthCheck::new(HealthCheckConfig {
            enabled: true,
            path: "/mcp/health".to_owned(),
            ..Default::default()
        });
        let listeners = ListenerBuilder::new(
            "127.0.0.1:0".parse().unwrap(),
            EndpointPath::default(),
            service,
            CorsConfig::default(),
        )
        .with_health(Some(&health_check))
        .unwrap();
        let UnboundListener { router, .. } = listeners.into_iter().next().unwrap();

        let req = Request::get("/mcp/health").body(Body::empty()).unwrap();
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
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
            path: EndpointPath::default(),
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
            path: EndpointPath::default(),
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

    #[rstest::rstest]
    #[case::enabled(true)]
    #[case::disabled(false)]
    #[tokio::test]
    async fn with_telemetry_exposes_the_client_address_only_when_enabled(#[case] enabled: bool) {
        use crate::server::states::telemetry::ClientAddress;

        let builder = ListenerBuilder {
            router: axum::Router::new().route(
                "/",
                axum::routing::get(|request: Request<Body>| async move {
                    request
                        .extensions()
                        .get::<ClientAddress>()
                        .is_some()
                        .to_string()
                }),
            ),
            listen_address: "127.0.0.1:0".parse().unwrap(),
            path: EndpointPath::default(),
            cors: CorsConfig::default(),
        };
        let mut request = Request::get("/").body(Body::empty()).unwrap();
        let peer: SocketAddr = "203.0.113.7:51234".parse().unwrap();
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(peer));

        let response = builder
            .with_telemetry(enabled, TrustedProxies::default())
            .router
            .oneshot(request)
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();

        assert_eq!(body, enabled.to_string());
    }

    #[tokio::test]
    async fn serve_http_exposes_the_peer_to_the_client_address_layer() {
        use crate::server::states::telemetry::ClientAddress;

        let router = with_client_address(
            axum::Router::new().route(
                "/",
                axum::routing::get(|request: Request<Body>| async move {
                    request
                        .extensions()
                        .get::<ClientAddress>()
                        .is_some()
                        .to_string()
                }),
            ),
            TrustedProxies::default(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        let server = tokio::spawn(serve_http(
            listener,
            router,
            shutdown.clone(),
            std::future::pending(),
        ));

        let body = reqwest::get(format!("http://{address}/"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        shutdown.cancel();
        server.await.unwrap().unwrap();

        assert_eq!(body, "true");
    }
}
