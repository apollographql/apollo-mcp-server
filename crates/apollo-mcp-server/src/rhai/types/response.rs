use rhai::{CustomType, Dynamic, Engine, EvalAltResult, TypeBuilder};

#[derive(Clone, Debug, CustomType)]
pub(crate) struct HttpResponse {
    status: i64,
    text_body: String,
}

impl HttpResponse {
    pub(crate) fn new(status: i64, text_body: String) -> Self {
        Self { status, text_body }
    }

    pub(crate) fn register(engine: &mut Engine) {
        engine
            .register_type::<HttpResponse>()
            .register_get("status", HttpResponse::get_status)
            .register_fn("text", HttpResponse::text)
            .register_fn("json", HttpResponse::json);
    }

    fn get_status(&mut self) -> i64 {
        self.status
    }

    fn text(&mut self) -> String {
        self.text_body.clone()
    }

    fn json(&mut self) -> Result<Dynamic, Box<EvalAltResult>> {
        serde_json::from_str(&self.text_body).map_err(|e| e.to_string().into())
    }
}
