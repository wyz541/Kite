use std::env;
use std::fs;
use std::io::{self, BufRead, Write};
use std::process::Command;
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

fn main() {
    let scenario = scenario();
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    if scenario == "startup-hang" {
        thread::sleep(Duration::from_secs(30));
        return;
    }

    if scenario == "sleeper" {
        thread::sleep(Duration::from_secs(60));
        return;
    }

    if scenario != "auth" && scenario != "idle" {
        if let Some(secret) = fixture_secret() {
            eprintln!(
                "fixture diagnostic credential {secret} url=https://alice:hunter2@example.test/path"
            );
        } else {
            eprintln!("fixture diagnostic ready");
        }
    }

    while let Some(Ok(line)) = lines.next() {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match method {
            "initialize" => {
                write_json(&json!({
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": {
                        "protocolVersion": if scenario == "old-version" { 0 } else { 1 },
                        "agentCapabilities": {
                            "loadSession": false,
                            "promptCapabilities": {
                                "image": false,
                                "audio": false,
                                "embeddedContext": false
                            },
                            "mcpCapabilities": {
                                "http": false,
                                "sse": false
                            },
                            "sessionCapabilities": {}
                        },
                        "agentInfo": {
                            "name": "Kite ACP Fixture",
                            "version": "1.0.0"
                        },
                        "authMethods": []
                    }
                }));
            }
            "session/new" => {
                if scenario == "auth" {
                    write_json(&json!({
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "error": {
                            "code": -32000,
                            "message": "authentication required"
                        }
                    }));
                } else {
                    write_json(&json!({
                        "jsonrpc": "2.0",
                        "id": message["id"],
                        "result": {
                            "sessionId": "fixture-session"
                        }
                    }));
                }
            }
            "session/prompt" => {
                handle_prompt(&scenario, &message, &mut lines);
            }
            "session/cancel" => {}
            _ => {}
        }
    }
}

fn handle_prompt(
    scenario: &str,
    message: &Value,
    lines: &mut impl Iterator<Item = io::Result<String>>,
) {
    let id = message["id"].clone();
    match scenario {
        "error" => {
            let message = fixture_secret()
                .map(|secret| format!("fixture failure at credential {secret} end"))
                .unwrap_or_else(|| "fixture failure".to_string());
            write_json(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32603,
                    "message": message
                }
            }));
        }
        "flaky" => {
            let marker = env::var("KITE_FIXTURE_STATE_FILE")
                .unwrap_or_else(|_| ".kite-agent-fixture-flaky".to_string());
            if fs::metadata(&marker).is_ok() {
                send_chunk("retry recovered");
                finish_prompt(id, "end_turn");
            } else {
                let _ = fs::write(marker, "failed-once");
                write_json(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32603,
                        "message": "fixture transient failure"
                    }
                }));
            }
        }
        "auth" => {
            write_json(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32000,
                    "message": "authentication required"
                }
            }));
        }
        "idle" => {
            thread::sleep(Duration::from_secs(30));
        }
        "slow-drip" => loop {
            send_chunk("tick");
            thread::sleep(Duration::from_millis(150));
        },
        "malformed" => {
            write_raw("{this is not json}\n");
            thread::sleep(Duration::from_secs(30));
        }
        "oversized" => {
            let text = "x".repeat(70_000);
            write_json(&json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "fixture-session",
                    "update": {
                        "sessionUpdate": "agent_message_chunk",
                        "content": {
                            "type": "text",
                            "text": text
                        }
                    }
                }
            }));
            thread::sleep(Duration::from_secs(30));
        }
        "slow" => {
            for chunk in ["one ", "two ", "three"] {
                send_chunk(chunk);
                thread::sleep(Duration::from_millis(80));
            }
            finish_prompt(id, "end_turn");
        }
        "markdown" => {
            send_chunk("## Fixture answer\n\n");
            send_chunk("```rust\n");
            send_chunk("println!(\"streaming markdown\");\n");
            send_chunk("```\n");
            finish_prompt(id, "end_turn");
        }
        "token-patterns" => {
            send_chunk("Authorization: Bear");
            send_chunk("er abc.def done\n");
            send_chunk("value sk");
            send_chunk("-proj-secret done\n");
            send_chunk("token=");
            send_chunk("abc123 done");
            finish_prompt(id, "end_turn");
        }
        "secret-stream" => {
            let Some(secret) = fixture_secret() else {
                write_json(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32603,
                        "message": "fixture credential is missing"
                    }
                }));
                return;
            };
            send_chunk("## Fixture answer\n\n");
            let split = secret.len().min(5);
            send_chunk(&format!("before {}", &secret[..split]));
            send_chunk(&format!("{}\n", &secret[split..]));
            finish_prompt(id, "end_turn");
        }
        "secret-error" => {
            if !send_secret_prefix_tail() {
                write_json(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32603,
                        "message": "fixture credential is missing"
                    }
                }));
                return;
            }
            let Some(secret) = fixture_secret() else {
                return;
            };
            write_json(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32603,
                    "message": format!("fixture failure at credential {secret} end")
                }
            }));
        }
        "cwd" => {
            let cwd = env::current_dir().unwrap_or_default();
            send_chunk(&cwd.to_string_lossy());
            finish_prompt(id, "end_turn");
        }
        "long" => {
            let chunk = "0123456789abcdef".repeat(64);
            for _ in 0..256 {
                send_chunk(&chunk);
            }
            finish_prompt(id, "end_turn");
        }
        "permission" | "permission-write" | "permission-command" | "permission-always"
        | "permission-repeat" => {
            handle_permission_scenario(scenario, id, lines);
        }
        "cancel" | "spawn-tree" => {
            if scenario == "spawn-tree"
                && let Ok(executable) = env::current_exe()
                && let Ok(child) = Command::new(executable)
                    .arg("--scenario")
                    .arg("sleeper")
                    .spawn()
            {
                if let Ok(path) = env::var("KITE_AGENT_FIXTURE_CHILD_PID_FILE") {
                    let _ = fs::write(path, child.id().to_string());
                }
                eprintln!("fixture-child-pid: {}", child.id());
            }
            send_chunk("waiting for cancel");
            loop {
                let Some(Ok(line)) = lines.next() else {
                    return;
                };
                let Ok(response) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if response.get("method").and_then(Value::as_str) == Some("session/cancel") {
                    finish_prompt(id, "cancelled");
                    return;
                }
            }
        }
        "cancel-tail" => {
            if !send_secret_prefix_tail() {
                write_json(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32603,
                        "message": "fixture credential is missing"
                    }
                }));
                return;
            }
            loop {
                let Some(Ok(line)) = lines.next() else {
                    return;
                };
                let Ok(response) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if response.get("method").and_then(Value::as_str) == Some("session/cancel") {
                    finish_prompt(id, "cancelled");
                    return;
                }
            }
        }
        _ => {
            send_chunk("hello");
            send_chunk(" world");
            finish_prompt(id, "end_turn");
        }
    }
}

fn fixture_secret() -> Option<String> {
    env::var("FIXTURE_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn send_secret_prefix_tail() -> bool {
    let Some(secret) = fixture_secret() else {
        return false;
    };
    send_chunk("tail ");
    let split = secret
        .char_indices()
        .nth(5)
        .map(|(index, _)| index)
        .unwrap_or(secret.len());
    send_chunk(&secret[..split]);
    true
}

fn handle_permission_scenario(
    scenario: &str,
    prompt_id: Value,
    lines: &mut impl Iterator<Item = io::Result<String>>,
) {
    send_chunk("waiting for permission");
    let request_ids: &[u64] = if scenario == "permission-repeat" {
        &[900, 901]
    } else {
        &[900]
    };

    for request_id in request_ids {
        let tool_kind = if scenario == "permission-command" {
            "execute"
        } else {
            "edit"
        };
        let title = if scenario == "permission-command" {
            "Fixture command"
        } else {
            "Fixture write"
        };
        let tool_call_id = if *request_id == 900 {
            "fixture-tool-1"
        } else {
            "fixture-tool-2"
        };
        write_json(&json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "session/request_permission",
            "params": {
                "sessionId": "fixture-session",
                "toolCall": {
                    "toolCallId": tool_call_id,
                    "title": title,
                    "kind": tool_kind,
                    "status": "pending"
                },
                "options": permission_options(scenario)
            }
        }));

        match read_permission_outcome(lines, *request_id) {
            PermissionFixtureOutcome::Selected(selected) => {
                send_chunk(&format!("permission selected: {selected}"));
            }
            PermissionFixtureOutcome::Cancelled => {
                send_chunk("permission cancelled");
                finish_prompt(prompt_id, "cancelled");
                return;
            }
        }
    }
    finish_prompt(prompt_id, "end_turn");
}

fn permission_options(scenario: &str) -> Value {
    match scenario {
        "permission" | "permission-repeat" => json!([
            { "optionId": "allow", "name": "Allow once", "kind": "allow_once" },
            { "optionId": "deny", "name": "Reject once", "kind": "reject_once" }
        ]),
        "permission-write" => json!([
            { "optionId": "allow-once", "name": "Allow once", "kind": "allow_once" },
            { "optionId": "allow-always", "name": "Always allow", "kind": "allow_always" },
            { "optionId": "reject-once", "name": "Reject once", "kind": "reject_once" },
            { "optionId": "reject-always", "name": "Always reject", "kind": "reject_always" }
        ]),
        "permission-command" => json!([
            { "optionId": "allow-once", "name": "Allow once", "kind": "allow_once" },
            { "optionId": "allow-always", "name": "Always allow", "kind": "allow_always" },
            { "optionId": "reject-once", "name": "Reject once", "kind": "reject_once" }
        ]),
        "permission-always" => json!([
            { "optionId": "allow-always", "name": "Always allow", "kind": "allow_always" },
            { "optionId": "reject-once", "name": "Reject once", "kind": "reject_once" }
        ]),
        _ => json!([]),
    }
}

enum PermissionFixtureOutcome {
    Selected(String),
    Cancelled,
}

fn read_permission_outcome(
    lines: &mut impl Iterator<Item = io::Result<String>>,
    request_id: u64,
) -> PermissionFixtureOutcome {
    loop {
        let Some(Ok(line)) = lines.next() else {
            return PermissionFixtureOutcome::Cancelled;
        };
        let Ok(response) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if response.get("id").and_then(Value::as_u64) == Some(request_id) {
            let outcome = &response["result"]["outcome"];
            if outcome.get("outcome").and_then(Value::as_str) != Some("selected") {
                return PermissionFixtureOutcome::Cancelled;
            }
            let selected = outcome
                .get("optionId")
                .and_then(Value::as_str)
                .unwrap_or("cancelled")
                .to_string();
            return PermissionFixtureOutcome::Selected(selected);
        }
        if response.get("method").and_then(Value::as_str) == Some("session/cancel") {
            return PermissionFixtureOutcome::Cancelled;
        }
    }
}

fn scenario() -> String {
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--scenario" {
            return args.next().unwrap_or_else(|| "normal".to_string());
        }
    }
    "normal".to_string()
}

fn send_chunk(text: &str) {
    write_json(&json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": "fixture-session",
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "messageId": "fixture-message",
                "content": {
                    "type": "text",
                    "text": text
                }
            }
        }
    }));
}

fn finish_prompt(id: Value, stop_reason: &str) {
    write_json(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "stopReason": stop_reason
        }
    }));
}

fn write_raw(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(line.as_bytes());
    let _ = stdout.flush();
}

fn write_json(value: &Value) {
    let mut stdout = io::stdout().lock();
    let _ = serde_json::to_writer(&mut stdout, value);
    let _ = stdout.write_all(b"\n");
    let _ = stdout.flush();
}
