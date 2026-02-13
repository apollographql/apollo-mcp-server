use std::net::{IpAddr, Ipv4Addr};

use apollo_mcp_registry::uplink::schema::SchemaSource;
use bon::bon;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use schemars::JsonSchema;
use serde::Deserialize;
use url::Url;

use crate::auth;
use crate::cors::CorsConfig;
use crate::custom_scalar_map::CustomScalarMap;
use crate::errors::ServerError;
use crate::event::Event as ServerEvent;
use crate::headers::ForwardHeaders;
use crate::health::HealthCheckConfig;
use crate::host_validation::HostValidationConfig;
use crate::operations::{MutationMode, OperationSource};
use crate::server_info::ServerInfoConfig;

mod states;

use states::StateMachine;

/// An Apollo MCP Server
pub struct Server {
    transport: Transport,
    schema_source: SchemaSource,
    operation_source: OperationSource,
    endpoint: Url,
    headers: HeaderMap,
    forward_headers: ForwardHeaders,
    execute_introspection: bool,
    validate_introspection: bool,
    introspect_introspection: bool,
    introspect_minify: bool,
    search_minify: bool,
    search_introspection: bool,
    execute_tool_hint: Option<String>,
    introspect_tool_hint: Option<String>,
    search_tool_hint: Option<String>,
    validate_tool_hint: Option<String>,
    explorer_graph_ref: Option<String>,
    custom_scalar_map: Option<CustomScalarMap>,
    mutation_mode: MutationMode,
    disable_type_description: bool,
    disable_schema_description: bool,
    enable_output_schema: bool,
    disable_auth_token_passthrough: bool,
    search_leaf_depth: usize,
    index_memory_bytes: usize,
    health_check: HealthCheckConfig,
    cors: CorsConfig,
    server_info: ServerInfoConfig,
}

#[derive(Debug, Clone, Deserialize, Default, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transport {
    /// Use standard IO for server <> client communication
    #[default]
    Stdio,

    /// Host the MCP server on the configuration, using streamable HTTP messages.
    StreamableHttp {
        /// Authentication configuration
        #[serde(default)]
        auth: Option<Box<auth::Config>>,

        /// The IP address to bind to
        #[serde(default = "Transport::default_address")]
        address: IpAddr,

        /// The port to bind to
        #[serde(default = "Transport::default_port")]
        port: u16,

        /// Enable stateful mode for session management
        #[serde(default = "Transport::default_stateful_mode")]
        stateful_mode: bool,

        /// Host header validation configuration for DNS rebinding protection.
        #[serde(default)]
        host_validation: HostValidationConfig,
    },
}

impl Transport {
    fn default_address() -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }

    fn default_port() -> u16 {
        8000
    }

    fn default_stateful_mode() -> bool {
        true
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
        forward_headers: ForwardHeaders,
        execute_introspection: bool,
        validate_introspection: bool,
        introspect_introspection: bool,
        search_introspection: bool,
        introspect_minify: bool,
        search_minify: bool,
        execute_tool_hint: Option<String>,
        introspect_tool_hint: Option<String>,
        search_tool_hint: Option<String>,
        validate_tool_hint: Option<String>,
        explorer_graph_ref: Option<String>,
        #[builder(required)] custom_scalar_map: Option<CustomScalarMap>,
        mutation_mode: MutationMode,
        disable_type_description: bool,
        disable_schema_description: bool,
        enable_output_schema: bool,
        disable_auth_token_passthrough: bool,
        search_leaf_depth: usize,
        index_memory_bytes: usize,
        health_check: HealthCheckConfig,
        cors: CorsConfig,
        server_info: ServerInfoConfig,
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
            forward_headers,
            execute_introspection,
            validate_introspection,
            introspect_introspection,
            search_introspection,
            introspect_minify,
            search_minify,
            execute_tool_hint,
            introspect_tool_hint,
            search_tool_hint,
            validate_tool_hint,
            explorer_graph_ref,
            custom_scalar_map,
            mutation_mode,
            disable_type_description,
            disable_schema_description,
            enable_output_schema,
            disable_auth_token_passthrough,
            search_leaf_depth,
            index_memory_bytes,
            health_check,
            cors,
            server_info,
        }
    }

    pub async fn start(self) -> Result<(), ServerError> {
        StateMachine {}.start(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::Transport;

    #[test]
    fn sse_transport_is_rejected_at_parse_time() {
        let yaml = "type: sse\nport: 8000";
        let result = serde_yaml::from_str::<Transport>(yaml);
        assert!(result.is_err(), "Expected SSE transport to be rejected");
    }

    #[test]
    fn stdio_transport_parses() {
        let yaml = "type: stdio";
        let result = serde_yaml::from_str::<Transport>(yaml);
        assert!(result.is_ok(), "Expected stdio transport to parse");
    }

    #[test]
    fn streamable_http_transport_parses() {
        let yaml = "type: streamable_http\nport: 9000";
        let result = serde_yaml::from_str::<Transport>(yaml);
        assert!(
            result.is_ok(),
            "Expected streamable_http transport to parse"
        );
    }
}
