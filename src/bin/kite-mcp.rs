use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use kite_core::control::{ControlError, ControlPlane, tool_specs};
use serde_json::{Value, json};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kite-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut plane = ControlPlane::new();

    for line in stdin.lock().lines() {
        let line = line.map_err(|error| error.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let request = match serde_json::from_str::<Value>(&line) {
            Ok(request) => request,
            Err(error) => {
                write_response(
                    &mut output,
                    json!({
                        "jsonrpc": "2.0",
                        "id": Value::Null,
                        "error": { "code": -32700, "message": format!("parse error: {error}") }
                    }),
                )?;
                continue;
            }
        };
        let id = request.get("id").cloned();
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let response = match method {
            "initialize" => Some(json!({
                "jsonrpc": "2.0",
                "id": id.clone().unwrap_or(Value::Null),
                "result": {
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": "kite",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            })),
            "ping" => Some(json!({
                "jsonrpc": "2.0",
                "id": id.clone().unwrap_or(Value::Null),
                "result": {}
            })),
            "tools/list" => Some(json!({
                "jsonrpc": "2.0",
                "id": id.clone().unwrap_or(Value::Null),
                "result": { "tools": tool_specs() }
            })),
            "tools/call" => Some(handle_tool_call(&mut plane, &params, id)),
            "notifications/initialized" | "notifications/cancelled" => None,
            _ => Some(json!({
                "jsonrpc": "2.0",
                "id": id.clone().unwrap_or(Value::Null),
                "error": { "code": -32601, "message": format!("method not found: {method}") }
            })),
        };
        if let Some(response) = response {
            write_response(&mut output, response)?;
        }
    }
    Ok(())
}

fn handle_tool_call(plane: &mut ControlPlane, params: &Value, id: Option<Value>) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return json!({
            "jsonrpc": "2.0",
            "id": id.unwrap_or(Value::Null),
            "error": { "code": -32602, "message": "tools/call requires params.name" }
        });
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match plane.execute(name, arguments) {
        Ok(result) => {
            let text = serde_json::to_string_pretty(&result).unwrap_or_else(|error| {
                format!("{{\"code\":\"serialization\",\"message\":\"{error}\"}}")
            });
            json!({
                "jsonrpc": "2.0",
                "id": id.unwrap_or(Value::Null),
                "result": {
                    "content": [{ "type": "text", "text": text }],
                    "structuredContent": result,
                    "isError": false
                }
            })
        }
        Err(error) => tool_error(id, &error),
    }
}

fn tool_error(id: Option<Value>, error: &ControlError) -> Value {
    let text = serde_json::to_string_pretty(error).unwrap_or_else(|serialization| {
        format!("{{\"code\":\"serialization\",\"message\":\"{serialization}\"}}")
    });
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": {
            "content": [{ "type": "text", "text": text }],
            "isError": true
        }
    })
}

fn write_response(output: &mut impl Write, response: Value) -> Result<(), String> {
    serde_json::to_writer(&mut *output, &response).map_err(|error| error.to_string())?;
    output.write_all(b"\n").map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())
}
