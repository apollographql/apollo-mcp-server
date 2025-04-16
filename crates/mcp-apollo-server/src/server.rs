use crate::operations::Operation;
use apollo_compiler::parser::Parser;
use futures_util::future::FutureExt;
use rmcp::model::{
    CallToolRequestParam, CallToolResult, Content, ErrorCode, ListToolsResult,
    PaginatedRequestParam,
};
use rmcp::serde_json::Value;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, serde_json};
use std::path::Path;
use tracing::info;

type McpError = rmcp::model::ErrorData;

/// An MCP Server for Apollo GraphQL operations
#[derive(Clone)]
pub struct Server {
    operations: Vec<Operation>,
}

impl Server {
    pub fn new<P: AsRef<Path>>(schema: P, operations: P) -> Self {
        let schema_path = schema.as_ref();
        info!(schema_path=?schema_path, "Loading schema");
        let graphql_schema = std::fs::read_to_string(schema_path).unwrap();
        let mut parser = Parser::new();
        let graphql_schema = parser.parse_ast(graphql_schema, schema_path).unwrap();
        let graphql_schema = graphql_schema.to_schema().unwrap();

        let operations = std::fs::read_to_string(operations.as_ref()).unwrap();
        let operations: Value =
            serde_json::from_str(&operations).expect("Operations must be valid JSON");
        let operations = operations.as_array().expect("Operations must be an array");
        let operations = operations
            .iter()
            .map(|operation| {
                let operation = &operation["query"]
                    .as_str()
                    .expect("Operation must be a string");
                Operation::new(operation, &graphql_schema, None)
            })
            .collect();
        info!(?operations, "Loaded operations");

        Self { operations }
    }
}

impl ServerHandler for Server {
    fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        const ENDPOINT: &str = "http://127.0.0.1:4000";

        Box::pin(async move {
            self.operations
                .iter()
                .find(|op| op.as_ref().name == request.name)
                .ok_or_else(|| {
                    McpError::new(
                        ErrorCode::METHOD_NOT_FOUND,
                        format!("Tool {} not found", request.name),
                        None,
                    )
                })?
                .execute(ENDPOINT, Value::from(request.arguments))
                .map(|result| {
                    Ok(CallToolResult {
                        content: vec![Content::json(result.unwrap()).unwrap()],
                        is_error: None,
                    })
                })
                .await
        })
    }

    fn list_tools(
        &self,
        _request: PaginatedRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult {
            next_cursor: None,
            tools: self
                .operations
                .iter()
                .map(|op| op.as_ref().clone())
                .collect(),
        }))
    }
}
