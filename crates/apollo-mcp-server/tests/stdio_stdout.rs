//! The stdio transport reserves stdout for MCP JSON-RPC messages, so every
//! line the server writes to stdout must be a JSON-RPC message. Log output
//! belongs on stderr.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Kills the server process when the test finishes, including on panic.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn stdio_transport_writes_only_jsonrpc_messages_to_stdout() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/introspection/tools/testdata/schema.graphql");
    // A JSON string is a valid YAML scalar, which quotes the path safely.
    let schema_path =
        serde_json::to_string(&schema_path.display().to_string()).expect("serialize schema path");

    // No `logging` section: logs use the default INFO level and no file path.
    // The endpoint is never contacted because no operations are executed.
    let config = format!(
        "endpoint: http://127.0.0.1:1/graphql\n\
         transport:\n  type: stdio\n\
         schema:\n  source: local\n  path: {schema_path}\n\
         introspection:\n  introspect:\n    enabled: true\n"
    );
    let config_path = dir.path().join("config.yaml");
    std::fs::write(&config_path, config).expect("write config");

    let child = Command::new(env!("CARGO_BIN_EXE_apollo-mcp-server"))
        .arg(&config_path)
        .current_dir(dir.path())
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn apollo-mcp-server");
    let mut child = KillOnDrop(child);

    let stdout = child.0.stdout.take().expect("child stdout");
    let (line_tx, line_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut stderr = child.0.stderr.take().expect("child stderr");
    let stderr_reader = std::thread::spawn(move || {
        let mut output = String::new();
        let _ = stderr.read_to_string(&mut output);
        output
    });

    let mut stdin = child.0.stdin.take().expect("child stdin");
    let initialize = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "stdio-stdout-test", "version": "1.0.0" }
        }
    });
    writeln!(stdin, "{initialize}").expect("write initialize request");
    stdin.flush().expect("flush initialize request");

    let deadline = Instant::now() + RESPONSE_TIMEOUT;
    let mut stdout_lines = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let line = line_rx.recv_timeout(remaining).unwrap_or_else(|err| {
            panic!("no initialize response on stdout ({err}); stdout so far: {stdout_lines:#?}")
        });

        let message: serde_json::Value = serde_json::from_str(&line)
            .unwrap_or_else(|err| panic!("stdout line is not JSON ({err}): {line:?}"));
        assert_eq!(
            message.get("jsonrpc").and_then(|v| v.as_str()),
            Some("2.0"),
            "stdout line is not a JSON-RPC 2.0 message: {line:?}"
        );
        stdout_lines.push(line);

        if message.get("id") == Some(&serde_json::json!(1)) {
            assert!(
                message.get("result").is_some(),
                "initialize failed: {message}"
            );
            break;
        }
    }

    drop(stdin);
    drop(child);
    let stderr_output = stderr_reader.join().expect("join stderr reader");
    assert!(
        stderr_output.contains("Starting MCP server in stdio mode"),
        "expected startup logs on stderr, got: {stderr_output:?}"
    );
}
