//! Hooks run concurrently, so these tests drive `worker_threads + 1` tool calls through a hook
//! that blocks on a slow HTTP call.
//!
//! A hook that blocks on an HTTP call used to be able to wedge the whole tokio runtime: every
//! hook call took an exclusive lock on the single shared engine and held it for the duration of
//! the script, so `worker_threads + 1` concurrent tool calls left no thread free to finish the
//! in-flight request the lock holder was waiting on. With the lock gone, the calls also overlap,
//! so state one call writes must not be visible to another.

use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use apollo_mcp_rhai::{SharedRhaiEngine, checkpoints};
use http::HeaderMap;
use url::Url;

const WORKER_THREADS: usize = 2;
/// One more than the worker count is all the old deadlock needed.
const CONCURRENT_CALLS: usize = WORKER_THREADS + 1;
const RESPONSE_DELAY: Duration = Duration::from_millis(500);

/// A slow HTTP server served from plain OS threads, so the runtime under test cannot starve it.
fn slow_http_server(delay: Duration) -> io::Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };

            thread::spawn(move || {
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                thread::sleep(delay);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
            });
        }
    });

    Ok(port)
}

fn write_script(script_dir: &Path, script: &str) -> io::Result<()> {
    std::fs::create_dir_all(script_dir)?;
    std::fs::write(script_dir.join("main.rhai"), script)
}

fn tool_name(call: usize) -> String {
    format!("tool-{call}")
}

/// Runs `CONCURRENT_CALLS` hook calls at once, call `i` as tool `tool-{i}`, and returns the
/// headers each call produced, in call order, with the wall-clock time the batch took.
///
/// The runtime is driven from its own thread. A wedged runtime cannot run its own timers, so the
/// deadline has to be enforced from outside it.
#[expect(
    clippy::expect_used,
    reason = "a failed setup step should fail the test"
)]
fn run_concurrently(engine: SharedRhaiEngine) -> (Vec<HeaderMap>, Duration) {
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();

    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .enable_all()
            .build()
            .expect("Should build a runtime");

        let headers = runtime.block_on(async move {
            let calls = (0..CONCURRENT_CALLS)
                .map(|call| {
                    let engine = engine.clone();

                    tokio::spawn(async move {
                        let endpoint =
                            Url::parse("https://example.com/graphql").expect("Valid URL");

                        checkpoints::on_execute_graphql_operation(
                            &engine,
                            &endpoint,
                            &HeaderMap::new(),
                            None,
                            &tool_name(call),
                            String::new,
                        )
                    })
                })
                .collect::<Vec<_>>();

            let mut headers = Vec::with_capacity(CONCURRENT_CALLS);
            for call in calls {
                let (_, call_headers) = call
                    .await
                    .expect("Task should not panic")
                    .expect("Hook should succeed");
                headers.push(call_headers);
            }
            headers
        });

        let _ = tx.send(headers);
    });

    let headers = rx
        .recv_timeout(RESPONSE_DELAY * 20)
        .expect("Concurrent hook calls stalled the runtime");

    (headers, started.elapsed())
}

/// The header's text, or `None` when the hook never set it, so a missing header can't pass for
/// an empty one.
fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

#[expect(
    clippy::expect_used,
    reason = "a failed setup step should fail the test"
)]
fn load_hook(body: &str) -> (SharedRhaiEngine, tempfile::TempDir) {
    let port = slow_http_server(RESPONSE_DELAY).expect("Should start the HTTP server");
    let dir = tempfile::tempdir().expect("Should create temp dir");
    let script_dir = dir.path().join("rhai");
    write_script(&script_dir, &body.replace("{port}", &port.to_string()))
        .expect("Should write the hook script");

    let engine = SharedRhaiEngine::load(&script_dir).expect("Should load scripts");
    (engine, dir)
}

/// Each call records the value it wrote, waits on HTTP while the others write theirs, then reads
/// the value back, once directly and once through a closure that captured it at load time.
const CAPTURED_STATE_HOOK: &str = r#"
    let state = #{ caller: "" };
    let read_state = || state.caller;

    fn on_execute_graphql_operation(ctx) {
        state.caller = ctx.tool_name;
        Http::get("http://127.0.0.1:{port}/", #{ timeout: 5 }).wait();
        ctx.headers["x-direct"] = state.caller;
        ctx.headers["x-closure"] = read_state.call();
    }
"#;

#[test]
fn concurrent_hooks_blocking_on_http_should_not_stall_the_runtime() {
    let (engine, _dir) = load_hook(
        r#"
        fn on_execute_graphql_operation(ctx) {
            let response = Http::get("http://127.0.0.1:{port}/", #{ timeout: 5 }).wait();
            ctx.headers["x-status"] = response.status.to_string();
        }
        "#,
    );

    let (headers, elapsed) = run_concurrently(engine);

    let statuses: Vec<_> = headers.iter().map(|h| header(h, "x-status")).collect();
    assert_eq!(statuses, vec![Some("200"); CONCURRENT_CALLS]);
    assert!(
        elapsed < RESPONSE_DELAY * CONCURRENT_CALLS as u32,
        "hook calls ran serially ({elapsed:?}), so they still contend for the engine"
    );
}

#[test]
fn concurrent_hooks_should_each_read_back_their_own_write_to_captured_state() {
    let (engine, _dir) = load_hook(CAPTURED_STATE_HOOK);

    let (headers, _) = run_concurrently(engine);

    let seen: Vec<_> = headers.iter().map(|h| header(h, "x-direct")).collect();
    let names: Vec<_> = (0..CONCURRENT_CALLS).map(tool_name).collect();
    let expected: Vec<_> = names.iter().map(|name| Some(name.as_str())).collect();
    assert_eq!(seen, expected, "a hook read another call's write");
}

#[test]
fn concurrent_hooks_should_read_the_load_time_value_through_a_closure() {
    let (engine, _dir) = load_hook(CAPTURED_STATE_HOOK);

    let (headers, _) = run_concurrently(engine);

    let seen: Vec<_> = headers.iter().map(|h| header(h, "x-closure")).collect();
    assert_eq!(
        seen,
        vec![Some(""); CONCURRENT_CALLS],
        "a closure saw a hook write"
    );
}
