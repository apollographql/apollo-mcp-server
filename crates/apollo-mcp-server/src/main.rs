use std::path::PathBuf;

use apollo_mcp_registry::platform_api::operation_collections::collection_poller::CollectionSource;
use apollo_mcp_registry::uplink::persisted_queries::ManifestSource;
use apollo_mcp_registry::uplink::schema::SchemaSource;
use apollo_mcp_server::custom_scalar_map::CustomScalarMap;
use apollo_mcp_server::errors::ServerError;
use apollo_mcp_server::operations::OperationSource;
use apollo_mcp_server::server::Server;
use apollo_mcp_server::server::Transport;
use clap::Parser;
use clap::builder::Styles;
use clap::builder::styling::{AnsiColor, Effects};
use runtime::IdOrDefault;
use tracing::{Level, info, warn};
use tracing_subscriber::EnvFilter;

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
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config: runtime::Config = {
        let args = Args::parse();
        runtime::read_config(args.config)?
    };

    let mut env_filter = EnvFilter::from_default_env().add_directive(config.logging.level.into());

    // Suppress noisy dependency logging at the INFO level
    if config.logging.level == Level::INFO {
        env_filter = env_filter
            .add_directive("rmcp=warn".parse()?)
            .add_directive("tantivy=warn".parse()?);
    }

    // When using the Stdio transport, send output to stderr since stdout is used for MCP messages
    match config.transport {
        Transport::SSE { .. } | Transport::StreamableHttp { .. } => tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_ansi(true)
            .with_target(false)
            .init(),
        Transport::Stdio => tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr)
            .with_ansi(true)
            .with_target(false)
            .init(),
    };

    info!(
        "Apollo MCP Server v{} // (c) Apollo Graph, Inc. // Licensed under MIT",
        std::env!("CARGO_PKG_VERSION")
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

    Ok(Server::builder()
        .transport(config.transport)
        .schema_source(schema_source)
        .operation_source(operation_source)
        .endpoint(config.endpoint)
        .maybe_explorer_graph_ref(explorer_graph_ref)
        .headers(config.headers)
        .execute_introspection(config.introspection.execute.enabled)
        .introspect_introspection(config.introspection.introspect.enabled)
        .search_introspection(config.introspection.search.enabled)
        .minify_introspection(config.introspection.minify)
        .mutation_mode(config.overrides.mutation_mode)
        .disable_type_description(config.overrides.disable_type_description)
        .disable_schema_description(config.overrides.disable_schema_description)
        .custom_scalar_map(
            config
                .custom_scalars
                .map(|custom_scalars_config| CustomScalarMap::try_from(&custom_scalars_config))
                .transpose()?,
        )
        .build()
        .start()
        .await?)
}
