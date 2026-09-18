use rhai::EvalAltResult;

use crate::shared_engine::SharedRhaiEngine;

pub fn on_startup(engine: &SharedRhaiEngine) -> Result<(), Box<EvalAltResult>> {
    engine.current().execute_hook("on_startup", ())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::on_startup;
    use crate::shared_engine::SharedRhaiEngine;

    fn create_engine(script: &str) -> SharedRhaiEngine {
        SharedRhaiEngine::from_script("rhai", script).expect("Script should compile")
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
