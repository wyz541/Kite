use std::env;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use kite_core::core_api::{CoreApi, CoreError, ExecutionContext, resource_specs, tool_specs};
use serde_json::{Value, json};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

fn main() -> ExitCode {
    let read_only = env::args().skip(1).any(|arg| arg == "--read-only");
    match run(read_only) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kite-mcp: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(read_only: bool) -> Result<(), String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut api = CoreApi::new();
    let base_context = ExecutionContext {
        read_only,
        ..ExecutionContext::default()
    };

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
                    jsonrpc_error(Value::Null, -32700, format!("parse error: {error}")),
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
                    "capabilities": {
                        "tools": { "listChanged": false },
                        "resources": { "subscribe": false, "listChanged": false },
                        "prompts": { "listChanged": false }
                    },
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
            "tools/call" => Some(handle_tool_call(
                &mut api,
                &params,
                id,
                base_context.clone(),
                read_only,
            )),
            "resources/list" => Some(json!({
                "jsonrpc": "2.0",
                "id": id.clone().unwrap_or(Value::Null),
                "result": { "resources": resource_specs() }
            })),
            "resources/read" => Some(handle_resource_read(
                &mut api,
                &params,
                id,
                base_context.clone(),
                read_only,
            )),
            "prompts/list" => Some(json!({
                "jsonrpc": "2.0",
                "id": id.clone().unwrap_or(Value::Null),
                "result": { "prompts": prompt_specs() }
            })),
            "prompts/get" => Some(handle_prompt_get(&params, id)),
            "notifications/initialized" | "notifications/cancelled" => None,
            _ if id.is_none() => None,
            _ => Some(jsonrpc_error(
                id.unwrap_or(Value::Null),
                -32601,
                format!("method not found: {method}"),
            )),
        };
        if let Some(response) = response {
            write_response(&mut output, response)?;
        }
    }
    Ok(())
}

fn handle_tool_call(
    api: &mut CoreApi,
    params: &Value,
    id: Option<Value>,
    base_context: ExecutionContext,
    read_only: bool,
) -> Value {
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return jsonrpc_error(
            id.unwrap_or(Value::Null),
            -32602,
            "tools/call requires params.name",
        );
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let context = match context_from_meta(params.get("_meta"), base_context, read_only) {
        Ok(context) => context,
        Err(error) => return tool_error(id, &error),
    };
    match api.invoke(name, arguments, context) {
        Ok(result) => {
            let text = serde_json::to_string_pretty(&result.data).unwrap_or_else(|error| {
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

fn handle_resource_read(
    api: &mut CoreApi,
    params: &Value,
    id: Option<Value>,
    base_context: ExecutionContext,
    read_only: bool,
) -> Value {
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
        return jsonrpc_error(
            id.unwrap_or(Value::Null),
            -32602,
            "resources/read requires params.uri",
        );
    };
    let context = match context_from_meta(params.get("_meta"), base_context, read_only) {
        Ok(context) => context,
        Err(error) => return jsonrpc_error_with_data(id, -32000, error),
    };
    match api.read_resource(uri, context) {
        Ok((resource, text)) => json!({
            "jsonrpc": "2.0",
            "id": id.unwrap_or(Value::Null),
            "result": {
                "contents": [{
                    "uri": resource.uri,
                    "mimeType": resource.mime_type,
                    "text": text
                }]
            }
        }),
        Err(error) => jsonrpc_error_with_data(id, -32000, error),
    }
}

fn context_from_meta(
    meta: Option<&Value>,
    mut base: ExecutionContext,
    server_read_only: bool,
) -> Result<ExecutionContext, CoreError> {
    if let Some(meta) = meta {
        let parsed = serde_json::from_value::<ExecutionContext>(meta.clone())
            .map_err(|error| CoreError::invalid_args(format!("invalid _meta context: {error}")))?;
        base = parsed;
    }
    base.read_only |= server_read_only;
    if base.trace_id.trim().is_empty() {
        base.trace_id = ExecutionContext::default().trace_id;
    }
    Ok(base)
}

fn handle_prompt_get(params: &Value, id: Option<Value>) -> Value {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match build_prompt(name, &arguments) {
        Ok((description, text)) => json!({
            "jsonrpc": "2.0",
            "id": id.unwrap_or(Value::Null),
            "result": {
                "description": description,
                "messages": [{
                    "role": "user",
                    "content": { "type": "text", "text": text }
                }]
            }
        }),
        Err(error) => jsonrpc_error_with_data(id, -32602, error),
    }
}

fn build_prompt(name: &str, arguments: &Value) -> Result<(String, String), CoreError> {
    match name {
        "agent_debug" => {
            let prompt = arguments
                .get("prompt")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| CoreError::invalid_args("prompt argument is required"))?;
            let profile = arguments
                .get("profile_id")
                .and_then(Value::as_str)
                .unwrap_or("<default>");
            let cwd = arguments
                .get("working_directory")
                .and_then(Value::as_str)
                .unwrap_or(".");
            let context = arguments
                .get("context_paths")
                .cloned()
                .unwrap_or_else(|| json!([]));
            Ok((
                "Run and inspect a real Kite Agent session through Core API tools.".to_string(),
                format!(
                    "Use Kite Core API tools in this order:\n\
                     1. Call agent_profiles_list to confirm profile availability.\n\
                     2. Call agent_connection_test with profile_id={profile}.\n\
                     3. Call agent_session_start with working_directory={cwd} and context_paths={context}.\n\
                     4. Call agent_session_prompt with the returned session_id.\n\
                     5. Poll agent_session_events and agent_session_status until completion or permission request.\n\
                     6. Resolve permission requests explicitly, then call agent_session_close.\n\n\
                     Task:\n{prompt}"
                ),
            ))
        }
        "agent_acceptance" => Ok((
            "Verify the Kite Agent lifecycle without a UI shell.".to_string(),
            "Run the agent_debug sequence with a synthetic workspace, capture JSON outputs for readiness, streaming, completion, cancellation, permissions, errors, and close cleanup. Do not use GUI automation.".to_string(),
        )),
        _ => Err(CoreError::not_found(
            "prompt_not_found",
            format!("unknown prompt: {name}"),
        )),
    }
}

fn prompt_specs() -> Value {
    json!([
        {
            "name": "agent_debug",
            "title": "Debug an Agent session",
            "description": "Guide an MCP client through the Kite Agent Core API lifecycle.",
            "arguments": [
                { "name": "prompt", "description": "Task to send to the Agent.", "required": true },
                { "name": "profile_id", "description": "Agent profile id.", "required": false },
                { "name": "working_directory", "description": "Synthetic or target workspace.", "required": false },
                { "name": "context_paths", "description": "JSON array of context paths.", "required": false }
            ]
        },
        {
            "name": "agent_acceptance",
            "title": "Agent acceptance checklist",
            "description": "Headless acceptance sequence for real Agent verification.",
            "arguments": []
        }
    ])
}

fn tool_error(id: Option<Value>, error: &CoreError) -> Value {
    let text = serde_json::to_string_pretty(error).unwrap_or_else(|serialization| {
        format!("{{\"code\":\"serialization\",\"message\":\"{serialization}\"}}")
    });
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": {
            "content": [{ "type": "text", "text": text }],
            "structuredContent": { "error": error },
            "isError": true
        }
    })
}

fn jsonrpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

fn jsonrpc_error_with_data(id: Option<Value>, code: i64, error: CoreError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": { "code": code, "message": error.message, "data": error }
    })
}

fn write_response(output: &mut impl Write, response: Value) -> Result<(), String> {
    serde_json::to_writer(&mut *output, &response).map_err(|error| error.to_string())?;
    output.write_all(b"\n").map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())
}
