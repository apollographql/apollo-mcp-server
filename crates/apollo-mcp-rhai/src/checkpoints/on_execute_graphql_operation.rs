use std::sync::Arc;

use http::HeaderMap;
use http::request::Parts;
use parking_lot::Mutex;
use rhai::{CustomType, Engine, EvalAltResult, TypeBuilder};
use rmcp::model::ErrorCode;
use tracing::{error, warn};
use url::Url;

use crate::{
    engine::RhaiEngine,
    shared_mut::{SharedMut, WithMut},
    types::{RhaiErrorCode, RhaiHeaderMap, RhaiHttpParts},
};

pub type McpError = rmcp::model::ErrorData;

#[derive(Clone, Debug, CustomType)]
pub struct OnExecuteGraphqlOperationContext {
    pub endpoint: String,
    pub headers: RhaiHeaderMap,
    pub incoming_request: RhaiHttpParts,
    pub tool_name: String,
}

impl OnExecuteGraphqlOperationContext {
    pub fn register(engine: &mut Engine) {
        engine
            .register_type::<OnExecuteGraphqlOperationContext>()
            .register_get_set(
                "endpoint",
                |obj: &mut SharedMut<OnExecuteGraphqlOperationContext>| -> String {
                    obj.with_mut(|ctx| ctx.endpoint.clone())
                },
                |obj: &mut SharedMut<OnExecuteGraphqlOperationContext>, value: String| {
                    obj.with_mut(|ctx| ctx.endpoint = value);
                },
            )
            .register_get_set(
                "headers",
                |obj: &mut SharedMut<OnExecuteGraphqlOperationContext>| -> RhaiHeaderMap {
                    obj.with_mut(|ctx| ctx.headers.clone())
                },
                |obj: &mut SharedMut<OnExecuteGraphqlOperationContext>, value: RhaiHeaderMap| {
                    obj.with_mut(|ctx| ctx.headers = value);
                },
            )
            .register_get(
                "incoming_request",
                |obj: &mut SharedMut<OnExecuteGraphqlOperationContext>| -> RhaiHttpParts {
                    obj.with_mut(|ctx| ctx.incoming_request.clone())
                },
            )
            .register_get(
                "tool_name",
                |obj: &mut SharedMut<OnExecuteGraphqlOperationContext>| -> String {
                    obj.with_mut(|ctx| ctx.tool_name.clone())
                },
            );
    }
}

pub fn on_execute_graphql_operation(
    engine: &Arc<Mutex<RhaiEngine>>,
    endpoint: &Url,
    headers: &HeaderMap,
    axum_parts: Option<&Parts>,
    tool_name: &str,
) -> Result<(Url, HeaderMap), McpError> {
    let hook_name = "on_execute_graphql_operation";
    let mut engine_guard = engine.lock();

    // Exit early if method doesn't exist, allow us to skip some more expensive cloning later in this method
    if !engine_guard.ast_has_function(hook_name) {
        return Ok((endpoint.clone(), headers.clone()));
    }

    let context = OnExecuteGraphqlOperationContext {
        endpoint: endpoint.to_string(),
        headers: RhaiHeaderMap::from(headers.clone()),
        incoming_request: match axum_parts {
            Some(parts) => RhaiHttpParts::from(parts.clone()),
            None => RhaiHttpParts::default(),
        },
        tool_name: tool_name.to_string(),
    };

    let shared_context = Arc::new(Mutex::new(context));

    engine_guard
        .execute_hook(hook_name, (shared_context.clone(),))
        // TODO: How much of this could be made generic and/or moved into execute_hook?
        .map_err(|err| match *err {
            EvalAltResult::ErrorRuntime(error_data, _) => {
                match error_data.as_map_ref() {
                    Ok(error_data) => {
                        let message = error_data.get("message").map(|val| val.to_string()).unwrap_or_else(|| {
                            warn!("Error was thrown with no 'message' field, using default.");
                            "Internal error".to_string()
                        });
                        let code = error_data
                            .get("code")
                            .and_then(|val| val.clone().try_cast::<RhaiErrorCode>())
                            .unwrap_or(RhaiErrorCode::InternalError);
                        McpError::new(ErrorCode::from(code), message, None)
                    },
                    Err(inner_err) =>{
                        error!("Error when executing on_execute_graphql_operation hook: Error when converting error_data to map: {inner_err}, actual error: {error_data}");
                        McpError::new(ErrorCode::INTERNAL_ERROR, "Internal error", None)
                    },
                }
            }
            _ => {
                error!("Error when executing on_execute_graphql_operation hook: {err}");
                McpError::new(ErrorCode::INTERNAL_ERROR, "Internal error", None)
            }
        })?;

    let context = shared_context.lock();

    let url = Url::parse(context.endpoint.as_str()).map_err(|err| {
        error!("Error when executing on_execute_graphql_operation hook: Error parsing context.endpoint: {err}");
        McpError::new(ErrorCode::INTERNAL_ERROR, "Internal error", None)
    })?;
    let headers = context.headers.as_header_map();

    Ok((url, headers))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use http::HeaderMap;
    use http::request::Parts;
    use parking_lot::Mutex;
    use rmcp::model::ErrorCode;
    use url::Url;

    use super::on_execute_graphql_operation;
    use crate::engine::RhaiEngine;

    fn create_engine(script: &str) -> Arc<Mutex<RhaiEngine>> {
        let mut engine = RhaiEngine::new("rhai");
        engine
            .load_from_string(script)
            .expect("Script should compile");
        Arc::new(Mutex::new(engine))
    }

    fn create_parts(method: &str, uri: &str, headers: HeaderMap) -> Parts {
        let mut builder = http::Request::builder().method(method).uri(uri);
        if let Some(h) = builder.headers_mut() {
            *h = headers;
        }
        builder.body(()).unwrap().into_parts().0
    }

    #[test]
    fn should_pass_through_when_no_hook_defined() {
        let engine = create_engine("");
        let url = Url::parse("https://example.com/graphql").expect("Valid URL");
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer token123".parse().unwrap());

        let (result_url, result_headers) =
            on_execute_graphql_operation(&engine, &url, &headers, None, "my-tool")
                .expect("Should not error");

        assert_eq!(result_url, url);
        assert_eq!(
            result_headers.get("authorization").unwrap(),
            "Bearer token123"
        );
    }

    #[test]
    fn should_return_original_values_when_hook_does_not_modify_context() {
        let engine = create_engine(
            r#"fn on_execute_graphql_operation(ctx) {
                // no-op
            }"#,
        );
        let url = Url::parse("https://example.com/graphql").expect("Valid URL");
        let headers = HeaderMap::new();

        let (result_url, _result_headers) =
            on_execute_graphql_operation(&engine, &url, &headers, None, "my-tool")
                .expect("Should not error");

        assert_eq!(result_url, url);
    }

    #[test]
    fn should_return_modified_endpoint() {
        let engine = create_engine(
            r#"fn on_execute_graphql_operation(ctx) {
                ctx.endpoint = "https://modified.example.com/graphql";
            }"#,
        );
        let url = Url::parse("https://example.com/graphql").expect("Valid URL");
        let headers = HeaderMap::new();

        let (result_url, _) =
            on_execute_graphql_operation(&engine, &url, &headers, None, "my-tool")
                .expect("Should not error");

        assert_eq!(
            result_url,
            Url::parse("https://modified.example.com/graphql").expect("Valid URL")
        );
    }

    #[test]
    fn should_return_modified_headers() {
        let engine = create_engine(
            r#"fn on_execute_graphql_operation(ctx) {
                let h = ctx.headers;
                h["x-custom"] = "custom-value";
                ctx.headers = h;
            }"#,
        );
        let url = Url::parse("https://example.com/graphql").expect("Valid URL");
        let headers = HeaderMap::new();

        let (_, result_headers) =
            on_execute_graphql_operation(&engine, &url, &headers, None, "my-tool")
                .expect("Should not error");

        assert_eq!(result_headers.get("x-custom").unwrap(), "custom-value");
    }

    #[test]
    fn should_return_error_with_message_and_code() {
        let engine = create_engine(
            r#"fn on_execute_graphql_operation(ctx) {
                throw #{
                    message: "unauthorized request",
                    code: ErrorCode::INVALID_REQUEST
                };
            }"#,
        );
        let url = Url::parse("https://example.com/graphql").expect("Valid URL");
        let headers = HeaderMap::new();

        let err = on_execute_graphql_operation(&engine, &url, &headers, None, "my-tool")
            .expect_err("Should return error");

        assert_eq!(err.code, ErrorCode::INVALID_REQUEST);
        assert_eq!(err.message, "unauthorized request");
    }

    #[test]
    fn should_return_error_with_default_message_when_message_field_missing() {
        let engine = create_engine(
            r#"fn on_execute_graphql_operation(ctx) {
                throw #{
                    code: ErrorCode::INVALID_REQUEST
                };
            }"#,
        );
        let url = Url::parse("https://example.com/graphql").expect("Valid URL");
        let headers = HeaderMap::new();

        let err = on_execute_graphql_operation(&engine, &url, &headers, None, "my-tool")
            .expect_err("Should return error");

        assert_eq!(err.message, "Internal error");
    }

    #[test]
    fn should_return_internal_error_when_throw_is_non_map() {
        let engine = create_engine(
            r#"fn on_execute_graphql_operation(ctx) {
                throw "something went wrong";
            }"#,
        );
        let url = Url::parse("https://example.com/graphql").expect("Valid URL");
        let headers = HeaderMap::new();

        let err = on_execute_graphql_operation(&engine, &url, &headers, None, "my-tool")
            .expect_err("Should return error");

        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
        assert_eq!(err.message, "Internal error");
    }

    #[test]
    fn should_return_error_when_hook_sets_invalid_url() {
        let engine = create_engine(
            r#"fn on_execute_graphql_operation(ctx) {
                ctx.endpoint = "not a valid url";
            }"#,
        );
        let url = Url::parse("https://example.com/graphql").expect("Valid URL");
        let headers = HeaderMap::new();

        let err = on_execute_graphql_operation(&engine, &url, &headers, None, "my-tool")
            .expect_err("Should return error");

        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
    }

    #[test]
    fn should_read_tool_name() {
        let engine = create_engine(
            r#"fn on_execute_graphql_operation(ctx) {
                let h = ctx.headers;
                h["x-tool-name"] = ctx.tool_name;
                ctx.headers = h;
            }"#,
        );
        let url = Url::parse("https://example.com/graphql").expect("Valid URL");
        let headers = HeaderMap::new();

        let (_, result_headers) =
            on_execute_graphql_operation(&engine, &url, &headers, None, "my-tool")
                .expect("Should not error");

        assert_eq!(result_headers.get("x-tool-name").unwrap(), "my-tool");
    }

    #[test]
    fn should_read_incoming_request_method() {
        let engine = create_engine(
            r#"fn on_execute_graphql_operation(ctx) {
                let h = ctx.headers;
                h["x-method"] = ctx.incoming_request.method;
                ctx.headers = h;
            }"#,
        );
        let url = Url::parse("https://example.com/graphql").expect("Valid URL");
        let headers = HeaderMap::new();
        let parts = create_parts("POST", "/mcp", HeaderMap::new());

        let (_, result_headers) =
            on_execute_graphql_operation(&engine, &url, &headers, Some(&parts), "my-tool")
                .expect("Should not error");

        assert_eq!(result_headers.get("x-method").unwrap(), "POST");
    }

    #[test]
    fn should_read_incoming_request_uri() {
        let engine = create_engine(
            r#"fn on_execute_graphql_operation(ctx) {
                let h = ctx.headers;
                h["x-uri"] = ctx.incoming_request.uri;
                ctx.headers = h;
            }"#,
        );
        let url = Url::parse("https://example.com/graphql").expect("Valid URL");
        let headers = HeaderMap::new();
        let parts = create_parts("GET", "/mcp/sse", HeaderMap::new());

        let (_, result_headers) =
            on_execute_graphql_operation(&engine, &url, &headers, Some(&parts), "my-tool")
                .expect("Should not error");

        assert_eq!(result_headers.get("x-uri").unwrap(), "/mcp/sse");
    }

    #[test]
    fn should_read_incoming_request_headers() {
        let engine = create_engine(
            r#"fn on_execute_graphql_operation(ctx) {
                let h = ctx.headers;
                h["x-forwarded"] = ctx.incoming_request.headers["authorization"];
                ctx.headers = h;
            }"#,
        );
        let url = Url::parse("https://example.com/graphql").expect("Valid URL");
        let headers = HeaderMap::new();
        let mut incoming_headers = HeaderMap::new();
        incoming_headers.insert("authorization", "Bearer token123".parse().unwrap());
        let parts = create_parts("POST", "/mcp", incoming_headers);

        let (_, result_headers) =
            on_execute_graphql_operation(&engine, &url, &headers, Some(&parts), "my-tool")
                .expect("Should not error");

        assert_eq!(
            result_headers.get("x-forwarded").unwrap(),
            "Bearer token123"
        );
    }
}
