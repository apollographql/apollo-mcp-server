use std::path::PathBuf;

use rhai::module_resolvers::FileModuleResolver;
use rhai::{AST, Dynamic, Engine, EvalAltResult, FuncArgs, Position, Scope};
use tracing::info;

use crate::rhai::checkpoints::OnExecuteGraphqlOperationContext;
use crate::rhai::functions::RhaiSha256;
use crate::rhai::types::{RhaiErrorCode, RhaiHeaderMap};

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
    }

    fn register_types(engine: &mut Engine) {
        RhaiHeaderMap::register(engine);
        OnExecuteGraphqlOperationContext::register(engine);
        RhaiErrorCode::register(engine);
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

/*
#[export_module]
mod router_sha256 {
    use sha2::Digest;

    #[rhai_fn(pure)]
    pub(crate) fn digest(input: &mut ImmutableString) -> String {
        let hash = sha2::Sha256::digest(input.as_bytes());
        hex::encode(hash)
    }
}
*/
