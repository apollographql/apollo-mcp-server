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
        //engine.build_type::<OnExecuteGraphqlOperationContext>();
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
