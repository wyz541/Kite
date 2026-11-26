use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kite_core::agent::{
    AgentCapabilities, AgentEnvironmentVariable, AgentPermissionDecision, AgentProfile,
    AgentRuntime, AgentRuntimeSnapshot, AgentRuntimeState, PermissionPolicy,
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

fn permission_profile(
    scenario: &str,
    policy: PermissionPolicy,
    capabilities: AgentCapabilities,
) -> AgentProfile {
    AgentProfile {
        permission: policy,
        capabilities,
        ..fixture_profile(scenario)
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
fn runtime_uses_resolved_profile_working_directory_not_caller_cwd() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "kite-agent-runtime-cwd-{}-{unique}",
        std::process::id()
    ));
    let nested = root.join("src");
    let file = nested.join("main.rs");
    fs::create_dir_all(&nested).expect("create nested directory");
    fs::write(&file, "fn main() {}").expect("write context file");

    let runtime = AgentRuntime::start(
        fixture_profile("cwd"),
        root.to_string_lossy().into_owned(),
        vec![file.to_string_lossy().into_owned()],
        Vec::new(),
    )
    .expect("start fixture runtime");
    wait_for(runtime.as_ref(), READY_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::Ready
    });
    let snapshot = prompt_and_wait(&runtime, "cwd", TURN_TIMEOUT);

    assert_eq!(snapshot.state, AgentRuntimeState::Completed);
    assert_eq!(
        PathBuf::from(snapshot.output.trim()),
        dunce::canonicalize(&nested).expect("canonical nested directory")
    );
    runtime.shutdown();
    wait_for(&runtime, PROCESS_TIMEOUT, |snapshot| snapshot.closed);
    assert_eq!(runtime.snapshot().state, AgentRuntimeState::Completed);
    thread::sleep(Duration::from_millis(100));
    fs::remove_dir_all(root).expect("remove temp directory");
}

#[test]
fn runtime_rejects_invalid_fixed_working_directory_before_spawn() {
    let mut profile = fixture_profile("normal");
    profile.working_directory = kite_core::agent::WorkingDirectoryPolicy::Fixed {
        path: std::env::temp_dir()
            .join("kite-missing-fixed-agent-directory")
            .to_string_lossy()
            .into_owned(),
    };
    let error = AgentRuntime::start(
        profile,
        env!("CARGO_MANIFEST_DIR").to_string(),
        vec![env!("CARGO_MANIFEST_DIR").to_string()],
        Vec::new(),
    )
    .expect_err("invalid fixed directory must block startup");
    assert!(error.to_string().contains("固定的工作目录不可用"));
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
fn permission_request_requires_an_exact_returned_option() {
    let runtime = start(permission_profile(
        "permission",
        PermissionPolicy::AgentManaged,
        AgentCapabilities::default(),
    ));
    runtime
        .send_prompt("fixture prompt".to_string(), Vec::new())
        .expect("send prompt");
    let waiting = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::WaitingPermission
            && !snapshot.permission_requests.is_empty()
    });
    let request = waiting.permission_requests[0].clone();
    assert_eq!(request.request_id, "900");
    assert!(
        !request
            .allowed_decisions
            .contains(&AgentPermissionDecision::AllowTask)
    );
    assert!(
        runtime
            .respond_permission(
                request.request_id.clone(),
                AgentPermissionDecision::AllowTask,
            )
            .is_err()
    );
    let still_waiting = runtime.snapshot();
    assert_eq!(still_waiting.state, AgentRuntimeState::WaitingPermission);
    assert_eq!(still_waiting.permission_requests.len(), 1);
    runtime
        .respond_permission(request.request_id, AgentPermissionDecision::AllowOnce)
        .expect("allow permission");
    let snapshot = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::Completed
    });
    assert!(snapshot.output.contains("permission selected: allow"));
    runtime.shutdown();
}

#[test]
fn read_only_profile_cannot_allow_write_requests() {
    let runtime = start(permission_profile(
        "permission-write",
        PermissionPolicy::ReadOnly,
        AgentCapabilities {
            write_files: true,
            execute_commands: true,
            network: true,
            ..AgentCapabilities::default()
        },
    ));
    runtime
        .send_prompt("fixture prompt".to_string(), Vec::new())
        .expect("send prompt");
    let waiting = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::WaitingPermission
            && !snapshot.permission_requests.is_empty()
    });
    let request = waiting.permission_requests[0].clone();
    assert_eq!(request.kind, "edit");
    assert!(
        !request
            .allowed_decisions
            .contains(&AgentPermissionDecision::AllowOnce)
    );
    assert!(
        !request
            .allowed_decisions
            .contains(&AgentPermissionDecision::AllowTask)
    );
    assert!(
        request
            .allowed_decisions
            .contains(&AgentPermissionDecision::RejectOnce)
    );
    assert!(
        request
            .allowed_decisions
            .contains(&AgentPermissionDecision::RejectAlways)
    );
    assert!(
        request
            .policy_notice
            .as_deref()
            .is_some_and(|notice| notice.contains("只读策略"))
    );

    assert!(
        runtime
            .respond_permission(
                request.request_id.clone(),
                AgentPermissionDecision::AllowOnce,
            )
            .is_err()
    );
    assert!(
        runtime
            .respond_permission(
                request.request_id.clone(),
                AgentPermissionDecision::AllowTask,
            )
            .is_err()
    );
    assert_eq!(
        runtime.snapshot().state,
        AgentRuntimeState::WaitingPermission
    );

    runtime
        .respond_permission(request.request_id, AgentPermissionDecision::RejectOnce)
        .expect("reject write permission");
    let snapshot = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::Completed
    });
    assert!(snapshot.output.contains("permission selected: reject-once"));
    runtime.shutdown();
}

#[test]
fn rejected_permission_with_upstream_error_is_classified_cleanly() {
    let runtime = start(fixture_profile("permission-error"));
    runtime
        .send_prompt("fixture prompt".to_string(), Vec::new())
        .expect("send prompt");
    let waiting = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::WaitingPermission
    });
    let request = waiting.permission_requests[0].clone();
    runtime
        .respond_permission(request.request_id, AgentPermissionDecision::RejectOnce)
        .expect("reject permission");
    let snapshot = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::Failed
    });
    assert_eq!(snapshot.error.as_deref(), Some("权限请求已拒绝"));
    assert!(
        !snapshot
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Internal error"))
    );
    runtime.shutdown();
}

#[test]
fn read_only_profile_cannot_allow_command_requests() {
    let runtime = start(permission_profile(
        "permission-command",
        PermissionPolicy::ReadOnly,
        AgentCapabilities {
            execute_commands: true,
            ..AgentCapabilities::default()
        },
    ));
    runtime
        .send_prompt("fixture prompt".to_string(), Vec::new())
        .expect("send prompt");
    let waiting = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::WaitingPermission
            && !snapshot.permission_requests.is_empty()
    });
    let request = waiting.permission_requests[0].clone();
    assert_eq!(request.kind, "execute");
    assert!(
        !request
            .allowed_decisions
            .contains(&AgentPermissionDecision::AllowOnce)
    );
    assert!(
        !request
            .allowed_decisions
            .contains(&AgentPermissionDecision::AllowTask)
    );

    runtime.cancel().expect("cancel blocked permission request");
    let snapshot = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::Cancelled
    });
    assert!(snapshot.permission_requests.is_empty());
    runtime.shutdown();
}

#[test]
fn confirm_writes_uses_the_declared_write_capability() {
    let blocked = start(permission_profile(
        "permission-write",
        PermissionPolicy::ConfirmWrites,
        AgentCapabilities::default(),
    ));
    blocked
        .send_prompt("fixture prompt".to_string(), Vec::new())
        .expect("send blocked prompt");
    let waiting = wait_for(&blocked, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::WaitingPermission
            && !snapshot.permission_requests.is_empty()
    });
    let blocked_request = waiting.permission_requests[0].clone();
    assert!(
        !blocked_request
            .allowed_decisions
            .contains(&AgentPermissionDecision::AllowOnce)
    );
    blocked
        .respond_permission(
            blocked_request.request_id,
            AgentPermissionDecision::RejectOnce,
        )
        .expect("reject undeclared write");
    wait_for(&blocked, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::Completed
    });
    blocked.shutdown();

    let allowed = start(permission_profile(
        "permission-write",
        PermissionPolicy::ConfirmWrites,
        AgentCapabilities {
            write_files: true,
            ..AgentCapabilities::default()
        },
    ));
    allowed
        .send_prompt("fixture prompt".to_string(), Vec::new())
        .expect("send allowed prompt");
    let waiting = wait_for(&allowed, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::WaitingPermission
            && !snapshot.permission_requests.is_empty()
    });
    let allowed_request = waiting.permission_requests[0].clone();
    assert!(
        allowed_request
            .allowed_decisions
            .contains(&AgentPermissionDecision::AllowOnce)
    );
    assert!(
        allowed_request
            .allowed_decisions
            .contains(&AgentPermissionDecision::AllowTask)
    );
    allowed
        .respond_permission(
            allowed_request.request_id,
            AgentPermissionDecision::AllowOnce,
        )
        .expect("allow declared write");
    let snapshot = wait_for(&allowed, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::Completed
    });
    assert!(snapshot.output.contains("permission selected: allow-once"));
    allowed.shutdown();
}

#[test]
fn only_allow_always_does_not_enable_allow_once() {
    let runtime = start(permission_profile(
        "permission-always",
        PermissionPolicy::AgentManaged,
        AgentCapabilities::default(),
    ));
    runtime
        .send_prompt("fixture prompt".to_string(), Vec::new())
        .expect("send prompt");
    let waiting = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::WaitingPermission
            && !snapshot.permission_requests.is_empty()
    });
    let request = waiting.permission_requests[0].clone();
    assert!(
        !request
            .allowed_decisions
            .contains(&AgentPermissionDecision::AllowOnce)
    );
    assert!(
        request
            .allowed_decisions
            .contains(&AgentPermissionDecision::AllowTask)
    );
    assert!(
        runtime
            .respond_permission(
                request.request_id.clone(),
                AgentPermissionDecision::AllowOnce,
            )
            .is_err()
    );
    assert_eq!(
        runtime.snapshot().state,
        AgentRuntimeState::WaitingPermission
    );

    runtime
        .respond_permission(request.request_id, AgentPermissionDecision::AllowTask)
        .expect("allow always");
    let snapshot = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::Completed
    });
    assert!(
        snapshot
            .output
            .contains("permission selected: allow-always")
    );
    runtime.shutdown();
}

#[test]
fn repeated_permission_requests_are_checked_independently() {
    let runtime = start(permission_profile(
        "permission-repeat",
        PermissionPolicy::ReadOnly,
        AgentCapabilities::default(),
    ));
    runtime
        .send_prompt("fixture prompt".to_string(), Vec::new())
        .expect("send prompt");
    let first = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::WaitingPermission
            && snapshot
                .permission_requests
                .iter()
                .any(|request| request.request_id == "900")
    });
    let first_request = first.permission_requests[0].clone();
    assert!(
        !first_request
            .allowed_decisions
            .contains(&AgentPermissionDecision::AllowOnce)
    );
    runtime
        .respond_permission(
            first_request.request_id,
            AgentPermissionDecision::RejectOnce,
        )
        .expect("reject first request");

    let second = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::WaitingPermission
            && snapshot
                .permission_requests
                .iter()
                .any(|request| request.request_id == "901")
    });
    let second_request = second.permission_requests[0].clone();
    assert!(
        !second_request
            .allowed_decisions
            .contains(&AgentPermissionDecision::AllowOnce)
    );
    runtime
        .respond_permission(
            second_request.request_id,
            AgentPermissionDecision::RejectOnce,
        )
        .expect("reject second request");

    let snapshot = wait_for(&runtime, TURN_TIMEOUT, |snapshot| {
        snapshot.state == AgentRuntimeState::Completed
    });
    assert_eq!(
        snapshot.output.matches("permission selected: deny").count(),
        2
    );
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
