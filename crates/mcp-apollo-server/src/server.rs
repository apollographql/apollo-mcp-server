use crate::OperationsList;
use crate::errors::ServerError;
use futures_util::TryFutureExt;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use rmcp::model::{
    CallToolRequestParam, CallToolResult, Content, ErrorCode, ListToolsResult,
    PaginatedRequestParam,
};
use rmcp::serde_json::Value;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use std::str::FromStr;

type McpError = rmcp::model::ErrorData;

/// An MCP Server for Apollo GraphQL operations
#[derive(Clone)]
pub struct Server {
    operations: OperationsList,
    endpoint: String,
    default_headers: HeaderMap,
}

impl Server {
    pub fn from_operations(
        endpoint: String,
        headers: Vec<String>,
        operations: OperationsList,
    ) -> Result<Self, ServerError> {
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
            .execute(
                &self.endpoint,
                Value::from(request.arguments),
                self.default_headers.clone(),
            )
            .map_err(|err| McpError {
                code: ErrorCode::INTERNAL_ERROR,
                message: "could not execute graphql request".into(),
                data: Some(Value::String(err.to_string())),
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
