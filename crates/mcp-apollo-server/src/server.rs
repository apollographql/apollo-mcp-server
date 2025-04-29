use crate::custom_scalar_map::CustomScalarMap;
use crate::errors::{McpError, OperationError, ServerError};
use crate::graphql;
use crate::graphql::Executable;
use crate::introspection::{EXECUTE_TOOL_NAME, Execute, GET_SCHEMA_TOOL_NAME, GetSchema};
use crate::operations::Operation;
use buildstructor::buildstructor;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use rmcp::model::{
    CallToolRequestParam, CallToolResult, Content, ErrorCode, ListToolsResult,
    PaginatedRequestParam, ServerCapabilities, ServerInfo,
};
use rmcp::serde_json::Value;
use rmcp::service::RequestContext;
use rmcp::{Peer, RoleServer, ServerHandler, ServiceError, serde_json};
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};

pub use apollo_compiler::Schema;
pub use apollo_compiler::validation::Valid;
use mcp_apollo_registry::uplink::persisted_queries::{
    ManifestChanged, ManifestSource, PersistedQueryManifestPoller,
};
use mcp_apollo_registry::uplink::{SecretString, UplinkConfig};
pub use rmcp::ServiceExt;
pub use rmcp::transport::SseServer;
pub use rmcp::transport::sse_server::SseServerConfig;
pub use rmcp::transport::stdio;
use tokio::sync::{RwLock, mpsc};

/// An MCP Server for Apollo GraphQL operations
#[derive(Clone)]
pub struct Server {
    schema: Valid<Schema>,
    operations: Vec<Operation>,
    endpoint: String,
    default_headers: HeaderMap,
    execute_tool: Option<Execute>,
    get_schema_tool: Option<GetSchema>,
    manifest_poller: Option<PersistedQueryManifestPoller>,
    peers: Arc<RwLock<Vec<Peer<RoleServer>>>>,
}

#[buildstructor]
impl Server {
    #[builder]
    pub async fn new<P: 'static + AsRef<Path> + Sync + Send + Clone>(
        schema: Valid<Schema>,
        operations: Vec<P>,
        endpoint: String,
        headers: Vec<String>,
        introspection: bool,
        uplink: bool,
        manifests: Vec<P>,
        custom_scalar_map: Option<CustomScalarMap>,
    ) -> Result<Self, ServerError> {
        // Load operations from static files
        let operations = operations
            .into_iter()
            .map(|operation| {
                info!(operation_path=?operation.as_ref(), "Loading operation");
                let operation = std::fs::read_to_string(operation)?;
                Operation::from_document(&operation, &schema, None, custom_scalar_map.as_ref())
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !operations.is_empty() {
            info!(
                "Loaded {} operations from local operations files:\n{}",
                operations.len(),
                serde_json::to_string_pretty(&operations)?
            );
        }

        if operations.is_empty() && manifests.is_empty() && !uplink {
            return Err(ServerError::NoOperations);
        }

        // Set up the manifest poller to pull operations from uplink or a manifest file
        let manifest_source =
            if !manifests.is_empty() {
                Some(ManifestSource::LocalHotReload(manifests))
            } else if uplink {
                Some(ManifestSource::Uplink(UplinkConfig {
                    apollo_key: SecretString::from(std::env::var("APOLLO_KEY").map_err(|_| {
                        ServerError::EnvironmentVariable(String::from("APOLLO_KEY"))
                    })?),
                    apollo_graph_ref: std::env::var("APOLLO_GRAPH_REF").map_err(|_| {
                        ServerError::EnvironmentVariable(String::from("APOLLO_GRAPH_REF"))
                    })?,
                    poll_interval: Duration::from_secs(10),
                    timeout: Duration::from_secs(30),
                    endpoints: None, // Use the default endpoints
                }))
            } else {
                None
            };
        let (manifest_poller, change_receiver) = if let Some(manifest_source) = manifest_source {
            let (change_sender, change_receiver) = mpsc::channel::<ManifestChanged>(1);

            (
                PersistedQueryManifestPoller::new(manifest_source, change_sender)
                    .await
                    .ok(),
                Some(change_receiver),
            )
        } else {
            (None, None)
        };

        // Load headers
        let mut default_headers = HeaderMap::new();
        default_headers.append(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        for header in headers {
            let parts: Vec<&str> = header.split(':').collect();
            match (parts.first(), parts.get(1), parts.get(2)) {
                (Some(key), Some(value), None) => {
                    default_headers
                        .append(HeaderName::from_str(key)?, HeaderValue::from_str(value)?);
                }
                _ => return Err(ServerError::Header(header)),
            }
        }

        let execute_tool = if introspection {
            Some(Execute::new())
        } else {
            None
        };
        let get_schema_tool = if introspection {
            Some(GetSchema::new(schema.clone()))
        } else {
            None
        };

        let peers = Arc::new(RwLock::new(vec![]));

        if let Some(change_receiver) = change_receiver {
            Self::spawn_change_listener(change_receiver, peers.clone());
        }

        Ok(Self {
            schema,
            operations,
            endpoint,
            default_headers,
            execute_tool,
            get_schema_tool,
            manifest_poller,
            peers,
        })
    }

    fn spawn_change_listener(
        mut change_receiver: mpsc::Receiver<ManifestChanged>,
        peers: Arc<RwLock<Vec<Peer<RoleServer>>>>,
    ) {
        tokio::spawn(async move {
            while change_receiver.recv().await.is_some() {
                let mut peers = peers.write().await;
                if !peers.is_empty() {
                    info!(
                        "Persisted query manifest changed, notifying {} peers of tool change",
                        peers.len()
                    );
                }
                let mut retained_peers = Vec::new();
                for peer in peers.iter() {
                    match peer.notify_tool_list_changed().await {
                        Ok(_) => retained_peers.push(peer.clone()),
                        Err(e) => {
                            match e {
                                ServiceError::Transport(e) => {
                                    if let Some(error_data) = e.get_ref() {
                                        if error_data.to_string() == *"disconnected" {
                                            // TODO: this always gets a "disconnected" error due to a bug in the SDK, but it actually works
                                            retained_peers.push(peer.clone());
                                        } else {
                                            error!(
                                                "Failed to notify peer of tool list change: {:?} - dropping peer",
                                                error_data
                                            );
                                        }
                                    } else {
                                        error!(
                                            "Failed to notify peer of tool list change: {:?}",
                                            e
                                        );
                                        retained_peers.push(peer.clone());
                                    }
                                }
                                _ => {
                                    error!("Failed to notify peer of tool list change {:?}", e);
                                    retained_peers.push(peer.clone());
                                }
                            }
                        }
                    }
                }
                *peers = retained_peers;
            }
        });
    }

    /// Get the current set of operations from the manifest, if any
    fn manifest_operations(&self) -> Result<Vec<Operation>, McpError> {
        if let Some(manifest_poller) = self.manifest_poller.as_ref() {
            manifest_poller
                .get_all_operations()
                .into_iter()
                .map(|(pq_id, operation)| {
                    Operation::from_document(&operation, &self.schema, Some(pq_id), None)
                })
                .collect::<Result<Vec<Operation>, OperationError>>()
                .map_err(|e| McpError::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None))
        } else {
            Ok(vec![])
        }
    }
}

impl ServerHandler for Server {
    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        if request.name == GET_SCHEMA_TOOL_NAME {
            let get_schema = self.get_schema_tool.as_ref().ok_or(McpError::new(
                ErrorCode::METHOD_NOT_FOUND,
                format!("Tool {} not found", request.name),
                None,
            ))?;
            Ok(CallToolResult {
                content: vec![Content::text(get_schema.schema.to_string())],
                is_error: None,
            })
        } else {
            let graphql_request = graphql::Request {
                input: Value::from(request.arguments.clone()),
                endpoint: &self.endpoint,
                headers: self.default_headers.clone(),
            };
            if request.name == EXECUTE_TOOL_NAME {
                self.execute_tool
                    .as_ref()
                    .ok_or(tool_not_found(&request.name))?
                    .execute(graphql_request)
                    .await
            } else {
                self.operations
                    .iter()
                    .chain(self.manifest_operations()?.iter())
                    .find(|op| op.as_ref().name == request.name)
                    .ok_or(tool_not_found(&request.name))?
                    .execute(graphql_request)
                    .await
            }
        }
    }

    async fn list_tools(
        &self,
        _request: PaginatedRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            next_cursor: None,
            tools: self
                .operations
                .iter()
                .chain(self.manifest_operations()?.iter())
                .map(|op| op.as_ref().clone())
                .chain(
                    self.execute_tool
                        .as_ref()
                        .iter()
                        .clone()
                        .map(|e| e.tool.clone()),
                )
                .chain(
                    self.get_schema_tool
                        .as_ref()
                        .iter()
                        .clone()
                        .map(|e| e.tool.clone()),
                )
                .collect(),
        })
    }

    fn set_peer(&mut self, p: Peer<RoleServer>) {
        let peers = self.peers.clone();
        tokio::spawn(async move {
            let mut peers = peers.write().await;
            // TODO: we need a way to remove these! The Rust SDK seems to leek running servers
            //  forever - it never times them out or disconnects them.
            peers.push(p);
        });
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
            ..Default::default()
        }
    }
}

fn tool_not_found(name: &str) -> McpError {
    McpError::new(
        ErrorCode::METHOD_NOT_FOUND,
        format!("Tool {} not found", name),
        None,
    )
}
