use std::sync::Arc;

use parking_lot::Mutex;
use rhai::plugin::*;
use rhai::{Engine, Module};

pub(crate) struct RhaiHttp {}

impl RhaiHttp {
    pub(crate) fn register(engine: &mut Engine) {
        engine.register_static_module("Http", exported_module!(http_module).into());
    }
}

#[derive(Clone, Debug)]
pub(crate) struct HttpOptions {}

#[export_module]
mod http_module {
    use crate::rhai::types::{HttpResponse, Promise, PromiseState};
    use tokio::sync::oneshot;

    #[rhai_fn(name = "get", return_raw)]
    pub(crate) fn get_no_options(url: ImmutableString) -> Result<Promise, Box<EvalAltResult>> {
        get(url, HttpOptions {})
    }

    #[rhai_fn(name = "get", return_raw)]
    pub(crate) fn get(
        url: ImmutableString,
        _options: HttpOptions,
    ) -> Result<Promise, Box<EvalAltResult>> {
        let (tx, rx) = oneshot::channel();

        tokio::spawn(async move {
            let result = reqwest::get(url.to_string()).await;
            let value = match result {
                Ok(resp) => {
                    let status = resp.status().as_u16() as i64;
                    match resp.text().await {
                        Ok(body) => Ok(Dynamic::from(HttpResponse::new(status, body))),
                        Err(e) => Err(e.to_string()),
                    }
                }
                Err(e) => Err(e.to_string()),
            };

            let _ = tx.send(value);
        });

        Ok(Promise {
            state: PromiseState::Pending,
            resolved_value: None,
            receiver: Arc::new(Mutex::new(Some(rx))),
        })
    }
}
