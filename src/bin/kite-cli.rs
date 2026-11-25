use std::env;
use std::fs;
use std::io::{self, BufRead, Read as _, Write as _};
use std::process::ExitCode;

use kite_core::core_api::{CoreApi, CoreError, CoreResult, ExecutionContext, tool_specs};
use serde_json::{Map, Value, json};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = serde_json::to_writer_pretty(io::stderr(), &error);
            let _ = writeln!(io::stderr());
            ExitCode::from(error.kind.exit_code())
        }
    }
}

fn run() -> Result<(), CoreError> {
    let (args, context) = parse_global_options(env::args().skip(1).collect())?;
    match args.first().map(String::as_str) {
        None | Some("help") | Some("--help") | Some("-h") => {
            print_help();
            Ok(())
        }
        Some("list-tools") => emit_value(
            serde_json::to_value(tool_specs())
                .map_err(|error| CoreError::new("serialization", error.to_string()))?,
            !args.iter().any(|arg| arg == "--compact"),
        ),
        Some("call") => run_call(&args[1..], context),
        Some("batch") => run_batch(context),
        Some("agent") => run_agent(&args[1..], context),
        Some("files") => run_files(&args[1..], context),
        Some("state") => {
            let result = CoreApi::new().invoke("app_state", json!({}), context)?;
            emit_result(result, true)
        }
        Some(command) => Err(CoreError::invalid_args(format!(
            "unknown command: {command}"
        ))),
    }
}

fn parse_global_options(args: Vec<String>) -> Result<(Vec<String>, ExecutionContext), CoreError> {
    let mut context = ExecutionContext::default();
    let mut cleaned = Vec::with_capacity(args.len());
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--trace-id" => {
                index += 1;
                context.trace_id = required_flag_arg(&args, index, "--trace-id")?.to_string();
            }
            "--actor" => {
                index += 1;
                context.actor = Some(required_flag_arg(&args, index, "--actor")?.to_string());
            }
            "--idempotency-key" => {
                index += 1;
                context.idempotency_key =
                    Some(required_flag_arg(&args, index, "--idempotency-key")?.to_string());
            }
            "--deadline-ms" => {
                index += 1;
                let value = required_flag_arg(&args, index, "--deadline-ms")?;
                let timeout_ms = value.parse::<u64>().map_err(|error| {
                    CoreError::invalid_args(format!("invalid --deadline-ms: {error}"))
                })?;
                context = context.with_timeout_ms(timeout_ms);
            }
            "--read-only" => context.read_only = true,
            "--confirm" => context.confirmed = true,
            _ => cleaned.push(args[index].clone()),
        }
        index += 1;
    }
    Ok((cleaned, context))
}

fn run_call(args: &[String], context: ExecutionContext) -> Result<(), CoreError> {
    let Some(tool) = args.first() else {
        return Err(CoreError::invalid_args(
            "usage: kite-cli call <tool> [--args <json> | --args-file <path>] [--compact]",
        ));
    };
    let mut arguments = json!({});
    let mut compact = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--args" => {
                index += 1;
                let raw = args
                    .get(index)
                    .ok_or_else(|| CoreError::invalid_args("--args requires JSON"))?;
                arguments = serde_json::from_str(raw).map_err(|error| {
                    CoreError::invalid_args(format!("invalid --args JSON: {error}"))
                })?;
            }
            "--args-file" => {
                index += 1;
                let path = args
                    .get(index)
                    .ok_or_else(|| CoreError::invalid_args("--args-file requires a path"))?;
                let raw = fs::read_to_string(path)
                    .map_err(|error| CoreError::new("io", error.to_string()))?;
                arguments = serde_json::from_str(&raw).map_err(|error| {
                    CoreError::invalid_args(format!("invalid arguments file JSON: {error}"))
                })?;
            }
            "--compact" => compact = true,
            flag => {
                return Err(CoreError::invalid_args(format!(
                    "unknown call flag: {flag}"
                )));
            }
        }
        index += 1;
    }
    let result = CoreApi::new().invoke(tool, arguments, context)?;
    emit_result(result, !compact)
}

fn run_batch(base_context: ExecutionContext) -> Result<(), CoreError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    let mut api = CoreApi::new();
    for (line_number, line) in stdin.lock().lines().enumerate() {
        let line = line.map_err(|error| CoreError::new("io", error.to_string()))?;
        if line.trim().is_empty() {
            continue;
        }
        let request = match serde_json::from_str::<Value>(&line) {
            Ok(request) => request,
            Err(error) => {
                let response = json!({
                    "ok": false,
                    "error": CoreError::invalid_args(format!(
                        "line {} is not valid JSON: {error}",
                        line_number + 1
                    )),
                });
                writeln!(output, "{response}")
                    .map_err(|error| CoreError::new("io", error.to_string()))?;
                continue;
            }
        };
        let tool = request
            .get("tool")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let arguments = request
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let context = match request.get("context").cloned() {
            Some(context) => serde_json::from_value::<ExecutionContext>(context)
                .map_err(|error| CoreError::invalid_args(format!("invalid context: {error}")))?,
            None => base_context.clone(),
        };
        let response = match api.invoke(tool, arguments, context) {
            Ok(result) => json!({ "ok": true, "result": result }),
            Err(error) => json!({ "ok": false, "error": error }),
        };
        writeln!(output, "{response}").map_err(|error| CoreError::new("io", error.to_string()))?;
        output
            .flush()
            .map_err(|error| CoreError::new("io", error.to_string()))?;
    }
    Ok(())
}

fn run_agent(args: &[String], context: ExecutionContext) -> Result<(), CoreError> {
    match args.first().map(String::as_str) {
        None | Some("help") | Some("--help") | Some("-h") => {
            eprintln!(
                "usage: kite-cli agent <profiles|test|run|diagnostics> [options]\n\
                 \n  profiles\n  test [--profile ID]\n  run --prompt TEXT [--profile ID] [--cwd DIR] [--context PATH]... [--timeout-ms N]\n  diagnostics"
            );
            Ok(())
        }
        Some("profiles") => {
            let result = CoreApi::new().invoke("agent_profiles_list", json!({}), context)?;
            emit_result(result, true)
        }
        Some("test") => {
            let profile_id = flag_value(args, "--profile")?;
            let mut arguments = Map::new();
            if let Some(profile_id) = profile_id {
                arguments.insert("profile_id".to_string(), json!(profile_id));
            }
            let result = CoreApi::new().invoke(
                "agent_connection_test",
                Value::Object(arguments),
                context,
            )?;
            emit_result(result, true)
        }
        Some("run") => run_agent_run(&args[1..], context),
        Some("diagnostics") => {
            let result = CoreApi::new().invoke("agent_diagnostics", json!({}), context)?;
            emit_result(result, true)
        }
        Some(command) => Err(CoreError::invalid_args(format!(
            "unknown agent command: {command}"
        ))),
    }
}

fn run_agent_run(args: &[String], context: ExecutionContext) -> Result<(), CoreError> {
    let mut arguments = Map::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--profile" => {
                index += 1;
                arguments.insert(
                    "profile_id".to_string(),
                    json!(required_flag_arg(args, index, "--profile")?),
                );
            }
            "--cwd" => {
                index += 1;
                arguments.insert(
                    "working_directory".to_string(),
                    json!(required_flag_arg(args, index, "--cwd")?),
                );
            }
            "--context" => {
                index += 1;
                let value = required_flag_arg(args, index, "--context")?;
                let values = arguments
                    .entry("context_paths".to_string())
                    .or_insert_with(|| json!([]));
                values
                    .as_array_mut()
                    .expect("context_paths initialized as array")
                    .push(json!(value));
            }
            "--prompt" => {
                index += 1;
                let value = required_flag_arg(args, index, "--prompt")?;
                let prompt = if value == "-" {
                    let mut text = String::new();
                    io::stdin()
                        .read_to_string(&mut text)
                        .map_err(|error| CoreError::new("io", error.to_string()))?;
                    text
                } else {
                    value.to_string()
                };
                arguments.insert("prompt".to_string(), json!(prompt));
            }
            "--timeout-ms" => {
                index += 1;
                let value = required_flag_arg(args, index, "--timeout-ms")?;
                let parsed = value.parse::<u64>().map_err(|error| {
                    CoreError::invalid_args(format!("invalid --timeout-ms: {error}"))
                })?;
                arguments.insert("timeout_ms".to_string(), json!(parsed));
            }
            flag => {
                return Err(CoreError::invalid_args(format!(
                    "unknown agent run flag: {flag}"
                )));
            }
        }
        index += 1;
    }
    if !arguments.contains_key("prompt") {
        return Err(CoreError::invalid_args("agent run requires --prompt"));
    }
    let result = CoreApi::new().invoke("agent_run", Value::Object(arguments), context)?;
    emit_result(result, true)
}

fn run_files(args: &[String], context: ExecutionContext) -> Result<(), CoreError> {
    match args.first().map(String::as_str) {
        None | Some("help") | Some("--help") | Some("-h") => {
            eprintln!(
                "usage: kite-cli files <list|stat|read> [options]\n\
                 \n  list [PATH] [--sort name|modified|kind|size] [--descending]\n\
                 \n  stat PATH\n\
                 \n  read PATH [--max-bytes N] [--truncate]"
            );
            Ok(())
        }
        Some("list") => {
            let mut arguments = Map::new();
            if let Some(path) = args.get(1).filter(|value| !value.starts_with("--")) {
                arguments.insert("path".to_string(), json!(path));
            }
            if let Some(sort) = flag_value(&args[1..], "--sort")? {
                arguments.insert("sort_by".to_string(), json!(sort));
            }
            if args.iter().any(|arg| arg == "--descending") {
                arguments.insert("ascending".to_string(), json!(false));
            }
            let result = CoreApi::new().invoke("files_list", Value::Object(arguments), context)?;
            emit_result(result, true)
        }
        Some("stat") => {
            let path = args
                .get(1)
                .ok_or_else(|| CoreError::invalid_args("files stat requires PATH"))?;
            let result = CoreApi::new().invoke("files_stat", json!({ "path": path }), context)?;
            emit_result(result, true)
        }
        Some("read") => {
            let path = args
                .get(1)
                .ok_or_else(|| CoreError::invalid_args("files read requires PATH"))?;
            let mut arguments = Map::new();
            arguments.insert("path".to_string(), json!(path));
            if let Some(max_bytes) = flag_value(&args[2..], "--max-bytes")? {
                let parsed = max_bytes.parse::<u64>().map_err(|error| {
                    CoreError::invalid_args(format!("invalid --max-bytes: {error}"))
                })?;
                arguments.insert("max_bytes".to_string(), json!(parsed));
            }
            if args.iter().any(|arg| arg == "--truncate") {
                arguments.insert("truncate".to_string(), json!(true));
            }
            let result =
                CoreApi::new().invoke("files_read_text", Value::Object(arguments), context)?;
            emit_result(result, true)
        }
        Some(command) => Err(CoreError::invalid_args(format!(
            "unknown files command: {command}"
        ))),
    }
}

fn flag_value(args: &[String], flag: &str) -> Result<Option<String>, CoreError> {
    let Some(index) = args.iter().position(|arg| arg == flag) else {
        return Ok(None);
    };
    let value = args
        .get(index + 1)
        .ok_or_else(|| CoreError::invalid_args(format!("{flag} requires a value")))?;
    Ok(Some(value.clone()))
}

fn required_flag_arg<'a>(
    args: &'a [String],
    index: usize,
    flag: &str,
) -> Result<&'a str, CoreError> {
    args.get(index)
        .map(String::as_str)
        .ok_or_else(|| CoreError::invalid_args(format!("{flag} requires a value")))
}

fn emit_result(value: CoreResult, pretty: bool) -> Result<(), CoreError> {
    if pretty {
        serde_json::to_writer_pretty(io::stdout(), &value)
    } else {
        serde_json::to_writer(io::stdout(), &value)
    }
    .map_err(|error| CoreError::new("serialization", error.to_string()))?;
    writeln!(io::stdout()).map_err(|error| CoreError::new("io", error.to_string()))
}

fn emit_value(value: Value, pretty: bool) -> Result<(), CoreError> {
    if pretty {
        serde_json::to_writer_pretty(io::stdout(), &value)
    } else {
        serde_json::to_writer(io::stdout(), &value)
    }
    .map_err(|error| CoreError::new("serialization", error.to_string()))?;
    writeln!(io::stdout()).map_err(|error| CoreError::new("io", error.to_string()))
}

fn print_help() {
    eprintln!(
        "kite-cli - Kite Core API terminal adapter\n\n\
         Global options:\n\
           --trace-id ID\n\
           --actor ID\n\
           --idempotency-key KEY\n\
           --deadline-ms N\n\
           --read-only\n\
           --confirm\n\n\
         Usage:\n\
           kite-cli list-tools [--compact]\n\
           kite-cli call <tool> [--args JSON | --args-file PATH] [--compact]\n\
           kite-cli batch\n\
           kite-cli agent profiles\n\
           kite-cli agent test [--profile ID]\n\
           kite-cli agent run --prompt TEXT [--profile ID] [--cwd DIR] [--context PATH]... [--timeout-ms N]\n\
           kite-cli agent diagnostics\n\
           kite-cli files list [PATH]\n\
           kite-cli files stat PATH\n\
           kite-cli files read PATH [--max-bytes N] [--truncate]\n\
           kite-cli state\n\n\
         Use '--prompt -' to read a prompt from stdin. Batch mode reads one JSON object per line\n\
         with shape {{\"tool\":\"...\",\"arguments\":{{...}},\"context\":{{...}}}}."
    );
}
