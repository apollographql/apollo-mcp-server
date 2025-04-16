use crate::operations::Operation;
use crate::{OperationsList, ServerError};
use apollo_compiler::parser::Parser;
use futures_util::TryFutureExt;
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
    endpoint: String,
}

impl Server {
    pub fn from_operations<P: AsRef<Path>>(
        schema: P,
        operations: P,
        endpoint: String,
    ) -> Result<Self, ServerError> {
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

        let operations = std::fs::File::open(&operations)?;
        let operations: OperationsList = serde_json::from_reader(operations)?;
        let operations = operations
            .into_iter()
            .map(|operation| Operation::from_document(&operation.query, &graphql_schema, None))
            .collect::<Result<Vec<_>, _>>()?;
        info!(
            "Loaded operations:\n{}",
            serde_json::to_string_pretty(&operations)?
        );

        Ok(Self {
            operations,
            endpoint,
        })
    }
}

impl ServerHandler for Server {
    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
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
            .execute(&self.endpoint, Value::from(request.arguments))
            .map_err(|err| McpError {
                code: ErrorCode::INTERNAL_ERROR,
                message: "could not execute graphql request".into(),
                data: Some(serde_json::Value::String(err.to_string())),
            })
            .and_then(async |result| Content::json(result))
            .map_ok(|result| CallToolResult {
                content: vec![result],
                is_error: None,
            })
            .await
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
                .collect(),
        })
    }
}
