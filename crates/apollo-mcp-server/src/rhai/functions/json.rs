use rhai::plugin::*;
use rhai::{Engine, Module};
use rhai::{export_module, exported_module};

pub(crate) struct Json {}

impl Json {
    pub(crate) fn register(engine: &mut Engine) {
        engine.register_static_module("JSON", exported_module!(json_module).into());
    }
}

#[export_module]
mod json_module {
    use serde_json::Value;

    #[rhai_fn(name = "stringify", pure)]
    pub(crate) fn from_value(x: &mut Value) -> String {
        format!("{x:?}")
    }

    #[rhai_fn(name = "stringify", pure, return_raw)]
    pub(crate) fn from_dynamic(input: &mut Dynamic) -> Result<String, Box<EvalAltResult>> {
        serde_json::to_string(input).map_err(|e| e.to_string().into())
    }

    #[rhai_fn(pure, return_raw)]
    pub(crate) fn parse(input: &mut ImmutableString) -> Result<Dynamic, Box<EvalAltResult>> {
        serde_json::from_str(input).map_err(|e| e.to_string().into())
    }
}

#[cfg(test)]
mod tests {
    use rhai::{Engine, EvalAltResult, FuncArgs, Scope};

    use crate::rhai::functions::Json;

    fn run_rhai_script<T: Clone + Send + Sync + 'static>(
        script: &str,
        args: impl FuncArgs,
    ) -> Result<T, Box<EvalAltResult>> {
        let mut engine = Engine::new();
        let mut scope = Scope::new();

        Json::register(&mut engine);

        let ast = engine.compile(script).expect("Script should have compiled");
        engine
            .run_ast_with_scope(&mut scope, &ast)
            .expect("Script should be able to run with AST");

        engine.call_fn::<T>(&mut scope, &ast, "test", args)
    }

    #[test]
    fn should_stringify_a_map() {
        let result = run_rhai_script::<String>(
            r#"fn test() {
                let obj = #{name: "apollo", version: 1};
                return JSON::stringify(obj);
            }"#,
            (),
        )
        .expect("Should not error");

        let parsed: serde_json::Value =
            serde_json::from_str(&result).expect("Should be valid JSON");
        assert_eq!(parsed["name"], "apollo");
    }

    #[test]
    fn should_stringify_a_string() {
        let result = run_rhai_script::<String>(
            r#"fn test() {
                let val = "hello";
                return JSON::stringify(val);
            }"#,
            (),
        )
        .expect("Should not error");

        assert_eq!(result, r#""hello""#);
    }

    #[test]
    fn should_stringify_a_number() {
        let result = run_rhai_script::<String>(
            r#"fn test() {
                let val = 42;
                return JSON::stringify(val);
            }"#,
            (),
        )
        .expect("Should not error");

        assert_eq!(result, "42");
    }

    #[test]
    fn should_parse_json_object() {
        let result = run_rhai_script::<String>(
            r#"fn test() {
                let data = JSON::parse("{\"name\":\"apollo\"}");
                return data.name;
            }"#,
            (),
        )
        .expect("Should not error");

        assert_eq!(result, "apollo");
    }

    #[test]
    fn should_parse_json_array() {
        let result = run_rhai_script::<i64>(
            r#"fn test() {
                let data = JSON::parse("[1, 2, 3]");
                return data.len();
            }"#,
            (),
        )
        .expect("Should not error");

        assert_eq!(result, 3);
    }

    #[test]
    fn should_return_error_for_invalid_json() {
        let result = run_rhai_script::<rhai::Dynamic>(
            r#"fn test() {
                return JSON::parse("not valid json");
            }"#,
            (),
        );

        assert!(result.is_err());
    }
}
