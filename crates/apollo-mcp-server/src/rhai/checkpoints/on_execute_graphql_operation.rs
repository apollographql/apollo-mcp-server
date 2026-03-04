use std::sync::Arc;

use http::HeaderMap;
use parking_lot::Mutex;
use rhai::{CustomType, Engine, EvalAltResult, TypeBuilder};
use rmcp::model::ErrorCode;
use tracing::{error, warn};
use url::Url;

use crate::{
    errors::McpError,
    rhai::{
        engine::RhaiEngine,
        shared_mut::{SharedMut, WithMut},
        types::{RhaiErrorCode, RhaiHeaderMap},
    },
};

#[derive(Clone, Debug, CustomType)]
pub(crate) struct OnExecuteGraphqlOperationContext {
    pub(crate) endpoint: String,
    pub(crate) headers: RhaiHeaderMap,
}

impl OnExecuteGraphqlOperationContext {
    pub(crate) fn register(engine: &mut Engine) {
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
            );
    }
}

pub fn on_execute_graphql_operation(
    engine: &Arc<Mutex<RhaiEngine>>,
    endpoint: &Url,
    headers: &HeaderMap,
) -> Result<(Url, HeaderMap), McpError> {
    let context = OnExecuteGraphqlOperationContext {
        endpoint: endpoint.to_string(),
        headers: RhaiHeaderMap::from(headers.clone()),
    };

    let shared_context = Arc::new(Mutex::new(context));

    engine
        .lock()
        .execute_hook("on_execute_graphql_operation", (shared_context.clone(),))
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
    use parking_lot::Mutex;
    use rmcp::model::ErrorCode;
    use url::Url;

    use super::on_execute_graphql_operation;
    use crate::rhai::engine::RhaiEngine;

    fn create_engine(script: &str) -> Arc<Mutex<RhaiEngine>> {
        let mut engine = RhaiEngine::new();
        engine
            .load_from_string(script)
            .expect("Script should compile");
        Arc::new(Mutex::new(engine))
    }

    #[test]
    fn should_pass_through_when_no_hook_defined() {
        let engine = create_engine("");
        let url = Url::parse("https://example.com/graphql").expect("Valid URL");
        let headers = HeaderMap::new();

        let (result_url, result_headers) =
            on_execute_graphql_operation(&engine, &url, &headers).expect("Should not error");

        assert_eq!(result_url, url);
        assert!(result_headers.is_empty());
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
            on_execute_graphql_operation(&engine, &url, &headers).expect("Should not error");

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
            on_execute_graphql_operation(&engine, &url, &headers).expect("Should not error");

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
            on_execute_graphql_operation(&engine, &url, &headers).expect("Should not error");

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

        let err =
            on_execute_graphql_operation(&engine, &url, &headers).expect_err("Should return error");

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

        let err =
            on_execute_graphql_operation(&engine, &url, &headers).expect_err("Should return error");

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

        let err =
            on_execute_graphql_operation(&engine, &url, &headers).expect_err("Should return error");

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

        let err =
            on_execute_graphql_operation(&engine, &url, &headers).expect_err("Should return error");

        assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
    }
}
