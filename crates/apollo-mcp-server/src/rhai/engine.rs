use std::path::PathBuf;

use rhai::module_resolvers::FileModuleResolver;
use rhai::{AST, Dynamic, Engine, EvalAltResult, FuncArgs, Scope};

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

        Self::register_modules();
        Self::handle_logging();
        Self::register_functions();

        Self {
            engine,
            scope,
            ast: AST::empty(),
        }
    }

    fn register_modules() {
        //  let mut module = exported_module!(router_plugin);
        // combine_with_exported_module!(&mut module, "header", router_header_map);

        // let base64_module = exported_module!(router_base64);

        // engine
        //     // Register our plugin module
        //     .register_global_module(module.into())
        //     // Register our base64 module (not global)
        //     .register_static_module("base64", base64_module.into())
    }

    fn handle_logging() {
        // // Default print/debug implementations
        // engine.on_print(|text| println!("{text}"));

        // engine.on_debug(|text, source, pos| match (source, pos) {
        //     (Some(source), Position::NONE) => println!("{source} | {text}"),
        //     (Some(source), pos) => println!("{source} @ {pos:?} | {text}"),
        //     (None, Position::NONE) => println!("{text}"),
        //     (None, pos) => println!("{pos:?} | {text}"),
        // });
    }

    fn register_functions() {
        // fn add_len(x: i64, s: ImmutableString) -> i64 {
        //     x + s.len()
        // }
        //engine.register_fn("add", add_len);

        // engine.register_fn("foo", move |x: i64, y: bool| {
        //     embedded_obj.borrow().do_foo(x, y);
        // });

        //     engine
        // // Register a series of logging functions
        // .register_fn("log_trace", move |message: Dynamic| {
        //     tracing::trace!(%message, target = %trace_main);
        // })
        // .register_fn("log_debug", move |message: Dynamic| {
        //     tracing::debug!(%message, target = %debug_main);
        // })
        // .register_fn("log_info", move |message: Dynamic| {
        //     tracing::info!(%message, target = %info_main);
        // })
        // .register_fn("log_warn", move |message: Dynamic| {
        //     tracing::warn!(%message, target = %warn_main);
        // })
        // .register_fn("log_error", move |message: Dynamic| {
        //     tracing::error!(%message, target = %error_main);
        // });
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
    ) -> Result<(), Box<EvalAltResult>> {
        if self.ast_has_function(hook_name) {
            let _ = self
                .engine
                .call_fn::<Dynamic>(&mut self.scope, &self.ast, hook_name, args)?;
        }

        Ok(())
    }

    pub fn ast_has_function(&self, name: &str) -> bool {
        self.ast.iter_functions().any(|fn_def| fn_def.name == name)
    }
}

// For creating custom types that can be used in Rhai
// #[derive(CustomType)]

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
