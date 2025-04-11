use std::sync::Arc;

use rmcp::{handler::server::tool::{ToolBoxItem, ToolCallContext}, model::{object, CallToolResult, ErrorData, Tool}, schemars::schema::RootSchema, serde_json::json, ServerHandler};

#[derive(Debug, Clone, Default)]
pub struct Server {}

impl ServerHandler for Server {
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParam,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::Error> {
        let tcc = ToolCallContext::new(self, request, context);
        Self::execute_operation(self, tcc).await
    }
}


impl Server {
    pub async fn execute_operation(&self, _operation: ToolCallContext<'_, Server>) -> Result<rmcp::model::CallToolResult, ErrorData> {
        Ok(CallToolResult::error(vec![]))
    }
}


pub fn json_schema_to_toolbox(schema: RootSchema) {
    let tool = Tool::new("name", "description", Arc::new(object(json!(schema))));
    let _toolbox_item = ToolBoxItem::new(tool, |_tool_call_context: ToolCallContext<'_, Server>| {
        Box::pin(async { Err(rmcp::model::ErrorData::new(rmcp::model::ErrorCode::RESOURCE_NOT_FOUND, "An error occurred", None)) })
    });
}