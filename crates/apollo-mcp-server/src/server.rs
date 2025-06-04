use crate::custom_scalar_map::CustomScalarMap;
use crate::errors::ServerError;
use crate::operations::{MutationMode, OperationSource};
use crate::states::StateMachine;
use bon::bon;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use std::net::IpAddr;

pub use apollo_mcp_registry::uplink::UplinkConfig;
pub use apollo_mcp_registry::uplink::persisted_queries::ManifestSource;
pub use apollo_mcp_registry::uplink::schema::SchemaSource;
pub use rmcp::ServiceExt;
pub use rmcp::transport::SseServer;
pub use rmcp::transport::sse_server::SseServerConfig;
pub use rmcp::transport::stdio;

/// An Apollo MCP Server
pub struct Server {
    pub(crate) transport: Transport,
    pub(crate) schema_source: SchemaSource,
    pub(crate) operation_source: OperationSource,
    pub(crate) endpoint: String,
    pub(crate) headers: HeaderMap,
    pub(crate) introspection: bool,
    pub(crate) explorer: bool,
    pub(crate) custom_scalar_map: Option<CustomScalarMap>,
    pub(crate) mutation_mode: MutationMode,
    pub(crate) disable_type_description: bool,
    pub(crate) disable_schema_description: bool,
}

#[derive(Clone)]
pub enum Transport {
    Stdio,
    SSE { address: IpAddr, port: u16 },
    StreamableHttp { address: IpAddr, port: u16 },
}

#[bon]
impl Server {
    #[builder]
    pub fn new(
        transport: Transport,
        schema_source: SchemaSource,
        operation_source: OperationSource,
        endpoint: String,
        headers: HeaderMap,
        introspection: bool,
        explorer: bool,
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
            introspection,
            explorer,
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
