use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use reqwest::Method;
use rhai::plugin::*;
use rhai::{Engine, EvalAltResult, Map, Module};

use crate::types::{HttpResponse, Promise, PromiseState};

pub struct RhaiHttp {}

impl RhaiHttp {
    pub fn register(engine: &mut Engine) {
        engine.register_static_module("Http", exported_module!(http_module).into());
    }
}

struct HttpOptions {
    headers: Vec<(String, String)>,
    body: Option<String>,
    timeout_secs: Option<u64>,
}

impl HttpOptions {
    fn from_map(map: Map) -> Result<Self, Box<EvalAltResult>> {
        let mut headers = Vec::new();
        let mut body = None;
        let mut timeout_secs = None;

        if let Some(h) = map.get("headers") {
            let header_map = h
                .clone()
                .try_cast::<Map>()
                .ok_or_else(|| -> Box<EvalAltResult> { "options.headers must be a map".into() })?;
            for (k, v) in header_map {
                headers.push((k.to_string(), v.to_string()));
            }
        }

        if let Some(b) = map.get("body") {
            body = Some(b.to_string());
        }

        if let Some(t) = map.get("timeout") {
            let secs = t
                .clone()
                .try_cast::<i64>()
                .ok_or_else(|| -> Box<EvalAltResult> {
                    "options.timeout must be an integer (seconds)".into()
                })?;
            timeout_secs = Some(secs as u64);
        }

        Ok(Self {
            headers,
            body,
            timeout_secs,
        })
    }
}

fn spawn_request(method: Method, url: String, options: HttpOptions) -> Promise {
    let (tx, rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let result = execute_request(method, url, options).await;
        let value = result.map(Dynamic::from);
        let _ = tx.send(value);
    });

    Promise {
        state: PromiseState::Pending,
        resolved_value: None,
        receiver: Arc::new(Mutex::new(Some(rx))),
    }
}

async fn execute_request(
    method: Method,
    url: String,
    options: HttpOptions,
) -> Result<HttpResponse, String> {
    let client = reqwest::Client::new();
    let mut builder = client.request(method, &url);

    for (k, v) in &options.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    if let Some(body) = options.body {
        builder = builder.body(body);
    }
    if let Some(secs) = options.timeout_secs {
        builder = builder.timeout(Duration::from_secs(secs));
    }

    let resp = builder.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16() as i64;
    let text = resp.text().await.map_err(|e| e.to_string())?;
    Ok(HttpResponse::new(status, text))
}

// Rhai's #[export_module] macro generates code that uses unwrap internally
#[allow(clippy::unwrap_used)]
#[export_module]
mod http_module {
    use rhai::Map;

    #[rhai_fn(name = "get", return_raw)]
    pub(crate) fn get_no_options(url: ImmutableString) -> Result<Promise, Box<EvalAltResult>> {
        get_with_options(url, Map::new())
    }

    #[rhai_fn(name = "get", return_raw)]
    pub(crate) fn get_with_options(
        url: ImmutableString,
        options: Map,
    ) -> Result<Promise, Box<EvalAltResult>> {
        let options = HttpOptions::from_map(options)?;
        Ok(spawn_request(Method::GET, url.to_string(), options))
    }

    #[rhai_fn(name = "post", return_raw)]
    pub(crate) fn post_no_options(url: ImmutableString) -> Result<Promise, Box<EvalAltResult>> {
        post_with_options(url, Map::new())
    }

    #[rhai_fn(name = "post", return_raw)]
    pub(crate) fn post_with_options(
        url: ImmutableString,
        options: Map,
    ) -> Result<Promise, Box<EvalAltResult>> {
        let options = HttpOptions::from_map(options)?;
        Ok(spawn_request(Method::POST, url.to_string(), options))
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

    #[tokio::test(flavor = "multi_thread")]
    async fn should_send_custom_headers_on_get() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/headers")
            .match_header("x-my-header", "my-value")
            .with_status(200)
            .with_body("OK")
            .create_async()
            .await;

        let url = format!("{}/headers", server.url());
        let script = format!(
            r#"fn test() {{
                let response = Http::get("{url}", #{{
                    headers: #{{
                        "x-my-header": "my-value"
                    }}
                }}).wait();
                return response.status;
            }}"#
        );

        let result = run_rhai_script::<i64>(&script, ()).expect("Should not error");

        assert_eq!(result, 200);
        mock.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn should_return_error_for_invalid_timeout_type() {
        let script = r#"fn test() {
            let response = Http::get("http://127.0.0.1:1", #{
                timeout: "not-a-number"
            }).wait();
            return response.status;
        }"#;

        let result = run_rhai_script::<i64>(script, ());

        assert!(result.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn should_succeed_with_timeout_option_set() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/timeout-ok")
            .with_status(200)
            .with_body("OK")
            .create_async()
            .await;

        let url = format!("{}/timeout-ok", server.url());
        let script = format!(
            r#"fn test() {{
                let response = Http::get("{url}", #{{
                    timeout: 30
                }}).wait();
                return response.status;
            }}"#
        );

        let result = run_rhai_script::<i64>(&script, ()).expect("Should not error");

        assert_eq!(result, 200);
        mock.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn should_post_with_body() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/data")
            .match_body("hello")
            .with_status(201)
            .with_body("created")
            .create_async()
            .await;

        let url = format!("{}/data", server.url());
        let script = format!(
            r#"fn test() {{
                let response = Http::post("{url}", #{{
                    body: "hello"
                }}).wait();
                return response.status;
            }}"#
        );

        let result = run_rhai_script::<i64>(&script, ()).expect("Should not error");

        assert_eq!(result, 201);
        mock.assert_async().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn should_post_with_headers_and_body() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/json")
            .match_header("content-type", "application/json")
            .match_body(r#"{"key":"value"}"#)
            .with_status(200)
            .with_body(r#"{"ok":true}"#)
            .create_async()
            .await;

        let url = format!("{}/json", server.url());
        let script = format!(
            r#"fn test() {{
                let response = Http::post("{url}", #{{
                    headers: #{{
                        "content-type": "application/json"
                    }},
                    body: `{{"key":"value"}}`
                }}).wait();
                return response.status;
            }}"#
        );

        let result = run_rhai_script::<i64>(&script, ()).expect("Should not error");

        assert_eq!(result, 200);
        mock.assert_async().await;
    }
}
