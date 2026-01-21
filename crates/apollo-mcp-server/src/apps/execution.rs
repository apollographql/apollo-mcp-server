use std::sync::Arc;

use futures::TryFutureExt;
use futures::future::try_join_all;
use http::HeaderMap;
use opentelemetry::Context;
use opentelemetry::trace::FutureExt;
use rmcp::ErrorData;
use rmcp::model::{
    CallToolResult, Content, ErrorCode, Extensions, JsonObject, Meta, Resource, ResourceContents,
    Tool,
};
use serde_json::{Map, Value, json};
use url::Url;

use crate::apps::AppResource;
use crate::errors::McpError;
use crate::graphql::{self, Executable};
use crate::operations::Operation;

use super::{App, AppTool};

pub(crate) async fn find_and_execute_app(
    apps: &[App],
    app_name: &str,
    tool_name: &str,
    headers: &HeaderMap,
    arguments: Option<&JsonObject>,
    endpoint: &Url,
) -> Option<Result<CallToolResult, McpError>> {
    let app = apps.iter().find(|app| app.name == app_name)?;

    for tool in &app.tools {
        if tool.tool.name == tool_name {
            return Some(execute_app(app, tool, headers, arguments, endpoint).await);
        }
    }
    None
}

async fn execute_app(
    app: &App,
    tool: &AppTool,
    headers: &HeaderMap,
    arguments: Option<&JsonObject>,
    endpoint: &Url,
) -> Result<CallToolResult, McpError> {
    let graphql_request = graphql::Request {
        input: Value::from(filter_inputs_for_operation(arguments, &tool.operation)),
        endpoint,
        headers,
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
            endpoint,
            headers,
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

/// For prefetch App data, there will potentially be 0 or multiple results.
/// We key any results based on a manifest-defined `prefetchID` so the UI can distinguish between different prefetches.
fn nest_app_tool_result(
    mut result: CallToolResult,
    tool_name: &str,
    prefetch_results: Vec<(String, CallToolResult)>,
) -> CallToolResult {
    if let Some(structured_content) = result.structured_content.take() {
        let mut map = Map::new();

        // Main tool result
        map.insert("result".into(), structured_content);

        // Prefetch results
        let mut prefetch = Map::new();
        for (prefetch_id, result) in prefetch_results.into_iter() {
            if let Some(structured_content) = result.structured_content {
                prefetch.insert(prefetch_id, structured_content);
            }
        }
        if !prefetch.is_empty() {
            map.insert("prefetch".into(), Value::Object(prefetch));
        }

        let wrapped = Value::Object(map);
        result.content =
            vec![Content::json(&wrapped).unwrap_or(Content::text(wrapped.to_string()))];
        result.structured_content = Some(wrapped);

        // Attach tool name to the result meta
        result.meta = Some({
            let mut meta = Meta::new();
            meta.insert("toolName".into(), Value::String(tool_name.to_string()));
            meta
        });
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
    meta.insert("openai/visibility".into(), "private".into());
    tool
}

// Attach tool meta data when requested to allow swapping between app targets (Apps SDK, MCP Apps)
pub(crate) fn attach_tool_metadata(app: &App, tool: &AppTool) -> Tool {
    let mut inner_tool = tool.tool.clone();
    let meta = inner_tool.meta.get_or_insert_with(Meta::new);
    meta.insert(
        "openai/outputTemplate".to_string(),
        app.uri.to_string().into(),
    );
    meta.insert("openai/widgetAccessible".to_string(), true.into());

    if let Some(tool_invocation_invoking) = &tool.labels.tool_invocation_invoking {
        meta.insert(
            "openai/toolInvocation/invoking".into(),
            tool_invocation_invoking.clone().into(),
        );
    }

    if let Some(tool_invocation_invoked) = &tool.labels.tool_invocation_invoked {
        meta.insert(
            "openai/toolInvocation/invoked".into(),
            tool_invocation_invoked.clone().into(),
        );
    }

    inner_tool
}

pub(crate) fn get_mime_type(app_target: &AppTarget) -> String {
    match app_target {
        AppTarget::AppsSDK => "text/html+skybridge".to_string(),
        AppTarget::MCPApps => "text/html;profile=mcp-app".to_string(),
    }
}

// Attach resource mime type when requested to allow swapping between app targets (Apps SDK, MCP Apps)
pub(crate) fn attach_resource_mime_type(
    mut resource: Resource,
    app_target: &AppTarget,
) -> Resource {
    resource.raw.mime_type = Some(get_mime_type(app_target));
    resource
}

pub(crate) async fn get_app_resource(
    apps: &[App],
    request: rmcp::model::ReadResourceRequestParam,
    request_uri: Url,
    app_target: &AppTarget,
) -> Result<ResourceContents, ErrorData> {
    let Some(app) = apps.iter().find(|app| app.uri.path() == request_uri.path()) else {
        return Err(ErrorData::resource_not_found(
            format!("Resource not found for URI: {}", request.uri),
            None,
        ));
    };

    let text = match &app.resource {
        AppResource::Local(contents) => contents.clone(),
        AppResource::Remote(url) => {
            let response = reqwest::Client::new()
                .get(url.clone())
                .send()
                .await
                .map_err(|err| {
                    ErrorData::resource_not_found(
                        format!("Failed to fetch resource from {}: {err}", url),
                        None,
                    )
                })?;

            if !response.status().is_success() {
                return Err(ErrorData::resource_not_found(
                    format!(
                        "Failed to fetch resource from {}: received status {}",
                        url,
                        response.status()
                    ),
                    None,
                ));
            }

            response.text().await.map_err(|err| {
                ErrorData::resource_not_found(
                    format!("Failed to read resource body from {}: {err}", url),
                    None,
                )
            })?
        }
    };

    let mut meta: Option<Meta> = None;
    if let Some(csp) = &app.csp_settings {
        match app_target {
            // Note that the difference in which keys are set here and the camelCase vs snake_key is on purpose. These are differences between the two specs.
            AppTarget::AppsSDK => {
                meta.get_or_insert_with(Meta::new).insert(
                    "openai/widgetCSP".into(),
                    json!({
                        "connect_domains": csp.connect_domains,
                        "resource_domains": csp.resource_domains,
                        "frame_domains": csp.frame_domains,
                        "redirect_domains": csp.redirect_domains
                    }),
                );
            }
            AppTarget::MCPApps => {
                meta.get_or_insert_with(Meta::new).insert(
                    "csp".into(),
                    json!({
                        "connectDomains": csp.connect_domains,
                        "resourceDomains": csp.resource_domains,
                        "frameDomains": csp.frame_domains,
                        "baseUriDomains": csp.base_uri_domains
                    }),
                );
            }
        }
    }
    if let Some(widget_settings) = &app.widget_settings {
        if let Some(description) = &widget_settings.description
            && matches!(app_target, AppTarget::AppsSDK)
        {
            meta.get_or_insert_with(Meta::new).insert(
                "openai/widgetDescription".into(),
                serde_json::to_value(description).unwrap_or_default(),
            );
        }

        if let Some(domain) = &widget_settings.domain {
            meta.get_or_insert_with(Meta::new).insert(
                match app_target {
                    AppTarget::AppsSDK => "openai/widgetDomain".into(),
                    AppTarget::MCPApps => "domain".into(),
                },
                serde_json::to_value(domain).unwrap_or_default(),
            );
        }

        if let Some(prefers_border) = &widget_settings.prefers_border {
            meta.get_or_insert_with(Meta::new).insert(
                match app_target {
                    AppTarget::AppsSDK => "openai/widgetPrefersBorder".into(),
                    AppTarget::MCPApps => "prefersBorder".into(),
                },
                serde_json::to_value(prefers_border).unwrap_or_default(),
            );
        }
    }

    // In the case of MCP Apps, the meta data is nested under `_meta.ui`
    if matches!(app_target, AppTarget::MCPApps) {
        let mut nested = Meta::new();
        nested.insert("ui".into(), serde_json::to_value(meta).unwrap_or_default());
        meta = Some(nested);
    }

    Ok(ResourceContents::TextResourceContents {
        uri: request.uri,
        mime_type: Some(get_mime_type(app_target)),
        text,
        meta,
    })
}

pub(crate) enum AppTarget {
    AppsSDK,
    MCPApps,
}

pub(crate) fn get_app_target(extensions: Extensions) -> Result<AppTarget, McpError> {
    let app_target_param = extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.uri.query())
        .and_then(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .find(|(key, _)| key == "appTarget")
                .map(|(_, value)| value.into_owned())
        });

    match app_target_param {
        Some(app_target) if app_target.to_lowercase() == "openai" => Ok(AppTarget::AppsSDK),
        Some(app_target) if app_target.to_lowercase() == "mcp" => Ok(AppTarget::MCPApps),
        Some(app_target) => Err(McpError::new(
            ErrorCode::INVALID_REQUEST,
            format!("App target {app_target} not recognized. Valid values are 'openai' or 'mcp'."),
            None,
        )),
        // TODO: In the future, once host capabilities are advertised, we should try auto detection before defaulting to apps sdk
        None => Ok(AppTarget::AppsSDK),
    }
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use rmcp::{model::Tool, object};

    use crate::apps::{CSPSettings, WidgetSettings};
    use rmcp::model::RawResource;

    use crate::{
        apps::{AppLabels, AppResource, PrefetchOperation},
        operations::{MutationMode, RawOperation},
    };

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
            resource: AppResource::Local("blah".to_string()),
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

        let response = execute_app(
            &app,
            &app.tools[0],
            &HeaderMap::new(),
            Some(&object!({"apples": 1, "oranges": 2, "bananas": 3})),
            &server.url().parse().unwrap(),
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
    async fn find_and_execute_app_with_valid_app_name() {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let app = App {
            name: "MyApp".to_string(),
            description: None,
            resource: AppResource::Local("test".to_string()),
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

        let result = find_and_execute_app(
            &[app],
            "MyApp",
            "GetId",
            &HeaderMap::new(),
            None,
            &server.url().parse().unwrap(),
        )
        .await;

        mock.assert();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn find_and_execute_app_with_invalid_app_name() {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let app = App {
            name: "MyApp".to_string(),
            description: None,
            resource: AppResource::Local("test".to_string()),
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

        let result = find_and_execute_app(
            &[app],
            "InvalidApp",
            "GetId",
            &HeaderMap::new(),
            None,
            &server.url().parse().unwrap(),
        )
        .await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn find_and_execute_app_with_valid_app_but_invalid_tool() {
        let schema = Schema::parse("type Query { id: String }", "schema.graphql")
            .unwrap()
            .validate()
            .unwrap();

        let app = App {
            name: "MyApp".to_string(),
            description: None,
            resource: AppResource::Local("test".to_string()),
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

        let result = find_and_execute_app(
            &[app],
            "MyApp",
            "InvalidTool",
            &HeaderMap::new(),
            None,
            &server.url().parse().unwrap(),
        )
        .await;

        assert!(result.is_none());
    }

    #[test]
    fn make_tool_private_adds_meta_when_tool_has_no_meta() {
        let mut tool = Tool::new("GetId", "a description", JsonObject::new());
        tool = make_tool_private(tool);

        let meta = tool.meta.unwrap();

        assert_eq!(meta.keys().len(), 1);
        assert_eq!(
            meta.get("openai/visibility").unwrap(),
            &Value::from("private")
        );
    }

    #[test]
    fn make_tool_private_modified_meta_when_tool_has_existing_meta() {
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
        assert_eq!(
            meta.get("openai/visibility").unwrap(),
            &Value::from("private")
        );
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
    fn attach_tool_metadata_adds_output_template_and_widget_accessible() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Local("test".to_string()),
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

        let result = attach_tool_metadata(&app, &tool);

        let meta = result.meta.unwrap();
        assert_eq!(
            meta.get("openai/outputTemplate").unwrap(),
            "ui://widget/TestApp#hash123"
        );
        assert_eq!(meta.get("openai/widgetAccessible").unwrap(), true);
    }

    #[test]
    fn attach_tool_metadata_adds_invocation_labels_when_present() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Local("test".to_string()),
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

        let result = attach_tool_metadata(&app, &tool);

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
            resource: AppResource::Local("test".to_string()),
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

        let result = attach_tool_metadata(&app, &tool);

        let meta = result.meta.unwrap();
        assert!(meta.get("openai/toolInvocation/invoking").is_none());
        assert!(meta.get("openai/toolInvocation/invoked").is_none());
        // These should still be present
        assert!(meta.get("openai/outputTemplate").is_some());
        assert!(meta.get("openai/widgetAccessible").is_some());
    }

    #[test]
    fn attach_tool_metadata_preserves_existing_meta() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Local("test".to_string()),
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

        let result = attach_tool_metadata(&app, &tool);

        let meta = result.meta.unwrap();
        assert_eq!(meta.get("custom-key").unwrap(), "custom-value");
        assert!(meta.get("openai/outputTemplate").is_some());
    }

    #[test]
    fn attach_correct_mime_type_when_open_ai() {
        let resource = Resource::new(
            RawResource {
                name: "TestResource".to_string(),
                uri: "ui://test".to_string(),
                mime_type: None,
                title: None,
                description: None,
                icons: None,
                size: None,
                meta: None,
            },
            None,
        );

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?appTarget=openai")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);
        let app_target = get_app_target(extensions).unwrap();

        let result = attach_resource_mime_type(resource, &app_target);

        assert_eq!(
            result.raw.mime_type,
            Some("text/html+skybridge".to_string())
        );
    }

    #[test]
    fn attach_correct_mime_type_when_mcp_apps() {
        let resource = Resource::new(
            RawResource {
                name: "TestResource".to_string(),
                uri: "ui://test".to_string(),
                mime_type: None,
                title: None,
                description: None,
                icons: None,
                size: None,
                meta: None,
            },
            None,
        );

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?appTarget=mcp")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);
        let app_target = get_app_target(extensions).unwrap();

        let result = attach_resource_mime_type(resource, &app_target);

        assert_eq!(
            result.raw.mime_type,
            Some("text/html;profile=mcp-app".to_string())
        );
    }

    #[test]
    fn attach_correct_mime_type_when_not_provided() {
        let resource = Resource::new(
            RawResource {
                name: "TestResource".to_string(),
                uri: "ui://test".to_string(),
                mime_type: None,
                title: None,
                description: None,
                icons: None,
                size: None,
                meta: None,
            },
            None,
        );

        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);
        let app_target = get_app_target(extensions).unwrap();

        let result = attach_resource_mime_type(resource, &app_target);

        assert_eq!(
            result.raw.mime_type,
            Some("text/html+skybridge".to_string())
        );
    }

    #[test]
    fn errors_when_invalid_target_provided() {
        let mut extensions = Extensions::new();
        let request = axum::http::Request::builder()
            .uri("http://localhost?appTarget=lol")
            .body(())
            .unwrap();
        let (parts, _) = request.into_parts();
        extensions.insert(parts);
        let app_target = get_app_target(extensions);

        assert!(app_target.is_err());
        assert_eq!(
            app_target.err().unwrap().message,
            "App target lol not recognized. Valid values are 'openai' or 'mcp'."
        )
    }

    #[tokio::test]
    async fn get_app_resource_returns_openai_format_when_target_is_openai() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Local("test content".to_string()),
            csp_settings: Some(CSPSettings {
                connect_domains: Some(vec!["connect.example.com".to_string()]),
                resource_domains: Some(vec!["resource.example.com".to_string()]),
                frame_domains: Some(vec!["frame.example.com".to_string()]),
                redirect_domains: Some(vec!["redirect.example.com".to_string()]),
                base_uri_domains: Some(vec!["base.example.com".to_string()]),
            }),
            widget_settings: Some(WidgetSettings {
                description: Some("Test description".to_string()),
                domain: Some("example.com".to_string()),
                prefers_border: Some(true),
            }),
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let result = get_app_resource(
            &[app],
            rmcp::model::ReadResourceRequestParam {
                uri: "ui://widget/TestApp".to_string(),
            },
            "ui://widget/TestApp".parse().unwrap(),
            &AppTarget::AppsSDK,
        )
        .await
        .unwrap();

        let ResourceContents::TextResourceContents {
            mime_type, meta, ..
        } = result
        else {
            unreachable!()
        };
        assert_eq!(mime_type, Some("text/html+skybridge".to_string()));

        let meta = meta.unwrap();
        // AppsSDK CSP uses snake_case keys and includes redirect_domains (not base_uri_domains)
        let csp = meta.get("openai/widgetCSP").unwrap();
        assert!(csp.get("connect_domains").is_some());
        assert!(csp.get("resource_domains").is_some());
        assert!(csp.get("frame_domains").is_some());
        assert!(csp.get("redirect_domains").is_some());
        assert!(csp.get("base_uri_domains").is_none());
        assert!(meta.get("openai/widgetDescription").is_some());
        assert!(meta.get("openai/widgetDomain").is_some());
        assert!(meta.get("openai/widgetPrefersBorder").is_some());
        // AppsSDK should not have ui nesting
        assert!(meta.get("ui").is_none());
    }

    #[tokio::test]
    async fn get_app_resource_returns_mcp_format_when_target_is_mcp() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Local("test content".to_string()),
            csp_settings: Some(CSPSettings {
                connect_domains: Some(vec!["connect.example.com".to_string()]),
                resource_domains: Some(vec!["resource.example.com".to_string()]),
                frame_domains: Some(vec!["frame.example.com".to_string()]),
                redirect_domains: Some(vec!["redirect.example.com".to_string()]),
                base_uri_domains: Some(vec!["base.example.com".to_string()]),
            }),
            widget_settings: Some(WidgetSettings {
                description: Some("Test description".to_string()),
                domain: Some("example.com".to_string()),
                prefers_border: Some(true),
            }),
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let result = get_app_resource(
            &[app],
            rmcp::model::ReadResourceRequestParam {
                uri: "ui://widget/TestApp".to_string(),
            },
            "ui://widget/TestApp".parse().unwrap(),
            &AppTarget::MCPApps,
        )
        .await
        .unwrap();

        let ResourceContents::TextResourceContents {
            mime_type, meta, ..
        } = result
        else {
            unreachable!()
        };
        assert_eq!(mime_type, Some("text/html;profile=mcp-app".to_string()));

        let meta = meta.unwrap();
        // MCPApps should have ui nesting
        let ui_meta = meta.get("ui").unwrap();
        // MCPApps CSP uses camelCase keys and includes baseUriDomains (not redirectDomains)
        let csp = ui_meta.get("csp").unwrap();
        assert!(csp.get("connectDomains").is_some());
        assert!(csp.get("resourceDomains").is_some());
        assert!(csp.get("frameDomains").is_some());
        assert!(csp.get("baseUriDomains").is_some());
        assert!(csp.get("redirectDomains").is_none());
        assert!(ui_meta.get("domain").is_some());
        assert!(ui_meta.get("prefersBorder").is_some());
        // MCPApps should not have description
        assert!(ui_meta.get("description").is_none());
    }

    #[tokio::test]
    async fn get_app_resource_returns_error_for_nonexistent_resource() {
        let app = App {
            name: "TestApp".to_string(),
            description: None,
            resource: AppResource::Local("test content".to_string()),
            csp_settings: None,
            widget_settings: None,
            uri: "ui://widget/TestApp#hash123".parse().unwrap(),
            tools: vec![],
            prefetch_operations: vec![],
        };

        let result = get_app_resource(
            &[app],
            rmcp::model::ReadResourceRequestParam {
                uri: "ui://widget/NonExistent".to_string(),
            },
            "ui://widget/NonExistent".parse().unwrap(),
            &AppTarget::AppsSDK,
        )
        .await;

        assert!(result.is_err());
    }
}
