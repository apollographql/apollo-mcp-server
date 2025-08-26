use apollo_mcp_registry::uplink::schema::SchemaSource;
use bon::bon;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use schemars::JsonSchema;
use serde::Deserialize;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::auth;
use crate::custom_scalar_map::CustomScalarMap;
use crate::errors::ServerError;
use crate::event::Event as ServerEvent;
use crate::health::HealthCheckConfig;
use crate::operations::{MutationMode, OperationSource};
use crate::server_config::ServerConfig;
use crate::server_handler::{ApolloMcpServerHandler, McpServerHandler};

pub mod states;

use states::StateMachine;

/// An Apollo MCP Server
pub struct Server<T: McpServerHandler> {
    schema_source: SchemaSource,
    operation_source: OperationSource,
    server_handler: T,
    cancellation_token: CancellationToken,
    server_config: ServerConfig,
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
        /// Authentication configuration
        #[serde(default)]
        auth: Option<auth::Config>,

        /// The IP address to bind to
        #[serde(default = "Transport::default_address")]
        address: IpAddr,

        /// The port to bind to
        #[serde(default = "Transport::default_port")]
        port: u16,
    },

    /// Host the MCP server on the configuration, using streamable HTTP messages.
    StreamableHttp {
        /// Authentication configuration
        #[serde(default)]
        auth: Option<auth::Config>,

        /// The IP address to bind to
        #[serde(default = "Transport::default_address")]
        address: IpAddr,

        /// The port to bind to
        #[serde(default = "Transport::default_port")]
        port: u16,
    },
}

impl Transport {
    fn default_address() -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }

    fn default_port() -> u16 {
        5000
    }
}

#[bon]
impl<T: McpServerHandler + Clone> Server<T> {
    #[builder]
    pub fn new(
        schema_source: SchemaSource,
        operation_source: OperationSource,
        server_handler: T,
        cancellation_token: CancellationToken,
        server_config: ServerConfig,
    ) -> Self {
        Self {
            schema_source,
            operation_source,
            server_handler,
            cancellation_token,
            server_config,
        }
    }

    pub async fn start(self) -> Result<(), ServerError> {
        StateMachine {}.start(self).await
    }
}
