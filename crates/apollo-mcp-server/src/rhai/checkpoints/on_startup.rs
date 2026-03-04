use std::sync::Arc;

use parking_lot::Mutex;
use tracing::error;

use crate::{errors::ServerError, rhai::engine::RhaiEngine};

pub fn on_startup(engine: &Arc<Mutex<RhaiEngine>>) -> Result<(), ServerError> {
    engine
        .lock()
        .execute_hook("on_startup", ())
        .map_err(|err| {
            error!("Error when executing on_startup hook: {err}");
            ServerError::RhaiError
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;

    use super::on_startup;
    use crate::rhai::engine::RhaiEngine;

    fn create_engine(script: &str) -> Arc<Mutex<RhaiEngine>> {
        let mut engine = RhaiEngine::new();
        engine
            .load_from_string(script)
            .expect("Script should compile");
        Arc::new(Mutex::new(engine))
    }

    #[test]
    fn should_succeed_when_no_hook_defined() {
        let engine = create_engine("");

        let result = on_startup(&engine);

        assert!(result.is_ok());
    }

    #[test]
    fn should_succeed_when_hook_runs_without_error() {
        let engine = create_engine(
            r#"fn on_startup() {
                // no-op
            }"#,
        );

        let result = on_startup(&engine);

        assert!(result.is_ok());
    }

    #[test]
    fn should_return_rhai_error_when_hook_throws() {
        let engine = create_engine(
            r#"fn on_startup() {
                throw "startup failed";
            }"#,
        );

        let result = on_startup(&engine);

        assert!(matches!(result, Err(crate::errors::ServerError::RhaiError)));
    }
}
