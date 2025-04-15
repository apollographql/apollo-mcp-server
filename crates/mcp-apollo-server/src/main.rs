use mcp_apollo_server::server::Server;
use rmcp::ServiceExt;
use rmcp::transport::stdio;
use std::env;
use std::io::Error;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Error> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive(tracing::Level::DEBUG.into()))
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let args: Vec<String> = env::args().collect();
    let working_directory = args.get(1).map(|s| s.as_str()).unwrap_or(".");
    let _ = env::set_current_dir(working_directory);

    tracing::info!("Starting MCP server");
    let server = Server::new("graphql/weather.graphql", "graphql/operations.json");
    let service = server.serve(stdio()).await.inspect_err(|e| {
        tracing::error!("serving error: {:?}", e);
    })?;

    service.waiting().await?;
    Ok(())
}
