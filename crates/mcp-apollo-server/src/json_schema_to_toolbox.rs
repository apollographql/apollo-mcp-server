use std::sync::Arc;

use rmcp::{handler::server::tool::{ToolBox, ToolBoxItem, ToolCallContext}, model::{object, CallToolResult, ErrorData, Tool}, serde_json::json, ServerHandler};

use crate::operations::ToolDefinition;

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


pub fn json_schema_to_toolbox(definition: ToolDefinition, mut tool_box: ToolBox<Server>) {
    let tool = Tool::new(
        definition.name,
        definition.description,
        // TODO: validate this works? 
        Arc::new(object(json!(definition.schema)))
    );

    let toolbox_item = ToolBoxItem::new(
        tool, 
        |_tool_call_context: ToolCallContext<'_, Server>| {
            Box::pin(async { Err(rmcp::model::ErrorData::new(rmcp::model::ErrorCode::RESOURCE_NOT_FOUND, "An error occurred", None)) })
        }
    );
    tool_box.add(toolbox_item);
}