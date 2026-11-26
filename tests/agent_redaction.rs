use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kite_core::agent::{
    AgentConversation, AgentEvent, AgentMessage, AgentMessageRole, AgentProfile, AgentRuntime,
    AgentRuntimeSnapshot, AgentRuntimeState, load_agent_sessions_at, save_agent_conversation,
};

const SECRET_ENV: &str = "KITE_AGENT_REDACTION_TEST_SECRET";
const SECRET: &str = "mango-river-credential-4729";
const WAIT_TIMEOUT: Duration = Duration::from_secs(20);

#[test]
fn runtime_snapshot_events_log_copy_and_restored_session_redact_resolved_credential() {
    let root = unique_temp_dir("kite-agent-redaction");
    fs::create_dir_all(&root).expect("create test directory");
    let log_path = root.join("agent.log");
    let session_path = root.join("agent-session.json");

    unsafe {
        std::env::set_var(SECRET_ENV, SECRET);
        std::env::set_var("KITE_AGENT_LOG_PATH", &log_path);
        std::env::set_var("KITE_AGENT_SESSION_PATH", &session_path);
    }

    let stream_profile = profile("secret-stream");
    let runtime = AgentRuntime::start(
        stream_profile,
        env!("CARGO_MANIFEST_DIR").to_string(),
        Vec::new(),
        Vec::new(),
    )
    .expect("start runtime with env credential");
    wait_for(&runtime, |snapshot| {
        snapshot.state == AgentRuntimeState::Ready
    });
    runtime
        .send_prompt("echo the credential".to_string(), Vec::new())
        .expect("send prompt");

    let mut events = Vec::new();
    let snapshot = wait_for_with_events(&runtime, &mut events, |snapshot| {
        matches!(
            snapshot.state,
            AgentRuntimeState::Completed
                | AgentRuntimeState::Failed
                | AgentRuntimeState::Cancelled
                | AgentRuntimeState::Closed
        )
    });
    assert_eq!(snapshot.state, AgentRuntimeState::Completed);
    assert!(snapshot.output.contains("## Fixture answer"));
    assert!(snapshot.output.contains("[REDACTED]"));
    assert!(!snapshot.output.contains(SECRET));
    assert_safe_snapshot(&snapshot);
    assert_events_are_safe(&events);

    let log = wait_for_log(&log_path);
    assert!(log.contains("agent_stderr"));
    assert!(log.contains("[REDACTED]"));
    assert!(!log.contains(SECRET));

    let conversation = AgentConversation {
        id: "redaction-conversation".to_string(),
        title: "credential echo".to_string(),
        profile_id: "redaction-profile".to_string(),
        model: None,
        working_directory: env!("CARGO_MANIFEST_DIR").to_string(),
        context_paths: Vec::new(),
        follow_selection: false,
        messages: vec![
            AgentMessage {
                id: "redaction-conversation:user:1".to_string(),
                role: AgentMessageRole::User,
                content: "echo the credential".to_string(),
                unix_ms: 1,
                truncated: false,
            },
            AgentMessage {
                id: "redaction-conversation:assistant:1".to_string(),
                role: AgentMessageRole::Assistant,
                content: snapshot.output.clone(),
                unix_ms: 2,
                truncated: false,
            },
        ],
        remote_session_id: None,
        draft: String::new(),
        window_width: 960,
        window_height: 760,
        updated_unix_ms: 2,
    };
    save_agent_conversation(conversation).expect("persist conversation");
    let restored = load_agent_sessions_at(&session_path);
    let restored_output = restored.store.conversations[0].messages[1].content.clone();
    assert!(!restored_output.contains(SECRET));
    assert!(restored_output.contains("[REDACTED]"));
    let persisted = fs::read_to_string(&session_path).expect("read persisted session");
    assert!(!persisted.contains(SECRET));
    assert_eq!(restored_output, snapshot.output);

    runtime.shutdown();
    wait_for(&runtime, |snapshot| snapshot.closed);

    let error_profile = profile("secret-error");
    let error_runtime = AgentRuntime::start(
        error_profile,
        env!("CARGO_MANIFEST_DIR").to_string(),
        Vec::new(),
        Vec::new(),
    )
    .expect("start error runtime");
    wait_for(&error_runtime, |snapshot| {
        snapshot.state == AgentRuntimeState::Ready
    });
    error_runtime
        .send_prompt("trigger the credential error".to_string(), Vec::new())
        .expect("send error prompt");
    let mut error_events = Vec::new();
    let error_snapshot = wait_for_with_events(&error_runtime, &mut error_events, |snapshot| {
        matches!(
            snapshot.state,
            AgentRuntimeState::Failed
                | AgentRuntimeState::Completed
                | AgentRuntimeState::Cancelled
                | AgentRuntimeState::Closed
        )
    });
    assert_eq!(error_snapshot.state, AgentRuntimeState::Failed);
    assert!(error_snapshot.output.contains("tail "));
    assert!(
        error_snapshot.output.contains("[REDACTED]"),
        "failed runtime output: {:?}",
        error_snapshot.output
    );
    assert!(!error_snapshot.output.contains(SECRET));
    let error = error_snapshot.error.as_deref().unwrap_or_default();
    assert!(error.contains("[REDACTED]"));
    assert!(!error.contains(SECRET));
    assert_safe_snapshot(&error_snapshot);
    assert_events_are_safe(&error_events);
    error_runtime.shutdown();
    wait_for(&error_runtime, |snapshot| snapshot.closed);

    let cancel_runtime = AgentRuntime::start(
        profile("cancel-tail"),
        env!("CARGO_MANIFEST_DIR").to_string(),
        Vec::new(),
        Vec::new(),
    )
    .expect("start cancel runtime");
    wait_for(&cancel_runtime, |snapshot| {
        snapshot.state == AgentRuntimeState::Ready
    });
    cancel_runtime
        .send_prompt("cancel after a secret prefix".to_string(), Vec::new())
        .expect("send cancel prompt");
    wait_for(&cancel_runtime, |snapshot| {
        snapshot.output.contains("tail ")
    });
    cancel_runtime.cancel().expect("cancel secret tail");
    let cancelled = wait_for(&cancel_runtime, |snapshot| {
        snapshot.state == AgentRuntimeState::Cancelled
    });
    assert!(cancelled.output.contains("tail "));
    assert!(cancelled.output.contains("[REDACTED]"));
    assert!(!cancelled.output.contains(SECRET));
    assert_safe_snapshot(&cancelled);
    cancel_runtime.shutdown();
    wait_for(&cancel_runtime, |snapshot| snapshot.closed);

    let pattern_runtime = AgentRuntime::start(
        profile("token-patterns"),
        env!("CARGO_MANIFEST_DIR").to_string(),
        Vec::new(),
        Vec::new(),
    )
    .expect("start token pattern runtime");
    wait_for(&pattern_runtime, |snapshot| {
        snapshot.state == AgentRuntimeState::Ready
    });
    pattern_runtime
        .send_prompt("stream split credentials".to_string(), Vec::new())
        .expect("send token pattern prompt");
    let pattern_snapshot = wait_for(&pattern_runtime, |snapshot| {
        snapshot.state == AgentRuntimeState::Completed
    });
    assert!(pattern_snapshot.output.contains("[REDACTED]"));
    for leaked in ["abc.def", "proj-secret", "abc123"] {
        assert!(
            !pattern_snapshot.output.contains(leaked),
            "streamed pattern leaked {leaked}: {:?}",
            pattern_snapshot.output
        );
    }
    assert_safe_snapshot(&pattern_snapshot);
    pattern_runtime.shutdown();
    wait_for(&pattern_runtime, |snapshot| snapshot.closed);

    let log = fs::read_to_string(&log_path).expect("read final log");
    assert!(!log.contains(SECRET));

    unsafe {
        std::env::remove_var(SECRET_ENV);
        std::env::remove_var("KITE_AGENT_LOG_PATH");
        std::env::remove_var("KITE_AGENT_SESSION_PATH");
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn short_env_credentials_are_rejected_before_runtime_start() {
    let short_env = "KITE_AGENT_REDACTION_SHORT_SECRET";
    unsafe { std::env::set_var(short_env, "abc") };
    let mut short_profile = profile("secret-stream");
    short_profile.credential_ref = Some(format!("env:{short_env}"));
    let result = AgentRuntime::start(
        short_profile,
        env!("CARGO_MANIFEST_DIR").to_string(),
        Vec::new(),
        Vec::new(),
    );
    unsafe { std::env::remove_var(short_env) };

    let error = result.expect_err("short credential must be rejected");
    assert!(error.to_string().contains("至少需要 4 个字符"));
}

fn profile(scenario: &str) -> AgentProfile {
    AgentProfile {
        id: format!("redaction-profile-{scenario}"),
        name: "Redaction Fixture".to_string(),
        executable: Some(env!("CARGO_BIN_EXE_kite-agent-fixture").to_string()),
        arguments: vec!["--scenario".to_string(), scenario.to_string()],
        credential_ref: Some(format!("env:{SECRET_ENV}")),
        credential_env_var: Some("FIXTURE_TOKEN".to_string()),
        timeout_seconds: 20,
        output_limit_bytes: 256 * 1024,
        ..AgentProfile::default()
    }
}

fn wait_for(
    runtime: &AgentRuntime,
    predicate: impl Fn(&AgentRuntimeSnapshot) -> bool,
) -> AgentRuntimeSnapshot {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    let mut last = runtime.snapshot();
    while Instant::now() < deadline {
        last = runtime.snapshot();
        assert_safe_snapshot(&last);
        if predicate(&last) {
            return last;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for runtime state; last snapshot: {last:?}");
}

fn wait_for_with_events(
    runtime: &AgentRuntime,
    events: &mut Vec<AgentEvent>,
    predicate: impl Fn(&AgentRuntimeSnapshot) -> bool,
) -> AgentRuntimeSnapshot {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    let mut last = runtime.snapshot();
    while Instant::now() < deadline {
        last = runtime.snapshot();
        let drained = runtime.drain_events();
        assert_events_are_safe(&drained);
        events.extend(drained);
        assert_safe_snapshot(&last);
        if predicate(&last) {
            let drained = runtime.drain_events();
            assert_events_are_safe(&drained);
            events.extend(drained);
            return last;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for terminal runtime state; last snapshot: {last:?}");
}

fn assert_safe_snapshot(snapshot: &AgentRuntimeSnapshot) {
    let rendered = format!("{snapshot:?}");
    assert!(
        !rendered.contains(SECRET),
        "snapshot leaked exact secret: {rendered}"
    );
}

fn assert_events_are_safe(events: &[AgentEvent]) {
    for event in events {
        if let Some(text) = event.text.as_deref() {
            assert!(!text.contains(SECRET), "event leaked exact secret: {text}");
        }
    }
}

fn wait_for_log(path: &Path) -> String {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(path)
            && text.contains("agent_stderr")
        {
            assert!(!text.contains(SECRET));
            return text;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "timed out waiting for redacted agent log at {}",
        path.display()
    );
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{unique}", std::process::id()))
}
