use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kite_core::agent::{
    AgentEnvironmentVariable, AgentPermissionDecision, AgentProfile, AgentRuntime,
    AgentRuntimeSnapshot, AgentRuntimeState,
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const READY_TIMEOUT: Duration = Duration::from_secs(20);
const TURN_TIMEOUT: Duration = Duration::from_secs(10);
const PROCESS_TIMEOUT: Duration = Duration::from_secs(15);

fn fixture_profile(scenario: &str) -> AgentProfile {
    AgentProfile {
        id: "fixture".to_string(),
        name: "Kite Fixture Agent".to_string(),
        executable: Some(env!("CARGO_BIN_EXE_kite-agent-fixture").to_string()),
        arguments: vec!["--scenario".to_string(), scenario.to_string()],
        timeout_seconds: 20,
        output_limit_bytes: 256 * 1024,
        ..AgentProfile::default()
    }
}

fn start(profile: AgentProfile) -> Arc<AgentRuntime> {
    let runtime = AgentRuntime::start(
        profile,
        env!("CARGO_MANIFEST_DIR").to_string(),
        vec![env!("CARGO_MANIFEST_DIR").to_string()],
        Vec::new(),
    )
    .expect("start fixture runtime");
    wait_for(runtime.as_ref(), READY_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::Ready
    });
    runtime
}

fn wait_for(
    runtime: &AgentRuntime,
    timeout: Duration,
    predicate: impl Fn(&AgentRuntimeSnapshot) -> bool,
) -> AgentRuntimeSnapshot {
    let deadline = Instant::now() + timeout;
    let mut last = runtime.snapshot();
    while Instant::now() < deadline {
        last = runtime.snapshot();
        if predicate(&last) {
            return last;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for Agent state; last snapshot: {last:?}");
}

fn prompt_and_wait(
    runtime: &AgentRuntime,
    _scenario: &str,
    timeout: Duration,
) -> AgentRuntimeSnapshot {
    runtime
        .send_prompt("fixture prompt".to_string(), Vec::new())
        .expect("send prompt");
    wait_for(runtime, timeout, |snapshot| {
        matches!(
            snapshot.state,
            AgentRuntimeState::Completed
                | AgentRuntimeState::Failed
                | AgentRuntimeState::Cancelled
                | AgentRuntimeState::Closed
        )
    })
}

#[test]
fn normal_stream_completes() {
    let runtime = start(fixture_profile("normal"));
    let snapshot = prompt_and_wait(&runtime, "normal", TURN_TIMEOUT);
    assert_eq!(snapshot.state, AgentRuntimeState::Completed);
    assert!(snapshot.output.contains("hello world"));
    runtime.shutdown();
}

#[test]
fn permission_request_requires_explicit_response() {
    let runtime = start(fixture_profile("permission"));
    runtime
        .send_prompt("fixture prompt".to_string(), Vec::new())
        .expect("send prompt");
    let waiting = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::WaitingPermission
            && !snapshot.permission_requests.is_empty()
    });
    let request = waiting.permission_requests[0].clone();
    assert_eq!(request.request_id, "900");
    runtime
        .respond_permission(request.request_id, AgentPermissionDecision::AllowOnce)
        .expect("allow permission");
    let snapshot = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::Completed
    });
    assert!(snapshot.output.contains("approved: allow"));
    runtime.shutdown();
}

#[test]
fn cancel_stops_a_hanging_turn() {
    let runtime = start(fixture_profile("cancel"));
    runtime
        .send_prompt("fixture prompt".to_string(), Vec::new())
        .expect("send prompt");
    wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::Running
    });
    runtime.cancel().expect("cancel");
    let snapshot = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::Cancelled
    });
    assert_eq!(snapshot.state, AgentRuntimeState::Cancelled);
    runtime.shutdown();
}

#[test]
fn idle_timeout_is_structured() {
    let mut profile = fixture_profile("idle");
    profile.timeout_seconds = 1;
    let runtime = start(profile);
    let snapshot = prompt_and_wait(&runtime, "", TURN_TIMEOUT);
    assert!(matches!(
        snapshot.state,
        AgentRuntimeState::Failed | AgentRuntimeState::Closed
    ));
    assert!(
        snapshot
            .error
            .as_deref()
            .is_some_and(|error| error.contains("超时"))
    );
    runtime.shutdown();
}

#[test]
fn oversized_output_is_rejected() {
    let mut profile = fixture_profile("oversized");
    profile.output_limit_bytes = 16 * 1024;
    let runtime = start(profile);
    runtime
        .send_prompt("fixture prompt".to_string(), Vec::new())
        .expect("send prompt");
    let snapshot = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.output_truncated
            || matches!(
                snapshot.state,
                AgentRuntimeState::Completed
                    | AgentRuntimeState::Failed
                    | AgentRuntimeState::Cancelled
                    | AgentRuntimeState::Closed
            )
    });
    assert!(snapshot.output_truncated);
    runtime.shutdown();
    wait_for(&runtime, PROCESS_TIMEOUT, |snapshot| snapshot.closed);
}

#[test]
fn fixture_failure_is_structured() {
    let runtime = start(fixture_profile("error"));
    let snapshot = prompt_and_wait(&runtime, "error", TURN_TIMEOUT);
    assert_eq!(snapshot.state, AgentRuntimeState::Failed);
    assert!(
        snapshot
            .error
            .as_deref()
            .is_some_and(|error| error.contains("fixture failure"))
    );
    runtime.shutdown();
}

#[test]
fn child_process_is_removed_with_the_agent_tree() {
    let pid_path = std::env::temp_dir().join(format!(
        "kite-agent-child-{}-{}.pid",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let mut profile = fixture_profile("spawn-tree");
    profile.environment.push(AgentEnvironmentVariable {
        name: "KITE_AGENT_FIXTURE_CHILD_PID_FILE".to_string(),
        value: pid_path.to_string_lossy().into_owned(),
    });
    let runtime = start(profile);
    runtime
        .send_prompt("fixture prompt".to_string(), Vec::new())
        .expect("send prompt");
    let child_pid = wait_for_pid(&pid_path, PROCESS_TIMEOUT);
    assert!(process_exists(child_pid));
    runtime.cancel().expect("cancel");
    wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        matches!(
            snapshot.state,
            AgentRuntimeState::Cancelled | AgentRuntimeState::Closed
        )
    });
    runtime.shutdown();
    wait_for_process_exit(child_pid, PROCESS_TIMEOUT);
    assert!(!process_exists(child_pid));
    let _ = fs::remove_file(pid_path);
}

fn wait_for_pid(path: &PathBuf, timeout: Duration) -> u32 {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(value) = fs::read_to_string(path)
            && let Ok(pid) = value.trim().parse()
        {
            return pid;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for fixture child pid");
}

fn process_exists(pid: u32) -> bool {
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing(),
    );
    system.process(pid).is_some()
}

fn wait_for_process_exit(pid: u32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
}
