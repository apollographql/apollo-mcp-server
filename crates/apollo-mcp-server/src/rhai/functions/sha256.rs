use rhai::plugin::*;
use rhai::{Engine, Module};
use rhai::{export_module, exported_module};

pub(crate) struct RhaiSha256 {}

impl RhaiSha256 {
    pub(crate) fn register(engine: &mut Engine) {
        engine.register_static_module("Sha256", exported_module!(rhai_sha256_module).into());
    }
}

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
