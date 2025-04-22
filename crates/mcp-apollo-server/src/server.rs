use crate::errors::{McpError, ServerError};
use crate::graphql;
use crate::graphql::Executable;
use crate::introspection::{EXECUTE_TOOL_NAME, Execute, GET_SCHEMA_TOOL_NAME, GetSchema};
use crate::operations::Operation;
use apollo_compiler::Schema;
use apollo_compiler::validation::Valid;
use buildstructor::buildstructor;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use rmcp::model::{
    CallToolRequestParam, CallToolResult, Content, ErrorCode, ListToolsResult,
    PaginatedRequestParam, ServerCapabilities, ServerInfo,
};
use rmcp::serde_json::Value;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, serde_json};
use rover_client::operations::persisted_queries::publish::ApolloPersistedQueryManifest;
use std::path::Path;
use std::str::FromStr;
use tracing::info;

pub use rmcp::ServiceExt;
pub use rmcp::transport::SseServer;
pub use rmcp::transport::stdio;

/// An MCP Server for Apollo GraphQL operations
#[derive(Clone)]
pub struct Server {
    operations: Vec<Operation>,
    endpoint: String,
    default_headers: HeaderMap,
    execute_tool: Option<Execute>,
    get_schema_tool: Option<GetSchema>,
}

#[buildstructor]
impl Server {
    #[builder]
    pub fn new<P: AsRef<Path>>(
        schema: Valid<Schema>,
        operations: Vec<P>,
        endpoint: String,
        headers: Vec<String>,
        introspection: bool,
        persisted_query_manifest: Option<ApolloPersistedQueryManifest>,
    ) -> Result<Self, ServerError> {
        // Load operations
        let mut operations = operations
            .into_iter()
            .map(|operation| {
                info!(operation_path=?operation.as_ref(), "Loading operation");
                let operation = std::fs::read_to_string(operation)?;
                Operation::from_document(&operation, &schema, None)
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Optionally load queries from a persisted query manifest
        if let Some(pq_manifest) = persisted_query_manifest {
            operations.extend(Operation::from_manifest(&schema, pq_manifest)?);
        }

        info!(
            "Loaded operations:\n{}",
            serde_json::to_string_pretty(&operations)?
        );

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

        Ok(Self {
            operations,
            endpoint,
            default_headers,
            execute_tool: if introspection {
                Some(Execute::new())
            } else {
                None
            },
            get_schema_tool: if introspection {
                Some(GetSchema::new(schema))
            } else {
                None
            },
        })
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

    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
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
