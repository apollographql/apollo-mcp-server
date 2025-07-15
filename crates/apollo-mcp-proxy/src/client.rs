use crate::server::ProxyServer;
use rmcp::transport::stdio;
use rmcp::{
    ServiceExt,
    model::{ClientCapabilities, ClientInfo, Implementation},
    transport::StreamableHttpClientTransport,
};
use std::error::Error;
use tracing::{debug, error, info};

pub async fn start_proxy_client(url: &str) -> Result<(), Box<dyn Error>> {
    let transport = StreamableHttpClientTransport::from_uri(url);
    let client_info = ClientInfo {
        protocol_version: Default::default(),
        capabilities: ClientCapabilities::default(),
        client_info: Implementation {
            name: "mcp remote rust client".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
    };

    let client = match client_info.serve(transport).await.inspect_err(|e| {
        error!("[Proxy] client error: {:?}", e);
    }) {
        Ok(client) => client,
        Err(e) => {
            error!("[Proxy] client startup error: {:?}", e);
            return Err(e.into());
        }
    };

    let server_info = client.peer_info();
    info!("[Proxy] Connected to server at {}", url);
    debug!("{server_info:#?}");

    let proxy_server = ProxyServer::new(client.peer().clone(), client.peer_info());
    let stdio_transport = stdio();
    let server = proxy_server.serve(stdio_transport).await?;

    server.waiting().await?;
    Ok(())
}
