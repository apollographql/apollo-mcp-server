use std::sync::Arc;

use parking_lot::Mutex;
use rhai::plugin::*;
use rhai::{Engine, Module};

pub struct RhaiHttp {}

impl RhaiHttp {
    pub fn register(engine: &mut Engine) {
        // Incomplete (needs POST and to implement options), mark as experimental for now
        if cfg!(feature = "experimental_rhai") {
            engine.register_static_module("Http", exported_module!(http_module).into());
        }
    }
}

#[derive(Clone, Debug)]
pub struct HttpOptions {}

#[export_module]
mod http_module {
    use crate::types::{HttpResponse, Promise, PromiseState};
    use tokio::sync::oneshot;

    #[rhai_fn(name = "get", return_raw)]
    pub(crate) fn get_no_options(url: ImmutableString) -> Result<Promise, Box<EvalAltResult>> {
        get(url, HttpOptions {})
    }

    #[rhai_fn(name = "get", return_raw)]
    pub(crate) fn get(
        url: ImmutableString,
        // TODO: Implement options
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

#[cfg(test)]
mod tests {
    use rhai::{Engine, EvalAltResult, FuncArgs, Scope};

    use crate::functions::RhaiHttp;
    use crate::types::{HttpResponse, Promise};

    fn run_rhai_script<T: Clone + Send + Sync + 'static>(
        script: &str,
        args: impl FuncArgs,
    ) -> Result<T, Box<EvalAltResult>> {
        let mut engine = Engine::new();
        let mut scope = Scope::new();

        RhaiHttp::register(&mut engine);
        Promise::register(&mut engine);
        HttpResponse::register(&mut engine);

        let ast = engine.compile(script).expect("Script should have compiled");
        engine
            .run_ast_with_scope(&mut scope, &ast)
            .expect("Script should be able to run with AST");

        engine.call_fn::<T>(&mut scope, &ast, "test", args)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn should_return_200_status_on_successful_get() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/hello")
            .with_status(200)
            .with_body("OK")
            .create_async()
            .await;

        let url = format!("{}/hello", server.url());
        let script = format!(
            r#"fn test() {{
                let response = Http::get("{url}").wait();
                return response.status;
            }}"#
        );

        let result = run_rhai_script::<i64>(&script, ()).expect("Should not error");

        assert_eq!(result, 200);
        mock.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn should_return_response_body_as_text() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/text")
            .with_status(200)
            .with_body("hello world")
            .create_async()
            .await;

        let url = format!("{}/text", server.url());
        let script = format!(
            r#"fn test() {{
                let response = Http::get("{url}").wait();
                return response.text();
            }}"#
        );

        let result = run_rhai_script::<String>(&script, ()).expect("Should not error");

        assert_eq!(result, "hello world");
        mock.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn should_return_response_body_as_json() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"name":"apollo","version":1}"#)
            .create_async()
            .await;

        let url = format!("{}/json", server.url());
        let script = format!(
            r#"fn test() {{
                let response = Http::get("{url}").wait();
                let data = response.json();
                return data.name;
            }}"#
        );

        let result = run_rhai_script::<String>(&script, ()).expect("Should not error");

        assert_eq!(result, "apollo");
        mock.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn should_return_non_200_status_code() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/not-found")
            .with_status(404)
            .with_body("not found")
            .create_async()
            .await;

        let url = format!("{}/not-found", server.url());
        let script = format!(
            r#"fn test() {{
                let response = Http::get("{url}").wait();
                return response.status;
            }}"#
        );

        let result = run_rhai_script::<i64>(&script, ()).expect("Should not error");

        assert_eq!(result, 404);
        mock.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn should_return_error_for_invalid_json() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/bad-json")
            .with_status(200)
            .with_body("not json")
            .create_async()
            .await;

        let url = format!("{}/bad-json", server.url());
        let script = format!(
            r#"fn test() {{
                let response = Http::get("{url}").wait();
                return response.json();
            }}"#
        );

        let result = run_rhai_script::<rhai::Dynamic>(&script, ());

        assert!(result.is_err());
        mock.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn should_return_error_for_connection_failure() {
        let script = r#"fn test() {
            let response = Http::get("http://127.0.0.1:1").wait();
            return response.status;
        }"#;

        let result = run_rhai_script::<i64>(script, ());

        assert!(result.is_err());
    }
}
