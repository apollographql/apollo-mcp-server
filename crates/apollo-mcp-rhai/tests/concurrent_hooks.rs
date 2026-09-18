//! A hook that blocks on an HTTP call used to be able to wedge the whole tokio runtime: every
//! hook call took an exclusive lock on the single shared engine and held it for the duration of
//! the script, so `worker_threads + 1` concurrent tool calls left no thread free to finish the
//! in-flight request the lock holder was waiting on.

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
const CONCURRENT_CALLS: usize = 4;
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

fn write_hook_script(script_dir: &Path, port: u16) -> io::Result<()> {
    std::fs::create_dir_all(script_dir)?;
    std::fs::write(
        script_dir.join("main.rhai"),
        format!(
            r#"fn on_execute_graphql_operation(ctx) {{
                let response = Http::get("http://127.0.0.1:{port}/", #{{ timeout: 5 }}).wait();
                ctx.headers["x-status"] = response.status.to_string();
            }}"#
        ),
    )
}

#[test]
fn concurrent_hooks_blocking_on_http_should_not_stall_the_runtime() {
    let port = slow_http_server(RESPONSE_DELAY).expect("Should start the HTTP server");
    let dir = tempfile::tempdir().expect("Should create temp dir");
    let script_dir = dir.path().join("rhai");
    write_hook_script(&script_dir, port).expect("Should write the hook script");

    let engine = SharedRhaiEngine::load(&script_dir).expect("Should load scripts");

    // Drive the runtime from its own thread. A wedged runtime cannot run its own timers, so the
    // deadline has to be enforced from outside it.
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKER_THREADS)
            .enable_all()
            .build()
            .expect("Should build a runtime");

        let statuses = runtime.block_on(async move {
            let calls = (0..CONCURRENT_CALLS)
                .map(|_| {
                    let engine = engine.clone();

                    tokio::spawn(async move {
                        let endpoint =
                            Url::parse("https://example.com/graphql").expect("Valid URL");

                        checkpoints::on_execute_graphql_operation(
                            &engine,
                            &endpoint,
                            &HeaderMap::new(),
                            None,
                            "tool",
                            String::new,
                        )
                    })
                })
                .collect::<Vec<_>>();

            let mut statuses = Vec::with_capacity(CONCURRENT_CALLS);
            for call in calls {
                let (_, headers) = call
                    .await
                    .expect("Task should not panic")
                    .expect("Hook should succeed");

                statuses.push(
                    headers
                        .get("x-status")
                        .and_then(|status| status.to_str().ok())
                        .unwrap_or_default()
                        .to_string(),
                );
            }
            statuses
        });

        let _ = tx.send(statuses);
    });

    let statuses = rx
        .recv_timeout(RESPONSE_DELAY * 20)
        .expect("Concurrent hook calls stalled the runtime");
    let elapsed = started.elapsed();

    assert_eq!(statuses, vec!["200".to_string(); CONCURRENT_CALLS]);
    assert!(
        elapsed < RESPONSE_DELAY * CONCURRENT_CALLS as u32,
        "hook calls ran serially ({elapsed:?}), so they still contend for the engine"
    );
}
