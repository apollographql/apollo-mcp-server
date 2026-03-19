use std::sync::Arc;

use futures::TryFutureExt;
use futures::future::try_join_all;
use http::HeaderMap;
use http::request::Parts;
use opentelemetry::Context;
use opentelemetry::trace::FutureExt;
use parking_lot::Mutex;
use rmcp::model::{CallToolResult, Content, JsonObject, Meta, Tool};
use serde_json::{Map, Value, json};
use url::Url;

use crate::apps::app::{AppTarget, AppTool};
use crate::errors::McpError;
use crate::graphql::{self, Executable};
use crate::operations::Operation;
use apollo_mcp_rhai::{RhaiEngine, checkpoints};

use super::App;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn find_and_execute_app_tool(
    apps: &[App],
    app_name: &str,
    tool_name: &str,
    headers: &HeaderMap,
    arguments: Option<&JsonObject>,
    endpoint: &Url,
    rhai_engine: &Arc<Mutex<RhaiEngine>>,
    axum_parts: Option<&Parts>,
) -> Option<Result<CallToolResult, McpError>> {
    let app = apps.iter().find(|app| app.name == app_name)?;

    for tool in &app.tools {
        if tool.tool.name == tool_name {
            return Some(
                execute_app_tool(
                    app,
                    tool,
                    headers,
                    arguments,
                    endpoint,
                    rhai_engine,
                    axum_parts,
                )
                .await,
            );
        }
    }
    None
}

async fn execute_app_tool(
    app: &App,
    tool: &AppTool,
    headers: &HeaderMap,
    arguments: Option<&JsonObject>,
    endpoint: &Url,
    rhai_engine: &Arc<Mutex<RhaiEngine>>,
    axum_parts: Option<&Parts>,
) -> Result<CallToolResult, McpError> {
    let (endpoint, headers) =
        checkpoints::on_execute_graphql_operation(rhai_engine, endpoint, headers, axum_parts)?;

    let graphql_request = graphql::Request {
        input: Value::from(filter_inputs_for_operation(arguments, &tool.operation)),
        endpoint: &endpoint,
        headers: &headers,
    };

    let result = tool
        .operation
        .execute(graphql_request)
        .with_context(Context::current())
        .await?;

    let mut prefetch_calls = Vec::new();
    for prefetch in &app.prefetch_operations {
        if Arc::ptr_eq(&prefetch.operation, &tool.operation) {
            // Don't re-run any prefetches that already ran as the base tool call
            continue;
        }

        let graphql_request = graphql::Request {
            input: Value::from(filter_inputs_for_operation(arguments, &prefetch.operation)),
            endpoint: &endpoint,
            headers: &headers,
        };
        prefetch_calls.push(
            prefetch
                .operation
                .execute(graphql_request)
                .with_context(Context::current())
                .map_ok(|res| (prefetch.prefetch_id.clone(), res)),
        );
    }

    let prefetch_results = try_join_all(prefetch_calls.into_iter()).await?;
    Ok(nest_app_tool_result(
        result,
        &tool.tool.name,
        prefetch_results,
    ))
}

/// Extract the full (unfiltered) result for use in the `meta.structuredContent` wrapper.
///
/// If the operation had `@private` fields, the full result was stashed in
/// `meta.structuredContent` by `execute()` — remove and return it.
/// Otherwise, fall back to cloning `structured_content` (which *is* the full result
/// when no fields were filtered).
fn take_full_result(result: &mut CallToolResult) -> Option<Value> {
    result
        .meta
        .as_mut()
        .and_then(|meta| meta.remove("structuredContent"))
        .or_else(|| result.structured_content.clone())
}

/// Wraps the primary tool result and any prefetch results into a single nested structure.
///
/// Prefetch results are keyed by a manifest-defined `prefetchID` so the UI can
/// distinguish between different prefetches.
///
/// When operations contain `@private` fields, the result is split into two tracks:
/// - `structured_content` holds the **restricted** result (private fields removed),
///   which is what the AI model sees.
/// - `meta.structuredContent` preserves the **full** result (including private fields),
///   accessible to the host client but hidden from the model.
fn nest_app_tool_result(
    mut result: CallToolResult,
    tool_name: &str,
    mut prefetch_results: Vec<(String, CallToolResult)>,
) -> CallToolResult {
    let Some(restricted_content) = result.structured_content.take() else {
        return result;
    };

    // Build the full (unfiltered) result if the primary has @private fields.
    // When @private fields were filtered, the full result (from meta) will differ
    // from the restricted content, so we use that difference to detect the split.
    let full_result = take_full_result(&mut result);
    let primary_has_private = full_result
        .as_ref()
        .is_some_and(|full| full != &restricted_content);

    // Build restricted wrapped object (always)
    let mut restricted_map = Map::new();
    restricted_map.insert("result".into(), restricted_content.clone());

    // Lazily initialized when any result (primary or prefetch) has @private fields.
    // When only a prefetch has @private, we still need the full map so the host
    // client can access unfiltered prefetch data.
    let mut full_map: Option<Map<String, Value>> = if primary_has_private {
        let mut m = Map::new();
        // full_result is always Some when primary_has_private is true
        m.insert("result".into(), full_result.unwrap_or_default());
        Some(m)
    } else {
        None
    };

    // Prefetch results
    let mut restricted_prefetch = Map::new();
    let mut full_prefetch = Map::new();
    for (prefetch_id, prefetch_result) in &mut prefetch_results {
        let prefetch_full = take_full_result(prefetch_result);
        if let Some(ref restricted) = prefetch_result.structured_content {
            restricted_prefetch.insert(prefetch_id.clone(), restricted.clone());
        }
        if let Some(full) = prefetch_full {
            let prefetch_has_private = prefetch_result
                .structured_content
                .as_ref()
                .is_some_and(|r| *r != full);
            if prefetch_has_private {
                // Lazily initialize full_map if this is the first result with @private.
                // Only prefetch data is included; the primary result is omitted since
                // it has no private fields and doesn't need a full/restricted split.
                full_map.get_or_insert_with(Map::new);
                full_prefetch.insert(prefetch_id.clone(), full);
            }
        }
    }
    if !restricted_prefetch.is_empty() {
        restricted_map.insert("prefetch".into(), Value::Object(restricted_prefetch));
    }
    if let Some(ref mut full_m) = full_map
        && !full_prefetch.is_empty()
    {
        full_m.insert("prefetch".into(), Value::Object(full_prefetch));
    }

    // This is a temporary workaround because some MCP hosts don't properly expose _meta so we need the tool name to be available here as a backup
    restricted_map.insert("toolName".into(), Value::String(tool_name.to_string()));
    if let Some(ref mut full_m) = full_map {
        full_m.insert("toolName".into(), Value::String(tool_name.to_string()));
    }

    let wrapped_restricted = Value::Object(restricted_map);
    result.content = vec![
        Content::json(&wrapped_restricted).unwrap_or(Content::text(wrapped_restricted.to_string())),
    ];
    result.structured_content = Some(wrapped_restricted);

    // Attach tool name (and full structured content if private fields exist) to meta
    let meta = result.meta.get_or_insert_with(Meta::new);
    meta.insert("toolName".into(), Value::String(tool_name.to_string()));
    if let Some(full_m) = full_map {
        meta.insert("structuredContent".into(), Value::Object(full_m));
    }

    result
}

fn filter_inputs_for_operation(
    inputs: Option<&JsonObject>,
    operation: &Operation,
) -> Option<JsonObject> {
    let inputs = inputs?;
    let operation_properties = operation.tool.input_schema.get("properties")?.as_object()?;

    Some(
        inputs
            .iter()
            .filter(|(key, _)| operation_properties.contains_key(*key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

/// This makes the tool executable from the app but hidden from the LLM
pub(crate) fn make_tool_private(mut tool: Tool) -> Tool {
    let meta = tool.meta.get_or_insert_with(Meta::new);

    let mut ui = Meta::new();
    ui.insert("visibility".into(), json!(["app"]));
    meta.insert("ui".into(), serde_json::to_value(ui).unwrap_or_default());

    tool
}

// Attach tool meta data when requested to allow swapping between app targets (Apps SDK, MCP Apps)
pub(crate) fn attach_tool_metadata(app: &App, tool: &AppTool, app_target: &AppTarget) -> Tool {
    let mut inner_tool = tool.tool.clone();
    let meta = inner_tool.meta.get_or_insert_with(Meta::new);
    let mut ui = Meta::new();

    ui.insert("resourceUri".to_string(), app.uri.to_string().into());
    ui.insert("visibility".to_string(), json!(["model", "app"]));
    // Deprecated in favor of ui.resourceUri... keeping it here for clients who haven't yet moved to the new property
    meta.insert("ui/resourceUri".to_string(), app.uri.to_string().into());

    if matches!(app_target, AppTarget::AppsSDK)
        && let Some(tool_invocation_invoking) = &tool.labels.tool_invocation_invoking
    {
        meta.insert(
            "openai/toolInvocation/invoking".into(),
            tool_invocation_invoking.clone().into(),
        );
    }

    if matches!(app_target, AppTarget::AppsSDK)
        && let Some(tool_invocation_invoked) = &tool.labels.tool_invocation_invoked
    {
        meta.insert(
            "openai/toolInvocation/invoked".into(),
            tool_invocation_invoked.clone().into(),
        );
    }

    meta.insert("ui".into(), serde_json::to_value(ui).unwrap_or_default());

    inner_tool
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use rmcp::{model::Tool, object};

    use crate::apps::app::{AppResource, AppResourceSource, PrefetchOperation};
    use crate::apps::manifest::AppLabels;
    use crate::operations::{MutationMode, RawOperation};

    use super::*;

    #[tokio::test]
    async fn multiple_requests_for_a_tool() {
        // Build a GraphQL schema and 3 operations that will be sent
        let schema = Schema::parse(
            "type Query { apples(first: Int): String, bananas(first: Int): String, oranges(first: Int): String }",
            "schema.graphql",
        )
        .unwrap()
        .validate()
        .unwrap();
        let primary_operation = Arc::new(
            RawOperation::from((
                "query Primary($apples: Int) { apples(first: $apples) }".to_string(),
                None,
            ))
            .into_operation(&schema, None, MutationMode::All, true, true, true)
            .unwrap()
            .unwrap(),
        );
        let first_prefetch_id = "first_prefetch_id";
        let second_prefetch_id = "second_prefetch_id";
        let first_prefetch_operation = Arc::new(
            RawOperation::from((
                "query FirstPrefetch($bananas: Int) { bananas(first: $bananas) }".to_string(),
                None,
            ))
            .into_operation(&schema, None, MutationMode::All, true, true, true)
            .unwrap()
            .unwrap(),
        );
        let second_prefetch_operation = Arc::new(
            RawOperation::from((
                "query SecondPrefetch($oranges: Int) { oranges(first: $oranges) }".to_string(),
                None,
            ))
            .into_operation(&schema, None, MutationMode::All, true, true, true)
            .unwrap()
            .unwrap(),
        );

        // Set up a fake GraphQL server to receive the requests and respond with the right data
        let mut server = mockito::Server::new_async().await;
        let primary_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex(
                r#".*"operationName"\s*:\s*"Primary".*"#.to_string(),
            ))
            .with_body(r#"{"data": {"apples": "AppleData"}}"#)
            .with_header("Content-Type", "application/json")
            .expect(1)
            .create_async()
            .await;
        let first_prefetch_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex(
                r#".*"operationName"\s*:\s*"FirstPrefetch".*"#.to_string(),
            ))
            .with_body(r#"{"data": {"bananas": "BananaData"}}"#)
            .with_header("Content-Type", "application/json")
            .expect(1)
            .create_async()
            .await;
        let second_prefetch_mock = server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex(
                r#".*"operationName"\s*:\s*"SecondPrefetch".*"#.to_string(),
            ))
            .with_body(r#"{"data": {"oranges": "OrangeData"}}"#)
            .with_header("Content-Type", "application/json")
            .expect(1)
            .create_async()
            .await;

        let app = App {
            name: "MyApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("blah".to_string())),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://MyApp".parse().unwrap(),
            tools: vec![AppTool {
                operation: primary_operation.clone(),
                tool: Tool::new("ATool", "", JsonObject::new()),
                labels: AppLabels::default(),
            }],
            prefetch_operations: vec![
                PrefetchOperation {
                    operation: first_prefetch_operation.clone(),
                    prefetch_id: first_prefetch_id.to_string(),
                },
                PrefetchOperation {
                    operation: second_prefetch_operation.clone(),
                    prefetch_id: second_prefetch_id.to_string(),
                },
            ],
        };

        let response = execute_app_tool(
            &app,
            &app.tools[0],
            &HeaderMap::new(),
            Some(&object!({"apples": 1, "oranges": 2, "bananas": 3})),
            &server.url().parse().unwrap(),
            &Arc::new(Mutex::new(RhaiEngine::new())),
            None,
        )
        .await
        .unwrap();

        // Check that the correct requests were sent. This will make sure that:
        // - Each GraphQL operation was only sent once
        // - Each operation was sent with the correct variables
        primary_mock.assert();
        first_prefetch_mock.assert();
        second_prefetch_mock.assert();

        // Now we'll verify that the response formatting is correct
        let mut data = response.structured_content.unwrap();
        let data = data.as_object_mut().unwrap();
        let mut primary_data = data.remove("result").expect("Primary data is missing");
        let primary_data = primary_data.as_object_mut().unwrap();
        let apples_data = primary_data.get("data").unwrap().as_object().unwrap();
        assert_eq!(
            apples_data.get("apples").unwrap().as_str().unwrap(),
            "AppleData"
        );

        let mut secondary_data = data.remove("prefetch").expect("Secondary data is missing");
        let secondary_data = secondary_data.as_object_mut().unwrap();

        let mut first_prefetch = secondary_data
            .remove(first_prefetch_id)
            .expect("First prefetch data is missing");
        let first_prefetch = first_prefetch.as_object_mut().unwrap();
        let bananas_data = first_prefetch.get("data").unwrap().as_object().unwrap();
        assert_eq!(
            bananas_data.get("bananas").unwrap().as_str().unwrap(),
            "BananaData"
        );

        let mut second_prefetch = secondary_data
            .remove(second_prefetch_id)
            .expect("Second prefetch data is missing");
        let second_prefetch = second_prefetch.as_object_mut().unwrap();
        let oranges_data = second_prefetch.get("data").unwrap().as_object().unwrap();
        assert_eq!(
            oranges_data.get("oranges").unwrap().as_str().unwrap(),
            "OrangeData"
        );

        assert!(
            secondary_data.is_empty(),
            "Primary result should not be duplicated in secondary data"
        );
    }

    #[tokio::test]
    async fn find_and_execute_app_tool_with_valid_app_name() {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let app = App {
            name: "MyApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test".to_string())),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://MyApp".parse().unwrap(),
            tools: vec![AppTool {
                operation: Arc::new(
                    RawOperation::from(("query GetId { id }".to_string(), None))
                        .into_operation(&schema, None, MutationMode::All, false, false, true)
                        .unwrap()
                        .unwrap(),
                ),
                labels: AppLabels::default(),
                tool: Tool::new("GetId", "a description", JsonObject::new()),
            }],
            prefetch_operations: vec![],
        };

        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/")
            .with_body(r#"{"data": {"id": "123"}}"#)
            .with_header("Content-Type", "application/json")
            .expect(1)
            .create_async()
            .await;

        let result = find_and_execute_app_tool(
            &[app],
            "MyApp",
            "GetId",
            &HeaderMap::new(),
            None,
            &server.url().parse().unwrap(),
            &Arc::new(Mutex::new(RhaiEngine::new())),
            None,
        )
        .await;

        mock.assert();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn find_and_execute_app_tool_with_invalid_app_name() {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let app = App {
            name: "MyApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test".to_string())),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://MyApp".parse().unwrap(),
            tools: vec![AppTool {
                operation: Arc::new(
                    RawOperation::from(("query GetId { id }".to_string(), None))
                        .into_operation(&schema, None, MutationMode::All, false, false, true)
                        .unwrap()
                        .unwrap(),
                ),
                labels: AppLabels::default(),
                tool: Tool::new("GetId", "a description", JsonObject::new()),
            }],
            prefetch_operations: vec![],
        };

        let server = mockito::Server::new_async().await;

        let result = find_and_execute_app_tool(
            &[app],
            "InvalidApp",
            "GetId",
            &HeaderMap::new(),
            None,
            &server.url().parse().unwrap(),
            &Arc::new(Mutex::new(RhaiEngine::new())),
            None,
        )
        .await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn find_and_execute_app_tool_with_valid_app_but_invalid_tool() {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let app = App {
            name: "MyApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test".to_string())),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://MyApp".parse().unwrap(),
            tools: vec![AppTool {
                operation: Arc::new(
                    RawOperation::from(("query GetId { id }".to_string(), None))
                        .into_operation(&schema, None, MutationMode::All, false, false, true)
                        .unwrap()
                        .unwrap(),
                ),
                labels: AppLabels::default(),
                tool: Tool::new("GetId", "a description", JsonObject::new()),
            }],
            prefetch_operations: vec![],
        };

        let server = mockito::Server::new_async().await;

        let result = find_and_execute_app_tool(
            &[app],
            "MyApp",
            "InvalidTool",
            &HeaderMap::new(),
            None,
            &server.url().parse().unwrap(),
            &Arc::new(Mutex::new(RhaiEngine::new())),
            None,
        )
        .await;

        assert!(result.is_none());
    }

    #[test]
    fn make_tool_private_adds_ui_meta_for_mcp_apps_when_tool_has_no_meta() {
        let mut tool = Tool::new("GetId", "a description", JsonObject::new());
        tool = make_tool_private(tool);

        let meta = tool.meta.unwrap();

        assert_eq!(meta.keys().len(), 1);
        assert_eq!(meta.get("ui").unwrap(), &json!({"visibility": ["app"]}));
    }

    #[test]
    fn make_tool_private_adds_ui_meta_for_mcp_apps_when_tool_has_existing_meta() {
        let mut existing_meta = Meta::new();
        existing_meta.insert("my-awesome-key".into(), "my-awesome-value".into());
        let mut tool = Tool::new("GetId", "a description", JsonObject::new());
        tool.meta = Some(existing_meta);
        tool = make_tool_private(tool);

        let meta = tool.meta.unwrap();

        assert_eq!(meta.keys().len(), 2);
        assert_eq!(
            meta.get("my-awesome-key").unwrap(),
            &Value::from("my-awesome-value")
        );
        assert_eq!(meta.get("ui").unwrap(), &json!({"visibility": ["app"]}));
    }

    fn create_test_operation() -> Arc<crate::operations::Operation> {
        let schema = Schema::parse("type Query { hello: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();
        Arc::new(
            RawOperation::from(("query TestOp { hello }".to_string(), None))
                .into_operation(&schema, None, MutationMode::All, true, true, true)
                .unwrap()
                .unwrap(),
        )
    }

    #[test]
    fn attach_tool_metadata_adds_resource_uri_and_visibility() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test".to_string())),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let tool = AppTool {
            operation: create_test_operation(),
            labels: AppLabels::default(),
            tool: Tool::new("TestTool", "description", JsonObject::new()),
        };

        let result = attach_tool_metadata(&app, &tool, &AppTarget::AppsSDK);

        let meta = result.meta.unwrap();
        let ui = meta.get("ui").unwrap().as_object().unwrap();
        assert_eq!(
            ui.get("resourceUri").unwrap(),
            "ui://widget/TestApp#hash123"
        );
        assert_eq!(ui.get("visibility").unwrap(), &json!(["model", "app"]));
        assert_eq!(
            meta.get("ui/resourceUri").unwrap(),
            "ui://widget/TestApp#hash123"
        );
    }

    #[test]
    fn attach_tool_metadata_adds_invocation_labels_when_present() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test".to_string())),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let tool = AppTool {
            operation: create_test_operation(),
            labels: AppLabels {
                tool_invocation_invoking: Some("Loading...".to_string()),
                tool_invocation_invoked: Some("Done!".to_string()),
            },
            tool: Tool::new("TestTool", "description", JsonObject::new()),
        };

        let result = attach_tool_metadata(&app, &tool, &AppTarget::AppsSDK);

        let meta = result.meta.unwrap();
        assert_eq!(
            meta.get("openai/toolInvocation/invoking").unwrap(),
            "Loading..."
        );
        assert_eq!(meta.get("openai/toolInvocation/invoked").unwrap(), "Done!");
    }

    #[test]
    fn attach_tool_metadata_does_not_add_invocation_labels_when_none() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test".to_string())),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let tool = AppTool {
            operation: create_test_operation(),
            labels: AppLabels::default(),
            tool: Tool::new("TestTool", "description", JsonObject::new()),
        };

        let result = attach_tool_metadata(&app, &tool, &AppTarget::AppsSDK);

        let meta = result.meta.unwrap();
        assert!(meta.get("openai/toolInvocation/invoking").is_none());
        assert!(meta.get("openai/toolInvocation/invoked").is_none());
        // These should still be present
        assert!(meta.get("ui/resourceUri").is_some());
        assert!(meta.get("ui").is_some());
    }

    #[test]
    fn attach_tool_metadata_preserves_existing_meta() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test".to_string())),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let mut existing_meta = Meta::new();
        existing_meta.insert("custom-key".into(), "custom-value".into());

        let mut tool_def = Tool::new("TestTool", "description", JsonObject::new());
        tool_def.meta = Some(existing_meta);

        let tool = AppTool {
            operation: create_test_operation(),
            labels: AppLabels::default(),
            tool: tool_def,
        };

        let result = attach_tool_metadata(&app, &tool, &AppTarget::AppsSDK);

        let meta = result.meta.unwrap();
        assert_eq!(meta.get("custom-key").unwrap(), "custom-value");
        assert!(meta.get("ui/resourceUri").is_some());
    }

    #[test]
    fn attach_tool_metadata_mcp_apps_adds_resource_uri_and_visibility() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test".to_string())),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let tool = AppTool {
            operation: create_test_operation(),
            labels: AppLabels::default(),
            tool: Tool::new("TestTool", "description", JsonObject::new()),
        };

        let result = attach_tool_metadata(&app, &tool, &AppTarget::MCPApps);

        let meta = result.meta.unwrap();

        // Check deprecated root-level ui/resourceUri
        assert_eq!(
            meta.get("ui/resourceUri").unwrap(),
            "ui://widget/TestApp#hash123"
        );

        // Check nested ui metadata
        let ui = meta.get("ui").unwrap().as_object().unwrap();
        assert_eq!(
            ui.get("resourceUri").unwrap(),
            "ui://widget/TestApp#hash123"
        );
        assert_eq!(ui.get("visibility").unwrap(), &json!(["model", "app"]));
    }

    #[test]
    fn attach_tool_metadata_mcp_apps_does_not_add_invocation_labels() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test".to_string())),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let tool = AppTool {
            operation: create_test_operation(),
            labels: AppLabels {
                tool_invocation_invoking: Some("Loading...".to_string()),
                tool_invocation_invoked: Some("Done!".to_string()),
            },
            tool: Tool::new("TestTool", "description", JsonObject::new()),
        };

        let result = attach_tool_metadata(&app, &tool, &AppTarget::MCPApps);

        let meta = result.meta.unwrap();
        assert!(meta.get("openai/toolInvocation/invoking").is_none());
        assert!(meta.get("openai/toolInvocation/invoked").is_none());
    }

    #[test]
    fn attach_tool_metadata_mcp_apps_preserves_existing_meta() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test".to_string())),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let mut existing_meta = Meta::new();
        existing_meta.insert("custom-key".into(), "custom-value".into());

        let mut tool_def = Tool::new("TestTool", "description", JsonObject::new());
        tool_def.meta = Some(existing_meta);

        let tool = AppTool {
            operation: create_test_operation(),
            labels: AppLabels::default(),
            tool: tool_def,
        };

        let result = attach_tool_metadata(&app, &tool, &AppTarget::MCPApps);

        let meta = result.meta.unwrap();
        assert_eq!(meta.get("custom-key").unwrap(), "custom-value");
        assert!(meta.get("ui/resourceUri").is_some());
        assert!(meta.get("ui").is_some());
    }

    #[test]
    fn attach_tool_metadata_mcp_apps_does_not_add_openai_keys() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Single(AppResourceSource::Local("test".to_string())),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let tool = AppTool {
            operation: create_test_operation(),
            labels: AppLabels::default(),
            tool: Tool::new("TestTool", "description", JsonObject::new()),
        };

        let result = attach_tool_metadata(&app, &tool, &AppTarget::MCPApps);

        let meta = result.meta.unwrap();
        assert!(meta.get("openai/outputTemplate").is_none());
        assert!(meta.get("openai/widgetAccessible").is_none());
    }

    #[test]
    fn nest_app_tool_result_no_private_no_prefetch() {
        let primary_data = json!({"data": {"fieldA": "a"}});

        let result = CallToolResult {
            content: vec![],
            is_error: None,
            meta: None,
            structured_content: Some(primary_data.clone()),
        };

        let nested = nest_app_tool_result(result, "MyTool", vec![]);

        // structured_content: primary result present, no prefetch
        let sc = nested.structured_content.unwrap();
        assert_eq!(sc.get("result").unwrap(), &primary_data);
        assert!(sc.get("prefetch").is_none());

        // meta: no structuredContent at all
        let meta = nested.meta.unwrap();
        assert!(meta.get("structuredContent").is_none());
        assert!(meta.get("toolName").is_some());
    }

    #[test]
    fn nest_app_tool_result_primary_private_no_prefetch() {
        let restricted = json!({"data": {"fieldA": "a"}});
        let full = json!({"data": {"fieldA": "a", "fieldB": "secret"}});

        let mut meta = Meta::new();
        meta.insert("structuredContent".into(), full.clone());

        let result = CallToolResult {
            content: vec![],
            is_error: None,
            meta: Some(meta),
            structured_content: Some(restricted.clone()),
        };

        let nested = nest_app_tool_result(result, "MyTool", vec![]);

        // structured_content: restricted primary, no prefetch
        let sc = nested.structured_content.unwrap();
        assert_eq!(sc.get("result").unwrap(), &restricted);
        assert!(sc.get("prefetch").is_none());

        // meta.structuredContent: full primary, no prefetch
        let meta = nested.meta.unwrap();
        let meta_sc = meta.get("structuredContent").unwrap();
        assert_eq!(meta_sc.get("result").unwrap(), &full);
        assert!(meta_sc.get("prefetch").is_none());
    }

    #[test]
    fn nest_app_tool_result_both_primary_and_prefetch_private() {
        let restricted_primary = json!({"data": {"fieldA": "a"}});
        let full_primary = json!({"data": {"fieldA": "a", "fieldB": "secret"}});

        let mut primary_meta = Meta::new();
        primary_meta.insert("structuredContent".into(), full_primary.clone());

        let result = CallToolResult {
            content: vec![],
            is_error: None,
            meta: Some(primary_meta),
            structured_content: Some(restricted_primary.clone()),
        };

        let restricted_prefetch = json!({"data": {"x": 1}});
        let full_prefetch = json!({"data": {"x": 1, "y": 2}});

        let mut prefetch_meta = Meta::new();
        prefetch_meta.insert("structuredContent".into(), full_prefetch.clone());

        let prefetch_result = CallToolResult {
            content: vec![],
            is_error: None,
            meta: Some(prefetch_meta),
            structured_content: Some(restricted_prefetch.clone()),
        };

        let nested = nest_app_tool_result(result, "MyTool", vec![("pf1".into(), prefetch_result)]);

        // structured_content: restricted primary and restricted prefetch
        let sc = nested.structured_content.unwrap();
        assert_eq!(sc.get("result").unwrap(), &restricted_primary);
        assert_eq!(
            sc.get("prefetch").unwrap().get("pf1").unwrap(),
            &restricted_prefetch
        );

        // meta.structuredContent: full primary and full prefetch
        let meta = nested.meta.unwrap();
        let meta_sc = meta.get("structuredContent").unwrap();
        assert_eq!(meta_sc.get("result").unwrap(), &full_primary);
        assert_eq!(
            meta_sc.get("prefetch").unwrap().get("pf1").unwrap(),
            &full_prefetch
        );
    }

    #[test]
    fn nest_app_tool_result_only_prefetch_private() {
        // Primary has no @private fields
        let primary_data = json!({"data": {"fieldA": "a"}});

        let result = CallToolResult {
            content: vec![],
            is_error: None,
            meta: None,
            structured_content: Some(primary_data.clone()),
        };

        // Prefetch has @private fields
        let restricted_prefetch = json!({"data": {"x": 1}});
        let full_prefetch = json!({"data": {"x": 1, "y": 2}});

        let mut prefetch_meta = Meta::new();
        prefetch_meta.insert("structuredContent".into(), full_prefetch.clone());

        let prefetch_result = CallToolResult {
            content: vec![],
            is_error: None,
            meta: Some(prefetch_meta),
            structured_content: Some(restricted_prefetch.clone()),
        };

        let nested = nest_app_tool_result(result, "MyTool", vec![("pf1".into(), prefetch_result)]);

        // structured_content: primary result and restricted prefetch
        let sc = nested.structured_content.unwrap();
        assert_eq!(sc.get("result").unwrap(), &primary_data);
        assert_eq!(
            sc.get("prefetch").unwrap().get("pf1").unwrap(),
            &restricted_prefetch
        );

        // meta.structuredContent: no primary (no private), full prefetch only
        let meta = nested.meta.unwrap();
        let meta_sc = meta.get("structuredContent").unwrap();
        assert!(meta_sc.get("result").is_none());
        assert_eq!(
            meta_sc.get("prefetch").unwrap().get("pf1").unwrap(),
            &full_prefetch
        );
    }

    #[test]
    fn nest_app_tool_result_only_primary_private() {
        // Primary has @private fields
        let restricted_primary = json!({"data": {"fieldA": "a"}});
        let full_primary = json!({"data": {"fieldA": "a", "fieldB": "secret"}});

        let mut primary_meta = Meta::new();
        primary_meta.insert("structuredContent".into(), full_primary.clone());

        let result = CallToolResult {
            content: vec![],
            is_error: None,
            meta: Some(primary_meta),
            structured_content: Some(restricted_primary.clone()),
        };

        // Prefetch has no @private fields
        let prefetch_data = json!({"data": {"x": 1}});
        let prefetch_result = CallToolResult {
            content: vec![],
            is_error: None,
            meta: None,
            structured_content: Some(prefetch_data.clone()),
        };

        let nested = nest_app_tool_result(result, "MyTool", vec![("pf1".into(), prefetch_result)]);

        // structured_content: restricted primary and prefetch
        let sc = nested.structured_content.unwrap();
        assert_eq!(sc.get("result").unwrap(), &restricted_primary);
        assert_eq!(
            sc.get("prefetch").unwrap().get("pf1").unwrap(),
            &prefetch_data
        );

        // meta.structuredContent: full primary, no prefetch (prefetch has no private)
        let meta = nested.meta.unwrap();
        let meta_sc = meta.get("structuredContent").unwrap();
        assert_eq!(meta_sc.get("result").unwrap(), &full_primary);
        assert!(meta_sc.get("prefetch").is_none());
    }
}
