use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

#[test]
fn mcp_stdio_exposes_and_calls_the_shared_registry() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kite-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kite-mcp");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let requests = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2024-11-05", "clientInfo": { "name": "test", "version": "1" } }
        }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "files_stat", "arguments": { "path": "Cargo.toml" } }
        }),
        json!({ "jsonrpc": "2.0", "id": 4, "method": "resources/list", "params": {} }),
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "resources/read",
            "params": { "uri": "kite://agent/profiles" }
        }),
        json!({ "jsonrpc": "2.0", "id": 6, "method": "prompts/list", "params": {} }),
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "prompts/get",
            "params": {
                "name": "agent_debug",
                "arguments": { "prompt": "inspect", "working_directory": "." }
            }
        }),
    ];
    for request in requests {
        writeln!(stdin, "{request}").expect("write request");
    }
    drop(stdin);

    let responses = BufReader::new(stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(&line.expect("response line")).expect("json"))
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 7);
    assert_eq!(responses[0]["result"]["serverInfo"]["name"], "kite");

    let tools = responses[1]["result"]["tools"]
        .as_array()
        .expect("tool list");
    assert!(
        tools
            .iter()
            .any(|tool| tool["name"] == "agent_session_prompt")
    );
    assert!(tools.iter().any(|tool| tool["name"] == "files_read_text"));

    assert_eq!(responses[2]["result"]["isError"], false);
    assert_eq!(
        responses[2]["result"]["structuredContent"]["data"]["path"],
        "Cargo.toml"
    );
    assert!(
        responses[3]["result"]["resources"]
            .as_array()
            .expect("resources")
            .iter()
            .any(|resource| resource["uri"] == "kite://agent/profiles")
    );
    assert!(
        responses[4]["result"]["contents"]
            .as_array()
            .expect("resource contents")
            .iter()
            .any(|content| content["mimeType"] == "application/json")
    );
    assert!(
        responses[5]["result"]["prompts"]
            .as_array()
            .expect("prompts")
            .iter()
            .any(|prompt| prompt["name"] == "agent_debug")
    );
    assert_eq!(responses[6]["result"]["messages"][0]["role"], "user");
    assert!(child.wait().expect("server exit").success());
}

fn run_server(args: &[&str], requests: Vec<Value>) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kite-mcp"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kite-mcp");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    for request in requests {
        writeln!(stdin, "{request}").expect("write request");
    }
    drop(stdin);
    let responses = BufReader::new(stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(&line.expect("response line")).expect("json"))
        .collect::<Vec<_>>();
    assert!(child.wait().expect("server exit").success());
    responses
}

#[test]
fn mcp_read_only_and_confirmation_guards_are_enforced() {
    let read_only = run_server(
        &["--read-only"],
        vec![
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": "2024-11-05" }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": "agent_session_start", "arguments": {} }
            }),
        ],
    );
    assert_eq!(read_only[1]["result"]["isError"], true);
    assert_eq!(
        read_only[1]["result"]["structuredContent"]["error"]["code"],
        "read_only_mode"
    );

    let confirmation = run_server(
        &[],
        vec![
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": "2024-11-05" }
            }),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "agent_session_close",
                    "arguments": { "session_id": "missing" }
                }
            }),
        ],
    );
    assert_eq!(confirmation[1]["result"]["isError"], true);
    assert_eq!(
        confirmation[1]["result"]["structuredContent"]["error"]["code"],
        "confirmation_required"
    );
}
