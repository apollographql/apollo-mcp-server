use std::sync::Arc;

use parking_lot::Mutex;
use rhai::EvalAltResult;

use crate::engine::RhaiEngine;

pub fn on_startup(engine: &Arc<Mutex<RhaiEngine>>) -> Result<(), Box<EvalAltResult>> {
    let hook_name = "on_startup";
    let mut engine_guard = engine.lock();

    if !engine_guard.ast_has_function(hook_name) {
        return Ok(());
    }

    engine_guard.execute_hook(hook_name, ())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;

    use super::on_startup;
    use crate::engine::RhaiEngine;

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
    fn should_return_error_when_hook_throws() {
        let engine = create_engine(
            r#"fn on_startup() {
                throw "startup failed";
            }"#,
        );

        let result = on_startup(&engine);

        assert!(result.is_err());
    }
}
