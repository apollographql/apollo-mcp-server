use rhai::plugin::*;
use rhai::{Engine, Module};
use rhai::{export_module, exported_module};

pub struct RhaiSha256 {}

impl RhaiSha256 {
    pub fn register(engine: &mut Engine) {
        engine.register_static_module("Sha256", exported_module!(rhai_sha256_module).into());
    }
}

// Rhai's #[export_module] macro generates code that uses unwrap internally
#[allow(clippy::unwrap_used)]
#[export_module]
mod rhai_sha256_module {
    use rhai::Dynamic;
    use rhai::ImmutableString;
    use rhai::plugin::TypeId;
    use sha2::Digest;

    #[rhai_fn(pure)]
    pub(crate) fn digest(input: &mut ImmutableString) -> String {
        let hash = sha2::Sha256::digest(input.as_bytes());
        hex::encode(hash)
    }
}

#[cfg(test)]
mod tests {
    use rhai::{Engine, EvalAltResult, FuncArgs, Scope};

    use crate::functions::RhaiSha256;

    fn run_rhai_script<T: Clone + Send + Sync + 'static>(
        script: &str,
        args: impl FuncArgs,
    ) -> Result<T, Box<EvalAltResult>> {
        let mut engine = Engine::new();
        let mut scope = Scope::new();

        RhaiSha256::register(&mut engine);

        let ast = engine.compile(script).expect("Script should have compiled");
        engine
            .run_ast_with_scope(&mut scope, &ast)
            .expect("Script should be able to run with AST");

        engine.call_fn::<T>(&mut scope, &ast, "test", args)
    }

    #[test]
    fn should_return_correct_sha256_digest() {
        let result = run_rhai_script::<String>(
            r#"fn test() {
                return Sha256::digest("hello");
            }"#,
            (),
        )
        .expect("Should not error");

        assert_eq!(
            result,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn should_return_correct_digest_for_empty_string() {
        let result = run_rhai_script::<String>(
            r#"fn test() {
                return Sha256::digest("");
            }"#,
            (),
        )
        .expect("Should not error");

        assert_eq!(
            result,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn should_return_different_digests_for_different_inputs() {
        let result_a = run_rhai_script::<String>(
            r#"fn test() {
                return Sha256::digest("a");
            }"#,
            (),
        )
        .expect("Should not error");

        let result_b = run_rhai_script::<String>(
            r#"fn test() {
                return Sha256::digest("b");
            }"#,
            (),
        )
        .expect("Should not error");

        assert_ne!(result_a, result_b);
    }
}
