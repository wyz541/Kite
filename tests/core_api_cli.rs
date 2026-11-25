use std::process::Command;

use serde_json::Value;

#[test]
fn cli_read_only_query_returns_structured_core_result() {
    let output = Command::new(env!("CARGO_BIN_EXE_kite-cli"))
        .args([
            "--read-only",
            "--trace-id",
            "cli-trace",
            "files",
            "stat",
            "Cargo.toml",
        ])
        .output()
        .expect("run kite-cli");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let result = serde_json::from_slice::<Value>(&output.stdout).expect("json result");
    assert_eq!(result["trace_id"], "cli-trace");
    assert_eq!(result["changed"], false);
    assert_eq!(result["data"]["path"], "Cargo.toml");
}

#[test]
fn cli_maps_core_error_kind_to_exit_code_and_stderr() {
    let output = Command::new(env!("CARGO_BIN_EXE_kite-cli"))
        .args(["--read-only", "call", "agent_session_start", "--args", "{}"])
        .output()
        .expect("run kite-cli");
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    let error = serde_json::from_slice::<Value>(&output.stderr).expect("json error");
    assert_eq!(error["kind"], "forbidden");
    assert_eq!(error["code"], "read_only_mode");
    assert!(error["recovery"].is_string());
}
