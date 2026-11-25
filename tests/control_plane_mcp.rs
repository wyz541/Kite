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
    ];
    for request in requests {
        writeln!(stdin, "{request}").expect("write request");
    }
    drop(stdin);

    let responses = BufReader::new(stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(&line.expect("response line")).expect("json"))
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 3);
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
        responses[2]["result"]["structuredContent"]["path"],
        "Cargo.toml"
    );
    assert!(child.wait().expect("server exit").success());
}
