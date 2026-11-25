use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use kite_core::agent::{
    AgentEnvironmentVariable, AgentErrorKind, AgentFailure, AgentSession, AgentSessionConfig,
    AgentSessionEvent, AgentSessionEventCallback, AgentSessionEventSubscription,
    AgentSessionPermissionRequest, AgentSessionState, AgentStopReason,
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const SECRET: &str = "fixture-super-secret-token";

struct Harness {
    session: Arc<AgentSession>,
    events: Receiver<AgentSessionEvent>,
    subscription: Arc<AgentSessionEventSubscription>,
    early_events: Vec<AgentSessionEvent>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.session.shutdown();
        let _ = &self.subscription;
    }
}

struct Collector {
    sender: Sender<AgentSessionEvent>,
}

impl AgentSessionEventCallback for Collector {
    fn on_event(&self, event: AgentSessionEvent) {
        let _ = self.sender.send(event);
    }
}

fn start_session(config: AgentSessionConfig) -> Harness {
    let session = AgentSession::new(config).expect("session config should be valid");
    let (sender, events) = mpsc::channel();
    let subscription = session.subscribe(Box::new(Collector { sender }));
    session.start().expect("session should start");
    let early_events = collect_until(&events, Duration::from_secs(5), |event| {
        matches!(
            event,
            AgentSessionEvent::StateChanged {
                state: AgentSessionState::Ready
            } | AgentSessionEvent::Error { .. }
        )
    });
    if let Some(AgentSessionEvent::Error { failure }) = early_events
        .iter()
        .find(|event| matches!(event, AgentSessionEvent::Error { .. }))
    {
        panic!("session failed during startup: {failure:?}");
    }
    Harness {
        session,
        events,
        subscription,
        early_events,
    }
}

fn start_error(config: AgentSessionConfig) -> AgentFailure {
    let session = AgentSession::new(config).expect("session config should be valid");
    let (sender, events) = mpsc::channel();
    let subscription = session.subscribe(Box::new(Collector { sender }));
    session.start().expect("start request");
    let seen = collect_until(&events, Duration::from_secs(6), |event| {
        matches!(event, AgentSessionEvent::Error { .. })
    });
    let failure = seen
        .iter()
        .find_map(|event| match event {
            AgentSessionEvent::Error { failure } => Some(failure.clone()),
            _ => None,
        })
        .expect("startup error");
    session.shutdown();
    drop(subscription);
    failure
}
fn fixture_config(scenario: &str) -> AgentSessionConfig {
    AgentSessionConfig {
        program: env!("CARGO_BIN_EXE_kite-agent-fixture").to_string(),
        args: vec!["--scenario".to_string(), scenario.to_string()],
        working_directory: env!("CARGO_MANIFEST_DIR").to_string(),
        environment: vec![AgentEnvironmentVariable {
            name: "FIXTURE_TOKEN".to_string(),
            value: SECRET.to_string(),
        }],
        startup_timeout_ms: 5_000,
        turn_timeout_ms: 5_000,
        idle_timeout_ms: 1_500,
        shutdown_timeout_ms: 800,
        max_frame_bytes: 8 * 1024,
        max_output_bytes: 512 * 1024,
        ..AgentSessionConfig::default()
    }
}

fn collect_until(
    events: &Receiver<AgentSessionEvent>,
    timeout: Duration,
    predicate: impl Fn(&AgentSessionEvent) -> bool,
) -> Vec<AgentSessionEvent> {
    let deadline = Instant::now() + timeout;
    let mut seen = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(remaining) {
            Ok(event) => {
                let matched = predicate(&event);
                seen.push(event);
                if matched {
                    return seen;
                }
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!("timed out waiting for Agent event; saw {seen:#?}");
}

fn collect_turn(events: &Receiver<AgentSessionEvent>) -> Vec<AgentSessionEvent> {
    collect_until(events, Duration::from_secs(5), |event| {
        matches!(
            event,
            AgentSessionEvent::TurnCompleted { .. }
                | AgentSessionEvent::TurnCancelled { .. }
                | AgentSessionEvent::Error { .. }
        )
    })
}

fn message_text(events: &[AgentSessionEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            AgentSessionEvent::MessageChunk { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>()
}

fn permission_request(events: &[AgentSessionEvent]) -> AgentSessionPermissionRequest {
    events
        .iter()
        .find_map(|event| match event {
            AgentSessionEvent::PermissionRequested { request } => Some(request.clone()),
            _ => None,
        })
        .expect("permission request event")
}

fn process_exists(pid: u32) -> bool {
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system.process(pid).is_some()
}

#[test]
fn streams_normal_output_and_completes() {
    let harness = start_session(fixture_config("normal"));
    harness
        .session
        .send_prompt("hello".to_string(), Vec::new())
        .expect("prompt");
    let events = collect_turn(&harness.events);
    assert_eq!(message_text(&events), "hello world");
    assert!(events.iter().any(|event| matches!(
        event,
        AgentSessionEvent::TurnCompleted {
            stop_reason: AgentStopReason::EndTurn,
            ..
        }
    )));
    let diagnostic = harness
        .early_events
        .iter()
        .chain(events.iter())
        .find_map(|event| match event {
            AgentSessionEvent::Diagnostic { message, .. } => Some(message.clone()),
            _ => None,
        })
        .expect("stderr diagnostic");
    assert!(!diagnostic.contains(SECRET));
    assert!(!diagnostic.contains("hunter2"));
    assert!(diagnostic.contains("[REDACTED]"));
}

#[test]
fn streamed_markdown_and_stderr_redact_the_resolved_credential() {
    let harness = start_session(fixture_config("secret-stream"));
    harness
        .session
        .send_prompt("echo the credential".to_string(), Vec::new())
        .expect("prompt");
    let events = collect_turn(&harness.events);
    let output = message_text(&events);
    assert!(output.contains("## Fixture answer"));
    assert!(output.contains("[REDACTED]"));
    assert!(!output.contains(SECRET));

    let diagnostics = harness
        .early_events
        .iter()
        .chain(events.iter())
        .filter_map(|event| match event {
            AgentSessionEvent::Diagnostic { message, .. } => Some(message.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(diagnostics.contains("[REDACTED]"));
    assert!(!diagnostics.contains(SECRET));
}

#[test]
fn permission_request_requires_explicit_response() {
    let harness = start_session(fixture_config("permission"));
    harness
        .session
        .send_prompt("write a file".to_string(), Vec::new())
        .expect("prompt");
    let request_events = collect_until(&harness.events, Duration::from_secs(5), |event| {
        matches!(event, AgentSessionEvent::PermissionRequested { .. })
    });
    let request = permission_request(&request_events);
    assert_eq!(request.tool_call_id, "fixture-tool-1");
    assert_eq!(request.options.len(), 2);
    assert!(!message_text(&request_events).contains("approved:"));

    harness
        .session
        .respond_permission(request.request_id, Some("allow".to_string()))
        .expect("permission response");
    let events = collect_turn(&harness.events);
    assert!(message_text(&events).contains("approved: allow"));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentSessionEvent::TurnCompleted {
            stop_reason: AgentStopReason::EndTurn,
            ..
        }
    )));
}

#[test]
fn cancel_returns_cancelled_and_keeps_session_reusable() {
    let harness = start_session(fixture_config("cancel"));
    harness
        .session
        .send_prompt("stop".to_string(), Vec::new())
        .expect("prompt");
    let started = collect_until(
        &harness.events,
        Duration::from_secs(5),
        |event| matches!(event, AgentSessionEvent::MessageChunk { text, .. } if text.contains("waiting")),
    );
    assert!(message_text(&started).contains("waiting for cancel"));
    harness.session.cancel().expect("cancel");
    let events = collect_turn(&harness.events);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentSessionEvent::TurnCancelled { .. }))
    );
    wait_for_state(&harness.session, AgentSessionState::Ready);
}

#[test]
fn idle_timeout_is_structured() {
    let harness = start_session(fixture_config("idle"));
    harness
        .session
        .send_prompt("hang".to_string(), Vec::new())
        .expect("prompt");
    let events = collect_turn(&harness.events);
    let error = events
        .iter()
        .find_map(|event| match event {
            AgentSessionEvent::Error { failure } => Some(failure),
            _ => None,
        })
        .expect("timeout error");
    assert_eq!(error.kind, AgentErrorKind::Timeout);
    wait_for_state(&harness.session, AgentSessionState::Failed);
}

#[test]
fn output_limit_is_structured() {
    let mut config = fixture_config("oversized");
    config.max_frame_bytes = 2048;
    config.max_output_bytes = 128 * 1024;
    let harness = start_session(config);
    harness
        .session
        .send_prompt("oversize".to_string(), Vec::new())
        .expect("prompt");
    let events = collect_turn(&harness.events);
    let error = events
        .iter()
        .find_map(|event| match event {
            AgentSessionEvent::Error { failure } => Some(failure),
            _ => None,
        })
        .expect("output limit error");
    assert_eq!(error.kind, AgentErrorKind::OutputLimit);
}

#[test]
fn authentication_and_rpc_errors_are_structured_and_redacted() {
    let auth_session =
        AgentSession::new(fixture_config("auth")).expect("auth config should be valid");
    let (auth_sender, auth_events) = mpsc::channel();
    let auth_subscription = auth_session.subscribe(Box::new(Collector {
        sender: auth_sender,
    }));
    auth_session.start().expect("auth start request");
    let auth_seen = collect_until(&auth_events, Duration::from_secs(5), |event| {
        matches!(event, AgentSessionEvent::Error { .. })
    });
    let auth_failure = auth_seen
        .iter()
        .find_map(|event| match event {
            AgentSessionEvent::Error { failure } => Some(failure),
            _ => None,
        })
        .expect("authentication failure");
    assert_eq!(auth_failure.kind, AgentErrorKind::AuthenticationRequired);
    auth_session.shutdown();
    drop(auth_subscription);

    let error_harness = start_session(fixture_config("error"));
    error_harness
        .session
        .send_prompt("fail".to_string(), Vec::new())
        .expect("prompt");
    let events = collect_turn(&error_harness.events);
    let failure = events
        .iter()
        .find_map(|event| match event {
            AgentSessionEvent::Error { failure } => Some(failure.clone()),
            _ => None,
        })
        .expect("RPC error");
    assert_ne!(failure.kind, AgentErrorKind::Timeout);
    let detail = failure.detail.unwrap_or_default();
    assert!(!detail.contains(SECRET));
    assert!(detail.contains("[REDACTED]"));
}

#[test]
fn not_installed_is_structured() {
    let mut config = fixture_config("normal");
    config.program = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("missing-agent-binary")
        .to_string_lossy()
        .into_owned();
    let failure = start_error(config);
    assert_eq!(failure.kind, AgentErrorKind::NotInstalled);
}

#[test]
fn cancellation_cleans_up_the_process_tree() {
    let harness = start_session(fixture_config("spawn-tree"));
    harness
        .session
        .send_prompt("spawn".to_string(), Vec::new())
        .expect("prompt");
    let child_events = collect_until(
        &harness.events,
        Duration::from_secs(5),
        |event| matches!(event, AgentSessionEvent::Diagnostic { message, .. } if message.contains("fixture-child-pid:")),
    );
    let child_pid = child_events
        .iter()
        .find_map(|event| match event {
            AgentSessionEvent::Diagnostic { message, .. } => message
                .strip_prefix("fixture-child-pid: ")
                .and_then(|pid| pid.trim().parse::<u32>().ok()),
            _ => None,
        })
        .expect("child pid diagnostic");
    assert!(process_exists(child_pid));
    let mut started = child_events;
    if !message_text(&started).contains("waiting for cancel") {
        started.extend(collect_until(
            &harness.events,
            Duration::from_secs(5),
            |event| matches!(event, AgentSessionEvent::MessageChunk { text, .. } if text.contains("waiting")),
        ));
    }
    assert!(message_text(&started).contains("waiting for cancel"));
    harness.session.cancel().expect("cancel");
    let events = collect_turn(&harness.events);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentSessionEvent::TurnCancelled { .. }))
    );
    harness.session.shutdown();
    wait_until(Duration::from_secs(5), || !process_exists(child_pid));
}

fn wait_for_state(session: &AgentSession, expected: AgentSessionState) {
    wait_until(Duration::from_secs(5), || session.state() == expected);
}

fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if predicate() {
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    panic!("condition was not met before timeout");
}

#[test]
fn handshake_rejects_old_protocol_version() {
    let failure = start_error(fixture_config("old-version"));
    assert_eq!(failure.kind, AgentErrorKind::UnsupportedVersion);
}

#[test]
fn startup_timeout_is_structured() {
    let mut config = fixture_config("startup-hang");
    config.startup_timeout_ms = 300;
    let failure = start_error(config);
    assert_eq!(failure.kind, AgentErrorKind::Timeout);
}

#[test]
fn total_turn_timeout_is_structured() {
    let mut config = fixture_config("slow-drip");
    config.turn_timeout_ms = 600;
    config.idle_timeout_ms = 3_000;
    let harness = start_session(config);
    harness
        .session
        .send_prompt("run".to_string(), Vec::new())
        .expect("prompt");
    let events = collect_turn(&harness.events);
    let failure = events
        .iter()
        .find_map(|event| match event {
            AgentSessionEvent::Error { failure } => Some(failure),
            _ => None,
        })
        .expect("turn timeout");
    assert_eq!(failure.kind, AgentErrorKind::Timeout);
}

#[test]
fn malformed_protocol_output_is_structured() {
    let harness = start_session(fixture_config("malformed"));
    harness
        .session
        .send_prompt("malformed".to_string(), Vec::new())
        .expect("prompt");
    let events = collect_turn(&harness.events);
    let failure = events
        .iter()
        .find_map(|event| match event {
            AgentSessionEvent::Error { failure } => Some(failure),
            _ => None,
        })
        .expect("protocol error");
    assert_eq!(failure.kind, AgentErrorKind::Protocol);
}
