use crate::errors::{McpError, ServerError};
use crate::graphql;
use crate::graphql::Executable;
use crate::introspection::{EXECUTE_TOOL_NAME, Execute, GET_SCHEMA_TOOL_NAME, GetSchema};
use crate::operations::Operation;
use apollo_compiler::parser::Parser;
use buildstructor::buildstructor;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use rmcp::model::{
    CallToolRequestParam, CallToolResult, Content, ErrorCode, ListToolsResult,
    PaginatedRequestParam,
};
use rmcp::serde_json::Value;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, serde_json};
use std::path::Path;
use std::str::FromStr;
use tracing::info;

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
        schema: P,
        operations: Vec<P>,
        endpoint: String,
        headers: Vec<String>,
        introspection: bool,
    ) -> Result<Self, ServerError> {
        // Load GraphQL schema
        let schema_path = schema.as_ref();
        info!(schema_path=?schema_path, "Loading schema");
        let graphql_schema = std::fs::read_to_string(schema_path)?;
        let mut parser = Parser::new();
        let graphql_schema = parser
            .parse_ast(graphql_schema, schema_path)
            .map_err(|e| ServerError::GraphQLDocument(Box::new(e)))?;
        let graphql_schema = graphql_schema
            .to_schema()
            .map_err(|e| ServerError::GraphQLSchema(Box::new(e)))?;

        let operations = operations
            .into_iter()
            .map(|operation| {
                info!(operation_path=?operation.as_ref(), "Loading operation");
                let operation = std::fs::read_to_string(operation)?;
                Operation::from_document(&operation, &graphql_schema, None)
            })
            .collect::<Result<Vec<_>, _>>()?;
        info!(
            "Loaded operations:\n{}",
            serde_json::to_string_pretty(&operations)?
        );

        // Load operations
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
                Some(GetSchema::new(graphql_schema))
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
}

fn tool_not_found(name: &str) -> McpError {
    McpError::new(
        ErrorCode::METHOD_NOT_FOUND,
        format!("Tool {} not found", name),
        None,
    )
}
