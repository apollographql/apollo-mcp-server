use apollo_compiler::Schema;
use clap::Parser;
use clap::builder::Styles;
use clap::builder::styling::{AnsiColor, Effects};
use mcp_apollo_server::custom_scalar_map::CustomScalarMap;
use mcp_apollo_server::errors::ServerError;
use mcp_apollo_server::server::Server;
use rmcp::ServiceExt;
use rmcp::transport::{SseServer, stdio};
use std::env;
use std::path::{Path, PathBuf};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

/// Clap styling
const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default());

/// Arguments to the MCP server
#[derive(Debug, clap::Parser)]
#[command(
    styles = STYLES,
    about = "Apollo MCP Server - invoke GraphQL operations from an AI agent",
)]
struct Args {
    /// The working directory to use
    #[clap(long, short = 'd')]
    directory: PathBuf,

    /// The path to the GraphQL API schema file
    #[clap(long, short = 's')]
    schema: PathBuf,

    /// The path to the GraphQL custom_scalars_config file
    #[clap(long, short = 'c', required = false)]
    custom_scalars_config: Option<PathBuf>,

    /// The GraphQL endpoint the server will invoke
    #[clap(long, short = 'e', default_value = "http://127.0.0.1:4000")]
    endpoint: String,

    /// Headers to send to the endpoint
    #[clap(long = "header", action = clap::ArgAction::Append)]
    headers: Vec<String>,

    /// Start the server using the SSE transport on the given port
    #[clap(long)]
    sse_port: Option<u16>,

    /// Expose the schema to the MCP client through `schema` and `execute` tools
    #[clap(long, short = 'i')]
    introspection: bool,

    /// Enable use of uplink to get the schema and persisted queries (requires APOLLO_KEY and APOLLO_GRAPH_REF)
    #[clap(long, short = 'u')]
    uplink: bool,

    /// Expose a tool to open queries in Apollo Explorer (requires APOLLO_KEY and APOLLO_GRAPH_REF)
    #[clap(long, short = 'x')]
    explorer: bool,

    /// Operation files to expose as MCP tools
    #[arg(long = "operations", short = 'o', num_args=0..)]
    operations: Vec<PathBuf>,

    /// The path to the persisted query manifest containing operations
    #[arg(long)]
    manifest: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(std::io::stderr)
        .with_ansi(true)
        .init();

    info!(
        "Apollo MCP Server v{} // (c) Apollo Graph, Inc. // Licensed as ELv2 (https://go.apollo.dev/elv2)",
        std::env!("CARGO_PKG_VERSION")
    );

    let args = Args::parse();
    env::set_current_dir(args.directory)?;

    let schema_path: &Path = args.schema.as_ref();
    info!(schema_path=?schema_path, "Loading schema");
    let schema = std::fs::read_to_string(schema_path)?;
    let schema = Schema::parse_and_validate(schema, schema_path)
        .map_err(|e| ServerError::GraphQLSchema(Box::new(e)))?;

    let server = Server::builder()
        .schema(schema)
        .endpoint(args.endpoint)
        .operations(args.operations)
        .headers(args.headers)
        .introspection(args.introspection)
        .uplink(args.uplink)
        .explorer(args.explorer)
        .manifests(args.manifest.into_iter().collect())
        .and_custom_scalar_map(
            args.custom_scalars_config
                .map(|custom_scalars_config| CustomScalarMap::try_from(&custom_scalars_config))
                .transpose()?,
        )
        .build()
        .await?;

    if let Some(port) = args.sse_port {
        info!(port = ?port, "Starting MCP server in SSE mode");
        let cancellation_token = SseServer::serve(format!("127.0.0.1:{port}").parse()?)
            .await?
            .with_service(move || server.clone());
        tokio::signal::ctrl_c().await?;
        cancellation_token.cancel();
    } else {
        info!("Starting MCP server in stdio mode");
        let service = server.serve(stdio()).await.inspect_err(|e| {
            error!("serving error: {:?}", e);
        })?;
        service.waiting().await?;
    }

    Ok(())
}
