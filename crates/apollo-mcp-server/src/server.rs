use std::net::{IpAddr, Ipv4Addr};

use apollo_mcp_registry::uplink::schema::SchemaSource;
use bon::bon;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use schemars::JsonSchema;
use serde::Deserialize;
use url::Url;

use crate::custom_scalar_map::CustomScalarMap;
use crate::errors::ServerError;
use crate::event::Event as ServerEvent;
use crate::operations::{MutationMode, OperationSource};

mod states;

use states::StateMachine;

/// An Apollo MCP Server
pub struct Server {
    transport: Transport,
    schema_source: SchemaSource,
    operation_source: OperationSource,
    endpoint: Url,
    headers: HeaderMap,
    execute_introspection: bool,
    introspect_introspection: bool,
    search_introspection: bool,
    explorer_graph_ref: Option<String>,
    custom_scalar_map: Option<CustomScalarMap>,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
}

#[derive(Debug, Clone, Deserialize, Default, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Transport {
    /// Use standard IO for server <> client communication
    #[default]
    Stdio,

    /// Host the MCP server on the supplied configuration, using SSE for communication
    ///
    /// Note: This is deprecated in favor of HTTP streams.
    #[serde(rename = "sse")]
    SSE {
        /// The IP address to bind to
        #[serde(default = "Transport::default_address")]
        address: IpAddr,

        /// The port to bind to
        #[serde(default = "Transport::default_port")]
        port: u16,
    },

    /// Host the MCP server on the configuration, using stremable HTTP messages.
    StreamableHttp {
        /// The IP address to bind to
        #[serde(default = "Transport::default_address")]
        address: IpAddr,

        /// The port to bind to
        #[serde(default = "Transport::default_port")]
        port: u16,

        /// Flag indicating whether to spin up a proxy server
        /// [default: false]
        #[serde(default)]
        proxy: bool,

        /// Proxy server endpoint to connect to if proxy is enabled
        #[serde(default)]
        proxy_url: Option<Url>,
    },
}

impl Transport {
    fn default_address() -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }

    fn default_port() -> u16 {
        5000
    }

    pub fn proxy_url(proxy_url: &Option<Url>, address: &IpAddr, port: &u16) -> Url {
        match proxy_url {
            Some(proxy_url) => proxy_url.clone(),
            None => {
                let address = format!("http://{address}:{port}/mcp");
                #[allow(clippy::unwrap_used)]
                Url::parse(address.as_str()).unwrap()
            }
        }
    }
}

#[bon]
impl Server {
    #[builder]
    pub fn new(
        transport: Transport,
        schema_source: SchemaSource,
        operation_source: OperationSource,
        endpoint: Url,
        headers: HeaderMap,
        execute_introspection: bool,
        introspect_introspection: bool,
        search_introspection: bool,
        explorer_graph_ref: Option<String>,
        #[builder(required)] custom_scalar_map: Option<CustomScalarMap>,
        mutation_mode: MutationMode,
        disable_type_description: bool,
        disable_schema_description: bool,
    ) -> Self {
        let headers = {
            let mut headers = headers.clone();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            headers
        };
        Self {
            transport,
            schema_source,
            operation_source,
            endpoint,
            headers,
            execute_introspection,
            introspect_introspection,
            search_introspection,
            explorer_graph_ref,
            custom_scalar_map,
            mutation_mode,
            disable_type_description,
            disable_schema_description,
        }
    }

    pub async fn start(self) -> Result<(), ServerError> {
        StateMachine {}.start(self).await
    }
}
