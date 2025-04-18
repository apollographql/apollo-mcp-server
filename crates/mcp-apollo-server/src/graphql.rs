//! Execute GraphQL operations from an MCP tool

use crate::errors::McpError;
use apollo_compiler::response::serde_json_bytes::serde_json;
use apollo_compiler::response::serde_json_bytes::serde_json::Value;
use reqwest::header::HeaderMap;
use rmcp::model::{CallToolResult, Content, ErrorCode};

pub struct Request<'a> {
    pub input: Value,
    pub endpoint: &'a str,
    pub headers: HeaderMap,
}

/// Able to be executed as a GraphQL operation
pub trait Executable {
    /// Get the operation to execute
    fn operation(&self, input: Value) -> Result<String, McpError>;

    /// Get the variables to execute the operation with
    fn variables(&self, input: Value) -> Result<Value, McpError>;

    /// Execute as a GraphQL operation using the endpoint and headers
    async fn execute(&self, request: Request<'_>) -> Result<CallToolResult, McpError> {
        reqwest::Client::new()
            .post(request.endpoint)
            .headers(request.headers)
            .body(
                serde_json::json!({
                    "query": self.operation(request.input.clone())?,
                    "variables": self.variables(request.input)?,
                })
                .to_string(),
            )
            .send()
            .await
            .map_err(|reqwest_error| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to send GraphQL request: {reqwest_error}"),
                    None,
                )
            })?
            .text()
            .await
            .map_err(|reqwest_error| {
                McpError::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Failed to read GraphQL response body: {reqwest_error}"),
                    None,
                )
            })
            .and_then(Content::json)
            .map(|result| CallToolResult {
                content: vec![result],
                is_error: None,
            })
    }
}
