use apollo_mcp_server::auth::Config;
use apollo_mcp_server::errors::ServerError;
use apollo_mcp_server::health::{HealthCheck, HealthCheckConfig};
use apollo_mcp_server::server::Transport;
use apollo_mcp_server::server::states::shutdown_signal;
use apollo_mcp_server::server_handler::ApolloMcpServerHandler;
use axum::extract::Query;
use axum::routing::get;
use axum::{Json, Router};
use http::StatusCode;
use rmcp::service::{RunningService, ServerInitializeError};
use rmcp::transport::sse_server::SseServerConfig;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::{SseServer, StreamableHttpService, stdio};
use rmcp::{RoleServer, ServiceExt};
use serde_json::json;
use std::io::Error;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, error, info, trace};

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

pub struct Serve;

impl Serve {
    pub async fn serve(
        server_handler: ApolloMcpServerHandler,
        transport: Transport,
        cancellation_token: CancellationToken,
        health_check_config: HealthCheckConfig,
    ) -> Result<(), ServerError> {
        match transport {
            Transport::StreamableHttp {
                auth,
                address,
                port,
            } => {
                serve_streamable_http(auth, address, port, server_handler, health_check_config)
                    .await?;
            }
            Transport::SSE {
                auth,
                address,
                port,
            } => {
                serve_sse(auth, address, port, server_handler, cancellation_token).await?;
            }
            Transport::Stdio => {
                let service = serve_stdio(server_handler)
                    .await
                    .map_err(|e| ServerError::McpInitializeError(e.into()))?;
                service.waiting().await.map_err(ServerError::StartupError)?;
            }
        }

        Ok(())
    }
}

// Create health check if enabled (only for StreamableHttp transport)
fn create_health_check(config: HealthCheckConfig) -> Option<HealthCheck> {
    // let telemetry: Arc<dyn Telemetry> = Arc::new(InMemoryTelemetry::new());
    Some(HealthCheck::new(config))
}

async fn serve_streamable_http(
    auth: Option<Config>,
    address: IpAddr,
    port: u16,
    server_handler: ApolloMcpServerHandler,
    health_check_config: HealthCheckConfig,
) -> Result<(), ServerError> {
    info!(port = ?port, address = ?address, "Starting MCP server in Streamable HTTP mode");
    let listen_address = SocketAddr::new(address, port);
    let service = StreamableHttpService::new(
        move || Ok(server_handler.clone()),
        LocalSessionManager::default().into(),
        Default::default(),
    );

    let mut router = with_auth!(Router::new().nest_service("/mcp", service), auth);

    // Add health check endpoint if configured
    if health_check_config.enabled {
        if let Some(health_check) = create_health_check(health_check_config) {
            let health_router = Router::new()
                .route(&health_check.config().path, get(health_endpoint))
                .with_state(health_check.clone());
            router = router.merge(health_router);
        }
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

    Ok(())
}

async fn serve_sse(
    auth: Option<Config>,
    address: IpAddr,
    port: u16,
    server_handler: ApolloMcpServerHandler,
    cancellation_token: CancellationToken,
) -> Result<(), Error> {
    info!(port = ?port, address = ?address, "Starting MCP server in SSE mode");
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
    let axum_server = axum::serve(listener, router).with_graceful_shutdown(async move {
        ct.cancelled().await;
        info!("mcp server cancelled");
    });

    tokio::spawn(
        async move {
            if let Err(e) = axum_server.await {
                error!(error = %e, "mcp shutdown with error");
            }
        }
        .instrument(tracing::info_span!("mcp-server", bind_address = %server.config.bind)),
    );

    server.with_service(move || server_handler.clone());
    Ok(())
}

async fn serve_stdio(
    server_handler: ApolloMcpServerHandler,
) -> Result<RunningService<RoleServer, ApolloMcpServerHandler>, ServerInitializeError<Error>> {
    info!("Starting MCP server in stdio mode");
    server_handler.serve(stdio()).await.inspect_err(|e| {
        error!("serving error: {:?}", e);
    })
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
