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
