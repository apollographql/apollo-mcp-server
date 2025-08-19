use schemars::JsonSchema;
use serde::Deserialize;
use std::net::IpAddr;
use url::Url;

/// Proxy client related config options
/// Note: only supported for Streamable HTTP
#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(default)]
pub struct ProxyConfig {
    /// Flag indicating whether the proxy client is enabled or not
    #[serde(default)]
    pub enabled: bool,

    /// Proxy url address
    /// [default: http://<transport.address>:<transport.port>/mcp]
    #[serde(default)]
    pub url: Option<Url>,
}

impl ProxyConfig {
    pub(crate) fn url(&self, transport_address: &IpAddr, transport_port: &u16) -> Url {
        match &self.url {
            Some(proxy_url) => proxy_url.clone(),
            None => {
                let address = format!("http://{transport_address}:{transport_port}/mcp");
                #[allow(clippy::unwrap_used)]
                Url::parse(address.as_str()).unwrap()
            }
        }
    }
}
