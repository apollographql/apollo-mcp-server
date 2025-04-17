use clap::Parser;
use clap::builder::Styles;
use clap::builder::styling::{AnsiColor, Effects};
use mcp_apollo_server::server::Server;
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use std::env;
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
    directory: String,

    /// The path to the GraphQL schema file
    #[clap(long, short = 's', default_value = "graphql/weather.graphql")]
    schema: String,

    /// The path to the GraphQL operations file
    #[clap(long, short = 'o', default_value = "graphql/operations.json")]
    operations: String,

    /// The GraphQL endpoint the server will invoke
    #[clap(long, short = 'e', default_value = "http://127.0.0.1:4000")]
    endpoint: String,

    /// Headers to send to endpoint
    #[clap(long = "header", action = clap::ArgAction::Append)]
    headers: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::DEBUG.into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let args = Args::parse();
    env::set_current_dir(args.directory)?;

    tracing::info!("Starting MCP server");
    let server = Server::from_operations(args.schema, args.operations, args.endpoint, args.headers)?;
    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("serving error: {:?}", e);
    })?;

    service.waiting().await?;
    Ok(())
}
