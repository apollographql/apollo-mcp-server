use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;
use rhai::module_resolvers::FileModuleResolver;
use rhai::{AST, Dynamic, Engine, EvalAltResult, FuncArgs, Position, Scope};
use tracing::info;

use crate::rhai::checkpoints::OnExecuteGraphqlOperationContext;
use crate::rhai::functions::{Json, RhaiHttp, RhaiSha256};
use crate::rhai::types::{HttpResponse, Promise, RhaiErrorCode, RhaiHeaderMap};

pub(crate) struct RhaiEngine {
    engine: Engine,
    scope: Scope<'static>,
    ast: AST,
}

impl Clone for RhaiEngine {
    fn clone(&self) -> Self {
        RhaiEngine::new()
    }
}

impl RhaiEngine {
    pub fn new() -> Self {
        let mut engine = Engine::new();

        let resolver = FileModuleResolver::new_with_path("/rhai");
        engine.set_module_resolver(resolver);

        engine.disable_symbol("await");

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
        RhaiSha256::register(engine);
        RhaiHttp::register(engine);
        Json::register(engine);
    }

    fn register_types(engine: &mut Engine) {
        RhaiHeaderMap::register(engine);
        HttpResponse::register(engine);
        OnExecuteGraphqlOperationContext::register(engine);
        RhaiErrorCode::register(engine);
        Promise::register(engine);
    }

    fn create_scope() -> Scope<'static> {
        let scope = Scope::new();
        // scope.push("my_string", "hello, world!");
        // scope.push_constant("MY_CONST", true);

        scope
    }

    pub fn load_from_path(&mut self) -> Result<(), Box<EvalAltResult>> {
        let mut main = PathBuf::from("rhai");
        main.push("main.rhai");

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
}
