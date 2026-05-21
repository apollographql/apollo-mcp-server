//! Multi-graph MCP `ServerHandler`.
//!
//! Implements the rmcp `ServerHandler` trait for a `HashMap<String, GraphContext>`,
//! routing the four built-in tools (execute/search/introspect/validate) to the
//! dispatcher functions in [`crate::graphs::dispatch`]. Operation-as-tool names
//! are aggregated across every configured graph.

use std::sync::Arc;

use parking_lot::Mutex;
use rmcp::ErrorData;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, ErrorCode, GetPromptRequestParams,
    GetPromptResult, Implementation, InitializeRequestParams, InitializeResult, JsonObject,
    ListPromptsResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ProtocolVersion, ServerCapabilities, ServerInfo, Tool, ToolsCapability,
};
use rmcp::schemars::{self, JsonSchema};
use rmcp::serde_json;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use serde::Deserialize;

use crate::introspection::tools::execute::{EXECUTE_TOOL_NAME, Execute};
use crate::operations::MutationMode;
use apollo_mcp_rhai::RhaiEngine;

use super::dispatch::{
    Graphs, dispatch_execute, dispatch_introspect, dispatch_search, dispatch_validate,
};

const SEARCH_TOOL_NAME: &str = "search";
const INTROSPECT_TOOL_NAME: &str = "introspect";
const VALIDATE_TOOL_NAME: &str = "validate";

#[derive(JsonSchema, Deserialize)]
#[allow(dead_code)]
struct ExecuteArgs {
    /// The namespace of the graph to execute against.
    graph: String,
    /// The GraphQL operation
    query: String,
    /// Variables as JSON
    #[serde(default)]
    variables: Option<serde_json::Value>,
}

#[derive(JsonSchema, Deserialize)]
#[allow(dead_code)]
struct SearchArgs {
    terms: Vec<String>,
    /// Optional graph namespace; omit to fan out across all graphs.
    #[serde(default)]
    graph: Option<String>,
}

#[derive(JsonSchema, Deserialize)]
#[allow(dead_code)]
struct IntrospectArgs {
    graph: String,
    #[serde(default)]
    type_name: Option<String>,
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(JsonSchema, Deserialize)]
#[allow(dead_code)]
struct ValidateArgs {
    graph: String,
    query: String,
}

fn schema_for<T: schemars::JsonSchema>() -> rmcp::model::JsonObject {
    let schema = schemars::schema_for!(T);
    // schemars always produces a JSON object schema; fall back to an empty
    // object on the impossible serialization-failure path.
    match serde_json::to_value(schema) {
        Ok(serde_json::Value::Object(obj)) => obj,
        _ => rmcp::model::JsonObject::new(),
    }
}

#[derive(Clone)]
pub struct MultiGraphServer {
    graphs: Graphs,
    execute_tool: Execute,
    rhai_engine: Arc<Mutex<RhaiEngine>>,
    search_leaf_depth: usize,
    introspect_minify: bool,
    search_minify: bool,
    server_name: String,
    server_version: String,
    tools: Vec<Tool>,
}

pub struct MultiGraphServerOptions {
    pub server_name: String,
    pub server_version: String,
    pub search_leaf_depth: usize,
    pub search_minify: bool,
    pub introspect_minify: bool,
}

impl Default for MultiGraphServerOptions {
    fn default() -> Self {
        Self {
            server_name: "apollo-mcp-server".to_string(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
            search_leaf_depth: 1,
            search_minify: false,
            introspect_minify: false,
        }
    }
}

impl MultiGraphServer {
    pub async fn new(graphs: Graphs, options: MultiGraphServerOptions) -> Self {
        let execute_tool = Execute::new(MutationMode::None, None);
        let rhai_engine = Arc::new(Mutex::new(RhaiEngine::new("rhai")));

        let mut tools = vec![
            Tool::new(
                EXECUTE_TOOL_NAME,
                "Execute a GraphQL operation against a specific graph. Required `graph` argument selects the upstream.",
                schema_for::<ExecuteArgs>(),
            ),
            Tool::new(
                SEARCH_TOOL_NAME,
                "Search GraphQL schemas. Optional `graph` argument scopes to a single graph; omit to search every configured graph.",
                schema_for::<SearchArgs>(),
            ),
            Tool::new(
                INTROSPECT_TOOL_NAME,
                "Introspect a graph's schema. Required `graph` argument.",
                schema_for::<IntrospectArgs>(),
            ),
            Tool::new(
                VALIDATE_TOOL_NAME,
                "Validate a query against a graph's schema. Required `graph` and `query` arguments.",
                schema_for::<ValidateArgs>(),
            ),
        ];

        // Add prefixed operation tools from each graph.
        {
            let read = graphs.read().await;
            for ctx in read.values() {
                let ops = ctx.operations.read().await;
                for op in ops.iter() {
                    tools.push(op.as_ref().clone());
                }
            }
        }

        Self {
            graphs,
            execute_tool,
            rhai_engine,
            search_leaf_depth: options.search_leaf_depth,
            search_minify: options.search_minify,
            introspect_minify: options.introspect_minify,
            server_name: options.server_name,
            server_version: options.server_version,
            tools,
        }
    }
}

impl ServerHandler for MultiGraphServer {
    async fn initialize(
        &self,
        _request: InitializeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        Ok(self.get_info())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            next_cursor: None,
            tools: self.tools.clone(),
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let args: Option<&JsonObject> = request.arguments.as_ref();
        let name = request.name.as_ref();

        match name {
            EXECUTE_TOOL_NAME => {
                dispatch_execute(&self.graphs, &self.execute_tool, args, None, &self.rhai_engine)
                    .await
            }
            SEARCH_TOOL_NAME => {
                dispatch_search(
                    &self.graphs,
                    self.search_leaf_depth,
                    self.search_minify,
                    args,
                )
                .await
            }
            INTROSPECT_TOOL_NAME => {
                dispatch_introspect(&self.graphs, self.introspect_minify, args).await
            }
            VALIDATE_TOOL_NAME => dispatch_validate(&self.graphs, args).await,
            _ => Ok(CallToolResult::error(vec![Content::text(format!(
                "Tool '{name}' not found"
            ))])),
        }
        .map_err(|e: rmcp::model::ErrorData| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, e.to_string(), None)
        })
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult {
            resources: vec![],
            next_cursor: None,
            meta: None,
        })
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult {
            prompts: vec![],
            next_cursor: None,
            meta: None,
        })
    }

    async fn get_prompt(
        &self,
        _request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        Err(ErrorData::method_not_found::<rmcp::model::GetPromptRequestMethod>())
    }

    fn get_info(&self) -> ServerInfo {
        let mut capabilities = ServerCapabilities::default();
        capabilities.tools = Some(ToolsCapability {
            list_changed: Some(false),
        });
        InitializeResult::new(capabilities)
            .with_protocol_version(ProtocolVersion::V_2025_03_26)
            .with_server_info(
                Implementation::new(self.server_name.clone(), self.server_version.clone())
                    .with_description("Apollo MCP multi-graph server"),
            )
    }
}
