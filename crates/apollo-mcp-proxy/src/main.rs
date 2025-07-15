use apollo_mcp_proxy::client::start_proxy_client;
use clap::Parser;
use std::error::Error;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser)]
struct Args {
    #[arg(short, long)]
    url: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Initialize logging
    let file_appender = RollingFileAppender::builder()
        .rotation(Rotation::NEVER)
        .filename_prefix("apollo_mcp_proxy")
        .filename_suffix("log")
        .build("./logs")?;

    let (non_blocking_writer, _guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_writer(non_blocking_writer))
        .init();

    let args = Args::parse();

    let _ = start_proxy_client(&args.url).await;

    Ok(())
}
