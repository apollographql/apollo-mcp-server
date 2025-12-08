use std::sync::Arc;

use futures::TryFutureExt;
use futures::future::try_join_all;
use http::HeaderMap;
use opentelemetry::Context;
use opentelemetry::trace::FutureExt;
use rmcp::model::{CallToolResult, Content, JsonObject, Meta};
use serde_json::{Map, Value};
use url::Url;

use crate::errors::McpError;
use crate::graphql::{self, Executable};
use crate::operations::Operation;

use super::{App, AppTool};

pub(crate) async fn find_and_execute_app(
    apps: &[App],
    tool_name: &str,
    headers: &HeaderMap,
    arguments: Option<&JsonObject>,
    endpoint: &Url,
) -> Option<Result<CallToolResult, McpError>> {
    for app in apps {
        for tool in &app.tools {
            if tool.tool.name == tool_name {
                return Some(execute_app(app, tool, headers, arguments, endpoint).await);
            }
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

    let r = Some(
        inputs
            .iter()
            .filter(|(key, _)| operation_properties.contains_key(*key))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );

    println!("result of filter: {:?}", r);

    return r;
}

#[cfg(test)]
mod tests {
    use apollo_compiler::Schema;
    use rmcp::{model::Tool, object};

    use crate::{
        apps::{AppResource, PrefetchOperation},
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
            .into_operation(&schema, None, MutationMode::All, true, true)
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
            .into_operation(&schema, None, MutationMode::All, true, true)
            .unwrap()
            .unwrap(),
        );
        let second_prefetch_operation = Arc::new(
            RawOperation::from((
                "query SecondPrefetch($oranges: Int) { oranges(first: $oranges) }".to_string(),
                None,
            ))
            .into_operation(&schema, None, MutationMode::All, true, true)
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
            resource: AppResource::Local("blah".to_string()),
            csp_settings: None,
            uri: "ui://MyApp".parse().unwrap(),
            tools: vec![AppTool {
                operation: primary_operation.clone(),
                tool: Tool::new("ATool", "", JsonObject::new()),
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
}
