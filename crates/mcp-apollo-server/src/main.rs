use anyhow::bail;
use clap::Parser;
use clap::builder::Styles;
use clap::builder::styling::{AnsiColor, Effects};
use mcp_apollo_registry::uplink::persisted_queries::ManifestSource;
use mcp_apollo_registry::uplink::schema::SchemaSource;
use mcp_apollo_registry::uplink::{SecretString, UplinkConfig};
use mcp_apollo_server::custom_scalar_map::CustomScalarMap;
use mcp_apollo_server::errors::ServerError;
use mcp_apollo_server::operations::OperationSource;
use mcp_apollo_server::server::{Server, Transport};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::env;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use tracing::info;
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
    schema: Option<PathBuf>,

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

    let schema_source = if let Some(path) = args.schema {
        SchemaSource::File { path, watch: true }
    } else {
        SchemaSource::Registry(uplink_config()?)
    };

    let operation_source = if let Some(manifest) = args.manifest {
        OperationSource::Manifest(ManifestSource::LocalHotReload(vec![manifest]))
    } else if args.uplink {
        OperationSource::Manifest(ManifestSource::Uplink(uplink_config()?))
    } else if !args.operations.is_empty() {
        OperationSource::Files(args.operations)
    } else {
        if !args.introspection {
            bail!(ServerError::NoOperations);
        }
        OperationSource::None
    };

    let mut default_headers = HeaderMap::new();
    for header in args.headers {
        let parts: Vec<&str> = header.split(':').map(|s| s.trim()).collect();
        match (parts.first(), parts.get(1), parts.get(2)) {
            (Some(key), Some(value), None) => {
                default_headers.append(HeaderName::from_str(key)?, HeaderValue::from_str(value)?);
            }
            _ => bail!(ServerError::Header(header)),
        }
    }

    let transport = args
        .sse_port
        .map_or(Transport::Stdio, |port| Transport::SSE { port });

    env::set_current_dir(args.directory)?;
    Ok(Server::builder()
        .transport(transport)
        .schema_source(schema_source)
        .operation_source(operation_source)
        .endpoint(args.endpoint)
        .explorer(args.explorer)
        .headers(default_headers)
        .introspection(args.introspection)
        .and_custom_scalar_map(
            args.custom_scalars_config
                .map(|custom_scalars_config| CustomScalarMap::try_from(&custom_scalars_config))
                .transpose()?,
        )
        .build()
        .start()
        .await?)
}

fn uplink_config() -> Result<UplinkConfig, ServerError> {
    Ok(UplinkConfig {
        apollo_key: SecretString::from(
            env::var("APOLLO_KEY")
                .map_err(|_| ServerError::EnvironmentVariable(String::from("APOLLO_KEY")))?,
        ),
        apollo_graph_ref: env::var("APOLLO_GRAPH_REF")
            .map_err(|_| ServerError::EnvironmentVariable(String::from("APOLLO_GRAPH_REF")))?,
        poll_interval: Duration::from_secs(10),
        timeout: Duration::from_secs(30),
        endpoints: None, // Use the default endpoints
    })
}
