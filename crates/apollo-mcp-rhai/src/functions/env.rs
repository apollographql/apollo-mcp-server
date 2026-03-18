use rhai::plugin::*;
use rhai::{Engine, Module};
use rhai::{export_module, exported_module};

pub struct RhaiEnv {}

impl RhaiEnv {
    pub fn register(engine: &mut Engine) {
        engine.register_static_module("Env", exported_module!(rhai_env_module).into());
    }
}

// Rhai's #[export_module] macro generates code that uses unwrap internally
#[allow(clippy::unwrap_used)]
#[export_module]
mod rhai_env_module {
    use rhai::ImmutableString;
    use tracing::warn;

    #[rhai_fn(pure)]
    pub(crate) fn get(name: &mut ImmutableString) -> String {
        match std::env::var(name.as_str()) {
            Ok(value) => value,
            Err(_) => {
                warn!("Environment variable '{}' is not set", name);
                String::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rhai::{Engine, EvalAltResult, FuncArgs, Scope};

    use tracing_test::traced_test;

    use crate::functions::RhaiEnv;

    fn run_rhai_script<T: Clone + Send + Sync + 'static>(
        script: &str,
        args: impl FuncArgs,
    ) -> Result<T, Box<EvalAltResult>> {
        let mut engine = Engine::new();
        let mut scope = Scope::new();

        RhaiEnv::register(&mut engine);

        let ast = engine.compile(script).expect("Script should have compiled");
        engine
            .run_ast_with_scope(&mut scope, &ast)
            .expect("Script should be able to run with AST");

        engine.call_fn::<T>(&mut scope, &ast, "test", args)
    }

    #[test]
    fn should_return_value_when_env_var_set() {
        unsafe { std::env::set_var("MY_AWESOME_VARIABLE", "my-value-12345") };
        let result = run_rhai_script::<String>(
            "fn test(){
        return Env::get(\"MY_AWESOME_VARIABLE\");
    }",
            (),
        )
        .expect("Should not error");

        assert_eq!(result, "my-value-12345");
    }

    #[traced_test]
    #[test]
    fn should_return_empty_string_when_no_env_var_set() {
        let result = run_rhai_script::<String>(
            "fn test(){
        return Env::get(\"MY_AWESOME_VARIABLE_NOT_SET\");
    }",
            (),
        )
        .expect("Should not error");

        assert_eq!(result, "");
        assert!(logs_contain(
            "Environment variable 'MY_AWESOME_VARIABLE_NOT_SET' is not set"
        ));
    }
}
