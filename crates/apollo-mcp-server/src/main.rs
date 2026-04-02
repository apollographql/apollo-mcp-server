use std::path::{Path, PathBuf};

use apollo_mcp_registry::platform_api::operation_collections::collection_poller::CollectionSource;
use apollo_mcp_registry::uplink::persisted_queries::ManifestSource;
use apollo_mcp_registry::uplink::schema::SchemaSource;
use apollo_mcp_server::custom_scalar_map::CustomScalarMap;
use apollo_mcp_server::errors::ServerError;
use apollo_mcp_server::operations::OperationSource;
use apollo_mcp_server::server::{Server, ShutdownReason, Transport};
use clap::Parser;
use clap::builder::Styles;
use clap::builder::styling::{AnsiColor, Effects};
use runtime::IdOrDefault;
use tracing::{debug, info, warn};

mod runtime;

/// Clap styling
const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default());

/// Arguments to the MCP server
#[derive(Debug, Parser)]
#[command(
    version,
    styles = STYLES,
    about = "Apollo MCP Server - invoke GraphQL operations from an AI agent",
)]
struct Args {
    /// Path to the config file
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let config_path = args.config;

    let config = load_config(config_path.as_deref())?;
    let _guard = runtime::telemetry::init_tracing_subscriber(&config)?;

    info!(
        "Apollo MCP Server v{} // (c) Apollo Graph, Inc. // Licensed under MIT",
        env!("CARGO_PKG_VERSION")
    );

    // For stdio transport, spawn a background config watcher that exits the process
    // when the config file changes, since the stdio event loop blocks and never
    // polls the config watch stream in the state machine.
    if matches!(config.transport, Transport::Stdio) {
        if let Some(ref path) = config_path {
            spawn_stdio_config_watcher(path.clone());
        }
        spawn_stdio_sighup_handler();
    }

    loop {
        let server = build_server(config_path.as_deref())?;
        match server.start().await {
            Ok(ShutdownReason::Shutdown) => break Ok(()),
            Ok(ShutdownReason::Restart) => {
                info!("Config changed, restarting server...");
                warn!(
                    "Logging and telemetry configuration changes require a full process restart to take effect"
                );
                // Brief delay to let the port be released
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
            Err(e) => break Err(e.into()),
        }
    }
}

/// Load and parse configuration from file or environment.
fn load_config(config_path: Option<&std::path::Path>) -> anyhow::Result<runtime::Config> {
    match config_path {
        Some(path) => runtime::read_config(path).map_err(Into::into),
        None => Ok(runtime::read_config_from_env().unwrap_or_default()),
    }
}

/// Build a Server from the current configuration.
fn build_server(config_path: Option<&std::path::Path>) -> anyhow::Result<Server> {
    let config = load_config(config_path)?;

    #[cfg_attr(coverage_nightly, coverage(off))]
    debug!("Configuration: {config:#?}");
    #[cfg_attr(coverage_nightly, coverage(on))]
    let schema_source = match config.schema {
        runtime::SchemaSource::Local { path } => SchemaSource::File { path, watch: true },
        runtime::SchemaSource::Uplink => SchemaSource::Registry(config.graphos.uplink_config()?),
    };

    let operation_source = match config.operations {
        // Default collection is special and requires other information
        runtime::OperationSource::Collection {
            id: IdOrDefault::Default,
        } => OperationSource::Collection(CollectionSource::Default(
            config.graphos.graph_ref()?,
            config.graphos.platform_api_config()?,
        )),

        runtime::OperationSource::Collection {
            id: IdOrDefault::Id(collection_id),
        } => OperationSource::Collection(CollectionSource::Id(
            collection_id,
            config.graphos.platform_api_config()?,
        )),
        runtime::OperationSource::Introspect => OperationSource::None,
        runtime::OperationSource::Local { paths } if !paths.is_empty() => {
            OperationSource::from(paths)
        }
        runtime::OperationSource::Manifest { path } => {
            OperationSource::from(ManifestSource::LocalHotReload(vec![path]))
        }
        runtime::OperationSource::Uplink => {
            OperationSource::from(ManifestSource::Uplink(config.graphos.uplink_config()?))
        }

        // TODO: Inference requires many different combinations and preferences
        // TODO: We should maybe make this more explicit.
        runtime::OperationSource::Local { .. } | runtime::OperationSource::Infer => {
            if config.introspection.any_enabled() {
                warn!("No operations specified, falling back to introspection");
                OperationSource::None
            } else if let Ok(graph_ref) = config.graphos.graph_ref() {
                warn!(
                    "No operations specified, falling back to the default collection in {}",
                    graph_ref
                );
                OperationSource::Collection(CollectionSource::Default(
                    graph_ref,
                    config.graphos.platform_api_config()?,
                ))
            } else {
                anyhow::bail!(ServerError::NoOperations)
            }
        }
    };

    let explorer_graph_ref = config
        .overrides
        .enable_explorer
        .then(|| config.graphos.graph_ref())
        .transpose()?;

    let disable_auth_token_passthrough = matches!(
        &config.transport,
        Transport::StreamableHttp { auth: Some(auth), .. } if auth.disable_auth_token_passthrough
    );

    Ok(Server::builder()
        .maybe_config_path(config_path.map(Path::to_path_buf))
        .transport(config.transport)
        .schema_source(schema_source)
        .operation_source(operation_source)
        .endpoint(config.endpoint.into_inner())
        .maybe_explorer_graph_ref(explorer_graph_ref)
        .headers(config.headers)
        .forward_headers(config.forward_headers)
        .execute_introspection(config.introspection.execute.enabled)
        .validate_introspection(config.introspection.validate.enabled)
        .introspect_introspection(config.introspection.introspect.enabled)
        .introspect_minify(config.introspection.introspect.minify)
        .search_minify(config.introspection.search.minify)
        .search_introspection(config.introspection.search.enabled)
        .maybe_execute_tool_hint(config.introspection.execute.hint)
        .maybe_introspect_tool_hint(config.introspection.introspect.hint)
        .maybe_search_tool_hint(config.introspection.search.hint)
        .maybe_validate_tool_hint(config.introspection.validate.hint)
        .mutation_mode(config.overrides.mutation_mode)
        .disable_type_description(config.overrides.disable_type_description)
        .disable_schema_description(config.overrides.disable_schema_description)
        .enable_output_schema(config.overrides.enable_output_schema)
        .descriptions(config.overrides.descriptions)
        .required_scopes(config.overrides.required_scopes)
        .disable_auth_token_passthrough(disable_auth_token_passthrough)
        .custom_scalar_map(
            config
                .custom_scalars
                .map(|custom_scalars_config| CustomScalarMap::try_from(&custom_scalars_config))
                .transpose()?,
        )
        .search_leaf_depth(config.introspection.search.leaf_depth)
        .index_memory_bytes(config.introspection.search.index_memory_bytes)
        .health_check(config.health_check)
        .cors(config.cors)
        .server_info(config.server_info)
        .build())
}

/// Spawn a background task that watches the config file and exits the process
/// when it changes. Used for stdio mode where the state machine event loop
/// is blocked by `service.waiting().await`.
#[expect(clippy::exit, reason = "process::exit used for stdio mode restart")]
fn spawn_stdio_config_watcher(config_path: PathBuf) {
    use apollo_mcp_registry::files;
    use futures::StreamExt as _;

    tokio::spawn(async move {
        // Skip the initial event that files::watch always emits on startup,
        // then exit when a real file change is detected.
        let mut stream = std::pin::pin!(files::watch(&config_path).skip(1));
        if stream.next().await.is_some() {
            info!("Config file changed, exiting for process manager to restart");
            std::process::exit(75);
        }
    });
}

/// Spawn a background SIGHUP handler for stdio mode.
#[cfg_attr(
    unix,
    expect(
        clippy::exit,
        reason = "process::exit used for stdio mode SIGHUP restart"
    )
)]
fn spawn_stdio_sighup_handler() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::SignalKind;

        tokio::spawn(async move {
            let Ok(mut signal) = tokio::signal::unix::signal(SignalKind::hangup()) else {
                tracing::error!("Failed to install SIGHUP handler");
                return;
            };
            if signal.recv().await.is_some() {
                info!("Received SIGHUP, exiting for process manager to restart");
                std::process::exit(75);
            }
        });
    }
}
