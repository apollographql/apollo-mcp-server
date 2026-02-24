use rhai::{Engine, Module};
use rhai::{export_module, exported_module};
use rmcp::model::ErrorCode;

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
