use std::sync::Arc;

use crate::rhai::types::{HttpResponse, Promise, PromiseState};
use parking_lot::Mutex;
use rhai::plugin::*;
use rhai::{Engine, Module, Shared};
use rhai::{export_module, exported_module};
use tokio::sync::oneshot;

pub(crate) struct RhaiHttp {}

impl RhaiHttp {
    pub(crate) fn register(engine: &mut Engine) {
        let mut module = Module::new();

        module.set_native_fn("get", move || {
            let (tx, rx) = oneshot::channel();
            // TODO: This needs to come from the args
            let url = "https://randomuser.me/api/".to_string();

            tokio::spawn(async move {
                let result = reqwest::get(&url).await;
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
                id: "1".to_string(),
                state: PromiseState::Pending,
                resolved_value: None,
                receiver: Arc::new(Mutex::new(Some(rx))),
            })
        });

        let module: Shared<Module> = module.into();

        engine
            // Register the module as a fixed sub-module
            .register_static_module("Http", module);
    }
}

// #[export_module]
// mod rhai_http_module {
//     use crate::rhai::types::{Promise, PromiseState};

//     pub(crate) fn get() -> Promise {
//         Promise {
//             id: "1".to_string(),
//             state: PromiseState::Pending,
//         }
//     }
// }
