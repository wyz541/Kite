use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

struct Fixture {
    child: Child,
    stdin: ChildStdin,
    frames: Receiver<String>,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    fn start(scenario: &str, environment: &[(&str, String)]) -> Self {
        let fixture_path = env!("CARGO_BIN_EXE_kite-agent-fixture");
        eprintln!("starting fixture {fixture_path} scenario={scenario}");
        let mut command = Command::new(fixture_path);
        command
            .arg("--scenario")
            .arg(scenario)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in environment {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn fixture");
        eprintln!("fixture pid={}", child.id());
        let stdin = child.stdin.take().expect("fixture stdin");
        let stdout = child.stdout.take().expect("fixture stdout");
        let stderr = child.stderr.take().expect("fixture stderr");
        let (frame_sender, frames) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if frame_sender.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        let target = stderr_lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if let Ok(mut target) = target.lock() {
                    target.push(line);
                }
            }
        });
        Self {
            child,
            stdin,
            frames,
            stderr: stderr_lines,
        }
    }

    fn write(&mut self, value: Value) {
        serde_json::to_writer(&mut self.stdin, &value).expect("encode frame");
        self.stdin.write_all(b"\n").expect("frame newline");
        self.stdin.flush().expect("flush frame");
    }

    fn response(&mut self, id: u64) -> Value {
        self.read_until(Duration::from_secs(15), |value| {
            value.get("id").and_then(Value::as_u64) == Some(id)
        })
    }

    fn read_until(
        &mut self,
        timeout: Duration,
        mut predicate: impl FnMut(&Value) -> bool,
    ) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for fixture frame");
            match self.frames.recv_timeout(remaining) {
                Ok(line) => {
                    let value = serde_json::from_str::<Value>(&line).expect("valid JSON frame");
                    if predicate(&value) {
                        return value;
                    }
                }
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => {
                    let status = self.child.try_wait().ok().flatten();
                    let stderr = self
                        .stderr
                        .lock()
                        .map(|lines| lines.join("\n"))
                        .unwrap_or_default();
                    panic!(
                        "timed out waiting for fixture frame; status={status:?}; stderr={stderr}"
                    )
                }
            }
        }
    }

    fn initialize(&mut self) {
        self.write(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientCapabilities": {},
                "clientInfo": { "name": "fixture-test", "version": "1" }
            }
        }));
        let response = self.response(1);
        assert!(response.get("error").is_none());
        assert_eq!(response["result"]["protocolVersion"], 1);

        self.write(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/new",
            "params": { "cwd": env!("CARGO_MANIFEST_DIR"), "mcpServers": [] }
        }));
        let response = self.response(2);
        assert!(response.get("error").is_none());
        assert_eq!(response["result"]["sessionId"], "fixture-session");
    }

    fn prompt(&mut self, id: u64) {
        self.write(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/prompt",
            "params": {
                "sessionId": "fixture-session",
                "prompt": [{ "type": "text", "text": "fixture prompt" }]
            }
        }));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn streams_markdown_chunks_without_losing_content() {
    let mut fixture = Fixture::start("markdown", &[]);
    fixture.initialize();
    fixture.prompt(3);
    let mut markdown = String::new();
    let response = fixture.read_until(Duration::from_secs(15), |value| {
        if value.get("method").and_then(Value::as_str) == Some("session/update")
            && let Some(text) = value
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
        {
            markdown.push_str(text);
        }
        value.get("id").and_then(Value::as_u64) == Some(3)
    });
    assert_eq!(response["result"]["stopReason"], "end_turn");
    assert_eq!(
        markdown,
        "## Fixture answer\n\n```rust\nprintln!(\"streaming markdown\");\n```\n"
    );
}

#[test]
fn permission_request_waits_for_a_structured_response() {
    let mut fixture = Fixture::start("permission", &[]);
    fixture.initialize();
    fixture.prompt(3);
    let permission = fixture.read_until(Duration::from_secs(15), |value| {
        value.get("method").and_then(Value::as_str) == Some("session/request_permission")
    });
    assert_eq!(permission["id"], 900);
    assert_eq!(
        permission["params"]["toolCall"]["toolCallId"],
        "fixture-tool-1"
    );
    assert_eq!(permission["params"]["options"].as_array().unwrap().len(), 2);

    fixture.write(json!({
        "jsonrpc": "2.0",
        "id": 900,
        "result": {
            "outcome": { "outcome": "selected", "optionId": "allow" }
        }
    }));
    let mut text = String::new();
    let response = fixture.read_until(Duration::from_secs(15), |value| {
        if value.get("method").and_then(Value::as_str) == Some("session/update")
            && let Some(chunk) = value
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
        {
            text.push_str(chunk);
        }
        value.get("id").and_then(Value::as_u64) == Some(3)
    });
    assert!(text.contains("permission selected: allow"));
    assert_eq!(response["result"]["stopReason"], "end_turn");
}

#[test]
fn cancel_returns_the_acp_cancelled_stop_reason() {
    let mut fixture = Fixture::start("cancel", &[]);
    fixture.initialize();
    fixture.prompt(3);
    fixture.read_until(Duration::from_secs(15), |value| {
        value
            .pointer("/params/update/content/text")
            .and_then(Value::as_str)
            .is_some_and(|text| text.contains("waiting for cancel"))
    });
    fixture.write(json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": { "sessionId": "fixture-session" }
    }));
    let response = fixture.response(3);
    assert_eq!(response["result"]["stopReason"], "cancelled");
}

#[test]
fn transient_failure_can_be_retried_in_a_new_fixture_process() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let marker = std::env::temp_dir().join(format!(
        "kite-agent-fixture-retry-{}-{unique}",
        std::process::id()
    ));
    let environment = [(
        "KITE_FIXTURE_STATE_FILE",
        marker.to_string_lossy().into_owned(),
    )];

    let mut first = Fixture::start("flaky", &environment);
    first.initialize();
    first.prompt(3);
    let failed = first.response(3);
    assert_eq!(failed["error"]["code"], -32603);
    drop(first);

    let mut second = Fixture::start("flaky", &environment);
    second.initialize();
    second.prompt(3);
    let mut text = String::new();
    let response = second.read_until(Duration::from_secs(15), |value| {
        if value.get("method").and_then(Value::as_str) == Some("session/update")
            && let Some(chunk) = value
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
        {
            text.push_str(chunk);
        }
        value.get("id").and_then(Value::as_u64) == Some(3)
    });
    assert!(text.contains("retry recovered"));
    assert_eq!(response["result"]["stopReason"], "end_turn");
    let _ = std::fs::remove_file(marker);
}

#[test]
fn long_output_throughput_is_measured_for_the_performance_baseline() {
    let mut fixture = Fixture::start("long", &[]);
    fixture.initialize();
    let started = Instant::now();
    fixture.prompt(3);
    let mut bytes = 0usize;
    let mut chunks = 0usize;
    let response = fixture.read_until(Duration::from_secs(10), |value| {
        if value.get("method").and_then(Value::as_str) == Some("session/update")
            && let Some(text) = value
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
        {
            bytes += text.len();
            chunks += 1;
        }
        value.get("id").and_then(Value::as_u64) == Some(3)
    });
    let elapsed = started.elapsed();
    assert_eq!(response["result"]["stopReason"], "end_turn");
    assert_eq!(chunks, 256);
    assert!(bytes >= 256 * 1024);
    assert!(elapsed < Duration::from_secs(10));
    eprintln!(
        "fixture long-output: {} bytes in {} ms ({:.1} MiB/s)",
        bytes,
        elapsed.as_millis(),
        bytes as f64 / 1024.0 / 1024.0 / elapsed.as_secs_f64().max(0.001)
    );
}

#[test]
fn fixture_emits_redaction_inputs_but_never_asserts_them_as_answers() {
    let fixture = Fixture::start(
        "normal",
        &[("FIXTURE_TOKEN", "unit-test-secret".to_string())],
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if fixture
            .stderr
            .lock()
            .is_ok_and(|lines| lines.iter().any(|line| line.contains("unit-test-secret")))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("fixture did not emit its diagnostic redaction input");
}
