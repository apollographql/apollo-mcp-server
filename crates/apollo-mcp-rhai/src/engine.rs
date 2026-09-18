use std::path::{Path, PathBuf};

use rhai::module_resolvers::FileModuleResolver;
use rhai::{AST, Dynamic, Engine, EvalAltResult, FuncArgs, Position, Scope};
use tracing::info;

use crate::checkpoints::OnExecuteGraphqlOperationContext;
use crate::functions::{Json, RhaiEnv, RhaiHttp, RhaiRegex, RhaiSha256};
use crate::types::{HttpResponse, Promise, RhaiErrorCode, RhaiHeaderMap, RhaiHttpParts};

pub struct RhaiEngine {
    engine: Engine,
    scope: Scope<'static>,
    ast: AST,
    main_file: PathBuf,
}

impl RhaiEngine {
    pub(crate) fn new(script_dir: impl AsRef<Path>) -> Self {
        let script_dir = script_dir.as_ref();
        let main_file = script_dir.join("main.rhai");

        let mut engine = Engine::new();

        let resolver = FileModuleResolver::new_with_path(script_dir);
        engine.set_module_resolver(resolver);

        let scope = Self::create_scope();

        Self::register_functions(&mut engine);
        Self::register_types(&mut engine);
        Self::register_logging(&mut engine);

        Self {
            engine,
            scope,
            ast: AST::empty(),
            main_file,
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

    pub(crate) fn load_from_path(&mut self) -> Result<(), Box<EvalAltResult>> {
        if !self.main_file.exists() {
            return Ok(());
        }

        self.ast = self
            .engine
            .compile_file(self.main_file.clone())
            .map_err(|err| format!("in Rhai script {}: {}", self.main_file.display(), err))?;

        // Run the AST with our scope to put any global variables
        // defined in scripts into scope.
        self.engine.run_ast_with_scope(&mut self.scope, &self.ast)?;

        Ok(())
    }

    /// Compiles the scripts in `script_dir` into a new engine.
    /// Unlike [`Self::load_from_path`], a missing main script is an error, so callers
    /// reloading scripts can keep the engine they already have.
    pub(crate) fn compile_from_path(
        script_dir: impl AsRef<Path>,
    ) -> Result<Self, Box<EvalAltResult>> {
        let mut engine = Self::new(script_dir);

        if !engine.main_file.exists() {
            return Err(format!("Rhai script {} not found", engine.main_file.display()).into());
        }

        engine.load_from_path()?;
        Ok(engine)
    }

    pub(crate) fn execute_hook(
        &self,
        hook_name: &str,
        args: impl FuncArgs,
    ) -> Result<Option<Dynamic>, Box<EvalAltResult>> {
        if self.ast_has_function(hook_name) {
            // CONCURRENCY: a per-invocation copy of the top-level scope keeps hook execution
            // off an exclusive lock, which would deadlock a script that blocks on HTTP. The copy
            // is what makes top-level values visible: `call_fn` re-runs the top-level statements
            // but discards their bindings. Neither half can go away, see the tests below.
            let mut scope = self.scope.clone();

            return Ok(Some(
                self.engine
                    .call_fn::<Dynamic>(&mut scope, &self.ast, hook_name, args)?,
            ));
        }

        Ok(None)
    }

    pub(crate) fn ast_has_function(&self, name: &str) -> bool {
        self.ast.iter_functions().any(|fn_def| fn_def.name == name)
    }

    #[cfg(test)]
    pub(crate) fn load_from_string(&mut self, script: &str) -> Result<(), Box<EvalAltResult>> {
        self.ast = self.engine.compile(script)?;
        self.engine.run_ast_with_scope(&mut self.scope, &self.ast)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_engine(script: &str) -> RhaiEngine {
        let mut engine = RhaiEngine::new("rhai");
        engine
            .load_from_string(script)
            .expect("Script should compile");
        engine
    }

    #[test]
    fn should_compile_and_run_valid_script() {
        let mut engine = RhaiEngine::new("rhai");

        let result = engine.load_from_string("let x = 1 + 2;");

        assert!(result.is_ok());
    }

    #[test]
    fn should_return_error_for_invalid_script() {
        let mut engine = RhaiEngine::new("rhai");

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
        let engine = RhaiEngine::new("rhai");

        assert!(!engine.ast_has_function("anything"));
    }

    #[test]
    fn should_return_none_when_hook_not_defined() {
        let engine = create_engine("");

        let result = engine
            .execute_hook("nonexistent_hook", ())
            .expect("Should not error");

        assert!(result.is_none());
    }

    #[test]
    fn should_return_some_with_return_value() {
        let engine = create_engine("fn my_hook() { 42 }");

        let result = engine
            .execute_hook("my_hook", ())
            .expect("Should not error");

        assert_eq!(result.unwrap().as_int().unwrap(), 42);
    }

    #[test]
    fn should_pass_arguments_to_hook() {
        let engine = create_engine("fn add(a, b) { a + b }");

        let result = engine
            .execute_hook("add", (3_i64, 4_i64))
            .expect("Should not error");

        assert_eq!(result.unwrap().as_int().unwrap(), 7);
    }

    #[test]
    fn should_return_error_when_hook_throws() {
        let engine = create_engine(r#"fn failing() { throw "oops"; }"#);

        let result = engine.execute_hook("failing", ());

        assert!(result.is_err());
    }

    #[test]
    fn should_access_registered_json_functions() {
        let engine = create_engine(
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
        let engine = create_engine(
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
        let mut engine = RhaiEngine::new("rhai");

        let result = engine.load_from_path();

        assert!(result.is_ok());
    }

    #[test]
    fn should_isolate_global_variable_writes_between_invocations() {
        let engine = create_engine("let counter = 0;\nfn bump() { counter += 1; counter }");

        let first = engine.execute_hook("bump", ()).expect("Should not error");
        let second = engine.execute_hook("bump", ()).expect("Should not error");

        assert_eq!(first.unwrap().as_int().unwrap(), 1);
        assert_eq!(second.unwrap().as_int().unwrap(), 1);
    }

    #[test]
    fn should_expose_top_level_values_through_the_scope_copy() {
        // A fresh scope is not enough: `call_fn` re-runs the top level but drops its bindings.
        let engine = create_engine("let global_var = 100;\nfn get_global() { global_var }");

        let result = engine
            .execute_hook("get_global", ())
            .expect("Should not error");

        assert_eq!(result.unwrap().as_int().unwrap(), 100);
    }

    #[test]
    #[tracing_test::traced_test]
    fn should_reevaluate_top_level_statements_on_every_invocation() {
        let engine = create_engine("print(\"top level ran\");\nfn noop() { 42 }");

        engine.execute_hook("noop", ()).expect("Should not error");

        logs_assert(|lines: &[&str]| {
            // Once when the script loaded, once for the hook call.
            match lines
                .iter()
                .filter(|line| line.contains("top level ran"))
                .count()
            {
                2 => Ok(()),
                count => Err(format!(
                    "expected the top level to run at load and once per hook call, saw {count}"
                )),
            }
        });
    }
}
