use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

struct Batch {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Batch {
    fn start() -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "kite-control-plane-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("create temp directory");
        let config = directory.join("agent.json");
        let profile = json!({
            "schema_version": 1,
            "profiles": [{
                "id": "fixture",
                "name": "Fixture",
                "transport": "acp_stdio",
                "executable": env!("CARGO_BIN_EXE_kite-agent-fixture"),
                "arguments": ["--scenario", "markdown"],
                "environment": [],
                "working_directory": "selected_item",
                "permission": "read_only",
                "timeout_seconds": 10,
                "output_limit_bytes": 8388608,
                "capabilities": {
                    "read_selection": true,
                    "read_workspace": false,
                    "write_files": false,
                    "execute_commands": false,
                    "network": false,
                    "permission_requests": true
                }
            }],
            "default_profile_id": "fixture"
        });
        fs::write(
            &config,
            serde_json::to_vec_pretty(&profile).expect("encode profile"),
        )
        .expect("write profile");
        let mut child = Command::new(env!("CARGO_BIN_EXE_kite-cli"))
            .arg("batch")
            .env("KITE_AGENT_PROFILE_PATH", &config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn kite-cli batch");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        Self {
            stdin,
            stdout: BufReader::new(stdout),
            child,
        }
    }

    fn request(&mut self, tool: &str, arguments: Value) -> Value {
        writeln!(
            self.stdin,
            "{}",
            json!({ "tool": tool, "arguments": arguments })
        )
        .expect("write request");
        self.stdin.flush().expect("flush request");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        serde_json::from_str(&line).expect("parse response")
    }
}

impl Drop for Batch {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn cli_batch_keeps_an_agent_session_alive_between_commands() {
    let mut batch = Batch::start();
    let started = batch.request(
        "agent_session_start",
        json!({ "profile_id": "fixture", "working_directory": std::env::temp_dir() }),
    );
    assert_eq!(started["ok"], true, "start response: {started}");
    let session_id = started["result"]["session_id"]
        .as_str()
        .expect("session id")
        .to_string();

    let prompted = batch.request(
        "agent_session_prompt",
        json!({ "session_id": session_id, "prompt": "fixture prompt" }),
    );
    assert_eq!(prompted["ok"], true, "prompt response: {prompted}");

    let waited = batch.request(
        "agent_session_wait",
        json!({ "session_id": session_id, "timeout_ms": 10_000 }),
    );
    assert_eq!(waited["ok"], true, "wait response: {waited}");
    assert_eq!(waited["result"]["snapshot"]["state"], "completed");
    let events = waited["result"]["events"].as_array().expect("events");
    assert!(events.iter().any(|event| {
        event["text"]
            .as_str()
            .is_some_and(|text| text.contains("Fixture answer"))
    }));

    let status = batch.request("agent_session_status", json!({ "session_id": session_id }));
    assert_eq!(status["ok"], true);
    assert_eq!(status["result"]["profile_id"], "fixture");

    let closed = batch.request("agent_session_close", json!({ "session_id": session_id }));
    assert_eq!(closed["ok"], true, "close response: {closed}");
    assert_eq!(closed["result"]["closed"], true, "close response: {closed}");
}
