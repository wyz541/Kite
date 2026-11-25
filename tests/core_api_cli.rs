use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

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

#[test]
fn cli_reports_unsupported_transport_for_connection_test_only_profile() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "kite-cli-unsupported-transport-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create temp directory");
    let config: PathBuf = root.join("agent.json");
    fs::write(
        &config,
        serde_json::to_vec_pretty(&json!({
            "schema_version": 1,
            "profiles": [{
                "id": "http-probe",
                "name": "HTTP Probe",
                "transport": "acp_http",
                "endpoint": "http://127.0.0.1:9/acp"
            }],
            "default_profile_id": "http-probe"
        }))
        .expect("serialize config"),
    )
    .expect("write config");

    let explicit_arguments = json!({
        "profile_id": "http-probe",
        "working_directory": "."
    })
    .to_string();
    let output = Command::new(env!("CARGO_BIN_EXE_kite-cli"))
        .env("KITE_AGENT_PROFILE_PATH", &config)
        .args([
            "--confirm",
            "call",
            "agent_session_start",
            "--args",
            explicit_arguments.as_str(),
        ])
        .output()
        .expect("run kite-cli");

    let default_arguments = json!({ "working_directory": "." }).to_string();
    let default_output = Command::new(env!("CARGO_BIN_EXE_kite-cli"))
        .env("KITE_AGENT_PROFILE_PATH", &config)
        .args([
            "--confirm",
            "call",
            "agent_session_start",
            "--args",
            default_arguments.as_str(),
        ])
        .output()
        .expect("run kite-cli with the stored default");

    let _ = fs::remove_dir_all(&root);
    assert_eq!(output.status.code(), Some(5));
    assert!(output.stdout.is_empty());
    let error = serde_json::from_slice::<Value>(&output.stderr).expect("json error");
    assert_eq!(error["kind"], "precondition_failed");
    assert_eq!(error["code"], "unsupported_transport");
    assert_eq!(error["details"]["transport"], "acp_http");
    assert!(
        error["recovery"]
            .as_str()
            .is_some_and(|value| { value.contains("ACP / JSON-RPC stdio") })
    );

    assert_eq!(default_output.status.code(), Some(4));
    let default_error =
        serde_json::from_slice::<Value>(&default_output.stderr).expect("json error");
    assert_eq!(default_error["code"], "profile_not_configured");
    assert!(
        default_error["recovery"]
            .as_str()
            .is_some_and(|value| value.contains("ACP / JSON-RPC stdio"))
    );
}
