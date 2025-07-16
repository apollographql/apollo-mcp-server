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
    validate_introspection: bool,
    introspect_introspection: bool,
    introspect_minify: bool,
    search_minify: bool,
    search_introspection: bool,
    explorer_graph_ref: Option<String>,
    custom_scalar_map: Option<CustomScalarMap>,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
    search_leaf_depth: usize,
    index_memory_bytes: usize,
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

    /// Host the MCP server on the configuration, using streamable HTTP messages.
    StreamableHttp {
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
impl Server {
    #[builder]
    pub fn new(
        transport: Transport,
        schema_source: SchemaSource,
        operation_source: OperationSource,
        endpoint: Url,
        headers: HeaderMap,
        execute_introspection: bool,
        validate_introspection: bool,
        introspect_introspection: bool,
        search_introspection: bool,
        introspect_minify: bool,
        search_minify: bool,
        explorer_graph_ref: Option<String>,
        #[builder(required)] custom_scalar_map: Option<CustomScalarMap>,
        mutation_mode: MutationMode,
        disable_type_description: bool,
        disable_schema_description: bool,
        search_leaf_depth: usize,
        index_memory_bytes: usize,
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
            validate_introspection,
            introspect_introspection,
            search_introspection,
            introspect_minify,
            search_minify,
            explorer_graph_ref,
            custom_scalar_map,
            mutation_mode,
            disable_type_description,
            disable_schema_description,
            search_leaf_depth,
            index_memory_bytes,
        }
    }

    pub async fn start(self) -> Result<(), ServerError> {
        StateMachine {}.start(self).await
    }
}
