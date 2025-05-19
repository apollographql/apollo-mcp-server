use anyhow::bail;
use apollo_mcp_registry::uplink::persisted_queries::ManifestSource;
use apollo_mcp_registry::uplink::schema::SchemaSource;
use apollo_mcp_registry::uplink::{SecretString, UplinkConfig};
use apollo_mcp_server::custom_scalar_map::CustomScalarMap;
use apollo_mcp_server::errors::ServerError;
use apollo_mcp_server::operations::MutationMode;
use apollo_mcp_server::operations::OperationSource;
use apollo_mcp_server::server::Server;
use apollo_mcp_server::server::Transport;
use clap::Parser;
use clap::builder::Styles;
use clap::builder::styling::{AnsiColor, Effects};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::env;
use std::net::{IpAddr, Ipv4Addr};
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
    #[arg(long, short = 'd')]
    directory: PathBuf,

    /// The path to the GraphQL API schema file
    #[arg(long, short = 's')]
    schema: Option<PathBuf>,

    /// The path to the GraphQL custom_scalars_config file
    #[arg(long, short = 'c', required = false)]
    custom_scalars_config: Option<PathBuf>,

    /// The GraphQL endpoint the server will invoke
    #[arg(long, short = 'e', default_value = "http://127.0.0.1:4000")]
    endpoint: String,

    /// Headers to send to the endpoint
    #[arg(long = "header", action = clap::ArgAction::Append)]
    headers: Vec<String>,

    /// The IP address to bind the SSE server to (default: 127.0.0.1)
    #[arg(long)]
    sse_address: Option<IpAddr>,

    /// Start the server using the SSE transport on the given port (default: 5000)
    #[arg(long)]
    sse_port: Option<u16>,

    /// Expose the schema to the MCP client through `introspect` and `execute` tools
    #[arg(long, short = 'i')]
    introspection: bool,

    /// Enable use of uplink to get the schema and persisted queries (requires APOLLO_KEY and APOLLO_GRAPH_REF)
    #[arg(long, short = 'u')]
    uplink: bool,

    /// Expose a tool to open queries in Apollo Explorer (requires APOLLO_KEY and APOLLO_GRAPH_REF)
    #[arg(long, short = 'x')]
    explorer: bool,

    /// Operation files to expose as MCP tools
    #[arg(long = "operations", short = 'o', num_args=0..)]
    operations: Vec<PathBuf>,

    /// The path to the persisted query manifest containing operations
    #[arg(long)]
    manifest: Option<PathBuf>,

    // Configure when to allow mutations
    #[clap(long, short = 'm', default_value_t, value_enum)]
    allow_mutations: MutationMode,

    /// Disable operation root field types in tool description
    #[arg(long)]
    disable_type_description: bool,

    /// Disable schema type definitions referenced by all fields returned by the operation in the tool description
    #[arg(long)]
    disable_schema_description: bool,
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

    let transport = if args.sse_port.is_some() || args.sse_address.is_some() {
        Transport::SSE {
            address: args.sse_address.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            port: args.sse_port.unwrap_or(5000),
        }
    } else {
        Transport::Stdio
    };

    env::set_current_dir(args.directory)?;
    Ok(Server::builder()
        .transport(transport)
        .schema_source(schema_source)
        .operation_source(operation_source)
        .endpoint(args.endpoint)
        .explorer(args.explorer)
        .headers(default_headers)
        .introspection(args.introspection)
        .mutation_mode(args.allow_mutations)
        .disable_type_description(args.disable_type_description)
        .disable_schema_description(args.disable_schema_description)
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
