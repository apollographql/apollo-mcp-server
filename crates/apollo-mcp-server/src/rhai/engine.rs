use std::path::PathBuf;

use rhai::module_resolvers::FileModuleResolver;
use rhai::{AST, Dynamic, Engine, EvalAltResult, FuncArgs, Position, Scope};
use tracing::info;

use crate::rhai::checkpoints::OnExecuteGraphqlOperationContext;
use crate::rhai::functions::{Json, RhaiEnv, RhaiHttp, RhaiRegex, RhaiSha256};
use crate::rhai::types::{HttpResponse, Promise, RhaiErrorCode, RhaiHeaderMap, RhaiHttpParts};

pub(crate) struct RhaiEngine {
    engine: Engine,
    scope: Scope<'static>,
    ast: AST,
}

impl RhaiEngine {
    pub fn new() -> Self {
        let mut engine = Engine::new();

        let resolver = FileModuleResolver::new_with_path("/rhai");
        engine.set_module_resolver(resolver);

        let scope = Self::create_scope();

        Self::register_functions(&mut engine);
        Self::register_types(&mut engine);
        Self::register_logging(&mut engine);

        Self {
            engine,
            scope,
            ast: AST::empty(),
        }
    }

    fn register_logging(engine: &mut Engine) {
        engine.on_print(|text| info!("{text}"));

        engine.on_debug(|text, source, pos| match (source, pos) {
            (Some(source), Position::NONE) => info!("{source} | {text}"),
            (Some(source), pos) => info!("{source} @ {pos:?} | {text}"),
            (None, Position::NONE) => info!("{text}"),
            (None, pos) => info!("{pos:?} | {text}"),
        });
    }

    fn register_functions(engine: &mut Engine) {
        RhaiEnv::register(engine);
        RhaiSha256::register(engine);
        RhaiHttp::register(engine);
        Json::register(engine);
        RhaiRegex::register(engine);
    }

    fn register_types(engine: &mut Engine) {
        RhaiHeaderMap::register(engine);
        RhaiHttpParts::register(engine);
        HttpResponse::register(engine);
        OnExecuteGraphqlOperationContext::register(engine);
        RhaiErrorCode::register(engine);
        Promise::register(engine);
    }

    fn create_scope() -> Scope<'static> {
        Scope::new()
    }

    pub fn load_from_path(&mut self) -> Result<(), Box<EvalAltResult>> {
        let mut main = PathBuf::from("rhai");
        main.push("main.rhai");

        if !main.exists() {
            return Ok(());
        }

        self.ast = self
            .engine
            .compile_file(main.clone())
            .map_err(|err| format!("in Rhai script {}: {}", main.display(), err))?;

        // Run the AST with our scope to put any global variables
        // defined in scripts into scope.
        self.engine.run_ast_with_scope(&mut self.scope, &self.ast)?;

        Ok(())
    }

    pub fn execute_hook(
        &mut self,
        hook_name: &str,
        args: impl FuncArgs,
    ) -> Result<Option<Dynamic>, Box<EvalAltResult>> {
        if self.ast_has_function(hook_name) {
            return Ok(Some(self.engine.call_fn::<Dynamic>(
                &mut self.scope,
                &self.ast,
                hook_name,
                args,
            )?));
        }

        Ok(None)
    }

    pub fn ast_has_function(&self, name: &str) -> bool {
        self.ast.iter_functions().any(|fn_def| fn_def.name == name)
    }

    #[cfg(test)]
    pub fn load_from_string(&mut self, script: &str) -> Result<(), Box<EvalAltResult>> {
        self.ast = self.engine.compile(script)?;
        self.engine.run_ast_with_scope(&mut self.scope, &self.ast)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_engine(script: &str) -> RhaiEngine {
        let mut engine = RhaiEngine::new();
        engine
            .load_from_string(script)
            .expect("Script should compile");
        engine
    }

    #[test]
    fn should_compile_and_run_valid_script() {
        let mut engine = RhaiEngine::new();

        let result = engine.load_from_string("let x = 1 + 2;");

        assert!(result.is_ok());
    }

    #[test]
    fn should_return_error_for_invalid_script() {
        let mut engine = RhaiEngine::new();

        let result = engine.load_from_string("this is not valid rhai {{{");

        assert!(result.is_err());
    }

    #[test]
    fn should_return_true_when_function_exists() {
        let engine = create_engine("fn my_hook() { 42 }");

        assert!(engine.ast_has_function("my_hook"));
    }

    #[test]
    fn should_return_false_when_function_does_not_exist() {
        let engine = create_engine("fn my_hook() { 42 }");

        assert!(!engine.ast_has_function("nonexistent"));
    }

    #[test]
    fn should_return_false_for_empty_ast() {
        let engine = RhaiEngine::new();

        assert!(!engine.ast_has_function("anything"));
    }

    #[test]
    fn should_return_none_when_hook_not_defined() {
        let mut engine = create_engine("");

        let result = engine
            .execute_hook("nonexistent_hook", ())
            .expect("Should not error");

        assert!(result.is_none());
    }

    #[test]
    fn should_return_some_with_return_value() {
        let mut engine = create_engine("fn my_hook() { 42 }");

        let result = engine
            .execute_hook("my_hook", ())
            .expect("Should not error");

        assert_eq!(result.unwrap().as_int().unwrap(), 42);
    }

    #[test]
    fn should_pass_arguments_to_hook() {
        let mut engine = create_engine("fn add(a, b) { a + b }");

        let result = engine
            .execute_hook("add", (3_i64, 4_i64))
            .expect("Should not error");

        assert_eq!(result.unwrap().as_int().unwrap(), 7);
    }

    #[test]
    fn should_return_error_when_hook_throws() {
        let mut engine = create_engine(r#"fn failing() { throw "oops"; }"#);

        let result = engine.execute_hook("failing", ());

        assert!(result.is_err());
    }

    #[test]
    fn should_access_registered_json_functions() {
        let mut engine = create_engine(
            r#"fn parse_json() {
                let obj = JSON::parse("{\"key\": \"value\"}");
                obj["key"]
            }"#,
        );

        let result = engine
            .execute_hook("parse_json", ())
            .expect("Should not error");

        assert_eq!(result.unwrap().into_string().unwrap(), "value");
    }

    #[test]
    fn should_access_registered_sha256_functions() {
        let mut engine = create_engine(
            r#"fn hash_it() {
                Sha256::digest("hello")
            }"#,
        );

        let result = engine
            .execute_hook("hash_it", ())
            .expect("Should not error");

        assert_eq!(
            result.unwrap().into_string().unwrap(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn should_return_ok_when_script_file_not_found() {
        let mut engine = RhaiEngine::new();

        let result = engine.load_from_path();

        assert!(result.is_ok());
    }

    #[test]
    fn should_persist_global_variables_in_scope() {
        let mut engine = create_engine("let global_var = 100;");

        let result = engine
            .execute_hook("get_var", ())
            .expect("Should not error");

        // The hook doesn't exist, so it should return None
        assert!(result.is_none());

        // But we can verify the script ran by loading another script that uses the scope
        // The scope should have 'global_var' from the first script
        let ast = engine
            .engine
            .compile("fn get_global() { global_var }")
            .expect("Should compile");
        engine.ast = ast;

        let result = engine
            .execute_hook("get_global", ())
            .expect("Should not error");

        assert_eq!(result.unwrap().as_int().unwrap(), 100);
    }
}
