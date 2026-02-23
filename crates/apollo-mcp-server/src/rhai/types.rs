use http::header::{HeaderName, InvalidHeaderName, InvalidHeaderValue};
use http::{HeaderMap, HeaderValue};
use parking_lot::Mutex;
use rhai::plugin::*;
use rhai::{CustomType, Dynamic, Engine, EvalAltResult, Module, Position, Shared, TypeBuilder};
use rhai::{export_module, exported_module};
use rmcp::model::ErrorCode;

/// With the `sync` feature, `rhai::Shared` is `Arc`, so this is `Arc<Mutex<T>>`.
pub(crate) type SharedMut<T> = Shared<Mutex<T>>;

pub(crate) trait WithMut<T> {
    /// Run a closure with a mutable reference to the inner value.
    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;
}

impl<T> WithMut<T> for SharedMut<T> {
    fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.lock();
        f(&mut guard)
    }
}

#[derive(Clone, Debug, CustomType)]
pub(crate) struct RhaiHeaderMap {
    header_map: HeaderMap,
}

impl From<HeaderMap> for RhaiHeaderMap {
    fn from(header_map: HeaderMap) -> Self {
        Self { header_map }
    }
}

impl RhaiHeaderMap {
    fn get_field(&mut self, key: String) -> String {
        self.header_map
            .get(key.as_str())
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    }

    fn set_field(&mut self, key: String, value: String) -> Result<(), Box<EvalAltResult>> {
        let header_value = HeaderValue::from_str(&value).map_err(|e: InvalidHeaderValue| {
            Box::new(EvalAltResult::ErrorRuntime(
                format!("invalid header value: {e}").into(),
                Position::NONE,
            ))
        })?;
        let header_name =
            HeaderName::from_bytes(key.as_bytes()).map_err(|e: InvalidHeaderName| {
                Box::new(EvalAltResult::ErrorRuntime(
                    format!("invalid header name: {e}").into(),
                    Position::NONE,
                ))
            })?;
        self.header_map.insert(header_name, header_value);
        Ok(())
    }

    pub(crate) fn register(engine: &mut Engine) {
        engine
            .register_type::<RhaiHeaderMap>()
            .register_indexer_get(RhaiHeaderMap::get_field)
            .register_indexer_set(RhaiHeaderMap::set_field);
    }

    pub(crate) fn as_header_map(&self) -> HeaderMap {
        self.header_map.clone()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum RhaiErrorCode {
    InvalidRequest,
    InternalError,
}

impl RhaiErrorCode {
    pub(crate) fn register(engine: &mut Engine) {
        engine
            .register_type_with_name::<RhaiErrorCode>("ErrorCode")
            .register_static_module("ErrorCode", exported_module!(rhai_error_code_module).into());
    }
}

impl From<RhaiErrorCode> for ErrorCode {
    fn from(code: RhaiErrorCode) -> Self {
        match code {
            RhaiErrorCode::InvalidRequest => ErrorCode::INVALID_REQUEST,
            RhaiErrorCode::InternalError => ErrorCode::INTERNAL_ERROR,
        }
    }
}

#[export_module]
mod rhai_error_code_module {

    use crate::rhai::types::RhaiErrorCode;

    pub const INVALID_REQUEST: RhaiErrorCode = RhaiErrorCode::InvalidRequest;
    pub const INTERNAL_ERROR: RhaiErrorCode = RhaiErrorCode::InternalError;
}
