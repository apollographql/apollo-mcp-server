use std::path::PathBuf;

use apollo_mcp_registry::platform_api::operation_collections::collection_poller::CollectionSource;
use apollo_mcp_registry::uplink::persisted_queries::ManifestSource;
use apollo_mcp_registry::uplink::schema::SchemaSource;
use apollo_mcp_server::custom_scalar_map::CustomScalarMap;
use apollo_mcp_server::errors::ServerError;
use apollo_mcp_server::operations::OperationSource;
use apollo_mcp_server::server::Server;
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
    let config: runtime::Config = match Args::parse().config {
        Some(config_path) => runtime::read_config(config_path)?,
        None => runtime::read_config_from_env().unwrap_or_default(),
    };

    let _guard = runtime::telemetry::init_tracing_subscriber(&config)?;

    info!(
        "Apollo MCP Server v{} // (c) Apollo Graph, Inc. // Licensed under MIT",
        env!("CARGO_PKG_VERSION")
    );

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

    let transport = config.transport.clone();

    Ok(Server::builder()
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
        .mutation_mode(config.overrides.mutation_mode)
        .disable_type_description(config.overrides.disable_type_description)
        .disable_schema_description(config.overrides.disable_schema_description)
        .disable_auth_token_passthrough(match transport {
            apollo_mcp_server::server::Transport::Stdio => false,
            apollo_mcp_server::server::Transport::SSE { auth, .. } => auth
                .map(|a| a.disable_auth_token_passthrough)
                .unwrap_or(false),
            apollo_mcp_server::server::Transport::StreamableHttp { auth, .. } => auth
                .map(|a| a.disable_auth_token_passthrough)
                .unwrap_or(false),
        })
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
        .build()
        .start()
        .await?)
}
