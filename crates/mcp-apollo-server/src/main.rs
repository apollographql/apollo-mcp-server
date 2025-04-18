use anyhow::Context as _;
use clap::Parser;
use clap::ValueEnum;
use clap::builder::Styles;
use clap::builder::styling::{AnsiColor, Effects};
use mcp_apollo_server::ApolloPersistedQueryManifest;
use mcp_apollo_server::OperationsList;
use mcp_apollo_server::RelayPersistedQueryManifest;
use mcp_apollo_server::operations::Operation;
use mcp_apollo_server::server::Server;
use rmcp::ServiceExt;
use rmcp::serde_json;
use rmcp::transport::{SseServer, stdio};
use std::env;
use std::path::Path;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

/// Clap styling
const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Cyan.on_default());

// Define clap arguments
#[derive(Debug, clap::Parser)]
#[command(
    styles = STYLES,
    about = "Apollo MCP Server - invoke GraphQL operations from an AI agent",
)]
struct Args {
    /// The working directory to use
    #[clap(long, short = 'd')]
    directory: PathBuf,

    /// The path to the GraphQL schema file
    #[clap(long, short = 's')]
    schema: PathBuf,

    /// The GraphQL endpoint the server will invoke
    #[clap(long, short = 'e', default_value = "http://127.0.0.1:4000")]
    endpoint: String,

    /// Headers to send to endpoint
    #[clap(long = "header", action = clap::ArgAction::Append)]
    headers: Vec<String>,

    /// Start the server using the SSE transport on the given port
    #[clap(long)]
    sse_port: Option<u16>,

    /// Operation files to expose as MCP tools
    #[arg(long = "operations", short = 'o', num_args=0.., default_value = "Vec::new()")]
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

    // Load all possible operations
    let operations = load_operations(args.schema, args.operations, args.pq_manifest)?;
    tracing::info!(
        "Loaded operations:\n{}",
        serde_json::to_string_pretty(&operations)?
    );

    let server = Server::from_operations(args.endpoint, args.headers, operations)?;

    if let Some(port) = args.sse_port {
        tracing::info!(port = ?port, "Starting MCP server in SSE mode");
        let cancellation_token = SseServer::serve(format!("127.0.0.1:{port}").parse()?)
            .await?
            .with_service(move || server.clone());
        tokio::signal::ctrl_c().await?;
        cancellation_token.cancel();
    } else {
        tracing::info!("Starting MCP server in stdio mode");
        let service = server.serve(stdio()).await.inspect_err(|e| {
            tracing::error!("serving error: {:?}", e);
        })?;
        service.waiting().await?;
    }

    Ok(())
}

fn load_operations<P: AsRef<Path>>(
    schema: P,
    raw_operations: Vec<P>,
    manifest: Option<ManifestArgs>,
) -> anyhow::Result<OperationsList> {
    use apollo_compiler::parser::Parser;

    let schema_path = schema.as_ref();
    tracing::info!(schema_path=?schema_path, "Loading schema");

    let graphql_schema = std::fs::read_to_string(schema_path)?;
    let mut parser = Parser::new();
    let graphql_schema = parser
        .parse_ast(graphql_schema, schema_path)
        .map_err(|e| anyhow::format_err!("Could not parse GraphQL schema: {e}"))?;
    let graphql_schema = graphql_schema
        .to_schema()
        .map_err(|e| anyhow::format_err!("Could not parse GraphQL schema: {e}"))?;

    let mut operations = raw_operations
        .into_iter()
        .map(|operation| {
            tracing::info!(operation_path=?operation.as_ref(), "Loading operation");
            let operation = std::fs::read_to_string(operation)?;

            Operation::from_document(&operation, &graphql_schema, None)
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Optionally add operations from PQ manifests
    if let Some(manifest) = manifest {
        let raw_manifest =
            std::fs::read_to_string(&manifest.manifest).context("Could not read manifest")?;
        let invalid_json_err = |manifest, format| {
            format!("JSON in {manifest:?} did not match '--manifest-format {format}'")
        };

        let operation_manifest = match manifest.manifest_format {
            PersistedQueriesManifestFormat::Apollo => {
                serde_json::from_str::<ApolloPersistedQueryManifest>(&raw_manifest)
                    .with_context(|| invalid_json_err(&manifest.manifest, "apollo"))?
            }
            PersistedQueriesManifestFormat::Relay => {
                serde_json::from_str::<RelayPersistedQueryManifest>(&raw_manifest)
                    .with_context(|| invalid_json_err(&manifest.manifest, "relay"))?
                    .try_into()?
            }
        };

        // TODO: This is a bit hacky, but operation takes source text...
        let pq_ops = operation_manifest
            .operations
            .into_iter()
            .map(|pq| {
                Operation::from_document(
                    &format!("{} {} {{ {} }}", pq.r#type, pq.name, pq.body),
                    &graphql_schema,
                    None,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        operations.extend(pq_ops.into_iter());
    }

    tracing::info!(
        "Loaded operations:\n{}",
        serde_json::to_string_pretty(&operations)?
    );

    Ok(operations)
}
