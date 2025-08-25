use crate::runtime::Serve;
use apollo_mcp_registry::platform_api::operation_collections::collection_poller::CollectionSource;
use apollo_mcp_registry::uplink::persisted_queries::ManifestSource;
use apollo_mcp_registry::uplink::schema::SchemaSource;
use apollo_mcp_server::custom_scalar_map::CustomScalarMap;
use apollo_mcp_server::errors::ServerError;
use apollo_mcp_server::operations::OperationSource;
use apollo_mcp_server::server::Server;
use apollo_mcp_server::server_config::ServerConfig;
use apollo_mcp_server::server_handler::ApolloMcpServerHandler;
use clap::Parser;
use clap::builder::Styles;
use clap::builder::styling::{AnsiColor, Effects};
use runtime::IdOrDefault;
use runtime::logging::Logging;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

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
    let config: runtime::Config = match Args::parse().config {
        Some(config_path) => runtime::read_config(config_path)?,
        None => runtime::read_config_from_env().unwrap_or_default(),
    };

    // WorkerGuard is not used but needed to be at least defined or else the guard
    // is cleaned up too early and file appender logging does not work
    let _guard = Logging::setup(&config)?;

    info!(
        "Apollo MCP Server v{} // (c) Apollo Graph, Inc. // Licensed under MIT",
        env!("CARGO_PKG_VERSION")
    );

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

    let server_handler = ApolloMcpServerHandler::new(config.headers.clone(), config.endpoint.clone());
    let cancellation_token = CancellationToken::new();

    let server_config = ServerConfig::builder()
        .maybe_explorer_graph_ref(explorer_graph_ref)
        .headers(config.headers.clone())
        .execute_enabled(config.introspection.execute.enabled)
        .validate_enabled(config.introspection.validate.enabled)
        .introspect_enabled(config.introspection.introspect.enabled)
        .introspect_minify(config.introspection.introspect.minify)
        .search_enabled(config.introspection.search.enabled)
        .search_minify(config.introspection.search.minify)
        .mutation_mode(config.overrides.mutation_mode)
        .disable_type_description(config.overrides.disable_type_description)
        .disable_schema_description(config.overrides.disable_schema_description)
        .custom_scalar_map(
            config
                .custom_scalars
                .map(|custom_scalars_config| CustomScalarMap::try_from(&custom_scalars_config))
                .transpose()?,
        )
        .search_leaf_depth(config.introspection.search.leaf_depth)
        .index_memory_bytes(config.introspection.search.index_memory_bytes)
        .health_check(config.health_check.clone())
        .build();
    let state_machine = Server::builder()
        .schema_source(schema_source)
        .operation_source(operation_source)
        .server_handler(server_handler.clone())
        .cancellation_token(cancellation_token.child_token())
        .server_config(server_config)
        .build()
        .start();

    let server = Serve::serve(
        server_handler,
        config.transport,
        cancellation_token,
        config.health_check,
    );

    let (state_machine_result, server_result) = tokio::join!(state_machine, server);

    match (state_machine_result, server_result) {
        (Ok(()), Ok(())) => {
            Ok(())
        },
        (Err(state_error), Err(server_error)) => anyhow::bail!("Both state machine and server have errors {} | {}", state_error, server_error),
        (Err(state_error), _) => Err(state_error.into()),
        (_, Err(server_error)) => Err(server_error.into()),
    }
}
