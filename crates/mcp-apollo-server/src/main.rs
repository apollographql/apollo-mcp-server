use anyhow::Context as _;
use apollo_compiler::Schema;
use clap::builder::Styles;
use clap::builder::styling::{AnsiColor, Effects};
use clap::{Parser, ValueEnum};
use mcp_apollo_server::errors::ServerError;
use mcp_apollo_server::server::Server;
use rmcp::ServiceExt;
use rmcp::transport::{SseServer, stdio};
use rover_copy::pq_manifest::{ApolloPersistedQueryManifest, RelayPersistedQueryManifest};
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

    /// Operation files to expose as MCP tools
    #[arg(long = "operations", short = 'o', num_args=0..)]
    operations: Vec<PathBuf>,

    /// Persisted Queries manifest to expose as MCP tools
    #[command(flatten)]
    pq_manifest: Option<ManifestArgs>,
}

// TODO: This is currently yoiked from rover
#[derive(Debug, Clone, ValueEnum)]
enum PersistedQueriesManifestFormat {
    Apollo,
    Relay,
}

#[derive(Debug, Parser)]
#[group(requires = "manifest")]
struct ManifestArgs {
    /// The path to the manifest containing operations to publish.
    #[arg(long, required = false)]
    manifest: PathBuf,

    /// The format of the manifest file.
    #[arg(long, value_enum, default_value_t = PersistedQueriesManifestFormat::Apollo)]
    manifest_format: PersistedQueriesManifestFormat,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::INFO.into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

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
        .and_persisted_query_manifest(
            args.pq_manifest
                .map(
                    |ManifestArgs {
                         manifest,
                         manifest_format,
                     }| {
                        tracing::info!(manifest=?manifest, "Loading persisted query manifest");
                        let raw_manifest = std::fs::read_to_string(&manifest)
                            .context("Could not read manifest")?;
                        let invalid_json_err = |manifest, format| {
                            format!(
                                "JSON in {manifest:?} did not match '--manifest-format {format}'"
                            )
                        };

                        let pq_manifest = match manifest_format {
                            PersistedQueriesManifestFormat::Apollo => {
                                rmcp::serde_json::from_str::<ApolloPersistedQueryManifest>(
                                    &raw_manifest,
                                )
                                .with_context(|| invalid_json_err(&manifest, "apollo"))?
                            }
                            PersistedQueriesManifestFormat::Relay => {
                                rmcp::serde_json::from_str::<RelayPersistedQueryManifest>(
                                    &raw_manifest,
                                )
                                .with_context(|| invalid_json_err(&manifest, "relay"))?
                                .try_into()
                                .context("Could not convert relay manifest to Apollo's format")?
                            }
                        };

                        // This disambiguiation is sad but needed
                        Ok::<_, anyhow::Error>(pq_manifest)
                    },
                )
                .transpose()?,
        )
        .build()?;

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
