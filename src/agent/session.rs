use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::agent::config::ValidatedSessionConfig;
use crate::agent::events::{AgentSessionEventSubscription, EventDispatcher};
use crate::agent::model::{
    AgentDiagnosticLevel, AgentError, AgentErrorKind, AgentSessionConfig, AgentSessionEvent,
    AgentSessionEventCallback, AgentSessionState, AgentStopReason,
};
use crate::agent::process::ProcessHandle;
use crate::agent::redact::Redactor;
use crate::agent::transport::{AgentPrompt as TransportPrompt, TransportEvent, transport_for};

/// A non-blocking Rust Core owner for one external ACP Agent process.
///
/// All process and stdout/stderr work happens off the calling thread. Subscribe
/// before calling `start` so readiness and early failures are observable.
#[derive(uniffi::Object)]
pub struct AgentSession {
    config: Arc<ValidatedSessionConfig>,
    state: Arc<Mutex<AgentSessionState>>,
    dispatcher: Arc<EventDispatcher>,
    command_tx: Sender<SessionCommand>,
    process_slot: Arc<OnceLock<Arc<ProcessHandle>>>,
    started: AtomicBool,
    closed: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

enum SessionCommand {
    Start,
    Prompt(TransportPrompt),
    Cancel,
    RespondPermission {
        request_id: String,
        option_id: Option<String>,
    },
    Shutdown,
}

struct ActiveAgent {
    transport: Box<dyn crate::agent::transport::AgentTransport>,
    session_id: String,
}

enum TurnOutcome {
    Completed {
        stop_reason: AgentStopReason,
        duration_ms: u64,
    },
    Cancelled {
        duration_ms: u64,
        forced: bool,
    },
    Shutdown,
    Failed(AgentError),
}

#[uniffi::export]
impl AgentSession {
    #[uniffi::constructor]
    pub fn new(config: AgentSessionConfig) -> Result<Arc<Self>, AgentError> {
        let config = Arc::new(ValidatedSessionConfig::from_public(config)?);
        let (command_tx, command_rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(AgentSessionState::Idle));
        let dispatcher = EventDispatcher::new();
        let process_slot = Arc::new(OnceLock::new());
        let closed = Arc::new(AtomicBool::new(false));

        let worker_config = config.clone();
        let worker_state = state.clone();
        let worker_dispatcher = dispatcher.clone();
        let worker_process_slot = process_slot.clone();
        let worker_closed = closed.clone();
        let worker = thread::Builder::new()
            .name("kite-agent-session".to_string())
            .spawn(move || {
                run_worker(
                    worker_config,
                    command_rx,
                    worker_dispatcher,
                    worker_state,
                    worker_process_slot,
                    worker_closed,
                );
            })
            .map_err(|error| {
                AgentError::new(AgentErrorKind::Io, "无法启动 Agent 会话线程。")
                    .with_detail(error.to_string())
            })?;

        Ok(Arc::new(Self {
            config,
            state,
            dispatcher,
            command_tx,
            process_slot,
            started: AtomicBool::new(false),
            closed,
            worker: Mutex::new(Some(worker)),
        }))
    }

    pub fn start(&self) -> Result<(), AgentError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(closed_error());
        }
        if self.started.swap(true, Ordering::AcqRel) {
            return Err(AgentError::new(
                AgentErrorKind::InvalidConfiguration,
                "Agent 会话已经启动。",
            ));
        }
        {
            let mut state = lock(&self.state);
            if *state != AgentSessionState::Idle {
                return Err(AgentError::new(
                    AgentErrorKind::InvalidConfiguration,
                    "Agent 会话当前不能启动。",
                ));
            }
            *state = AgentSessionState::Starting;
        }
        self.dispatcher.emit(AgentSessionEvent::StateChanged {
            state: AgentSessionState::Starting,
        });
        if let Err(error) = self.send_command(SessionCommand::Start) {
            set_state(
                &self.state,
                &self.dispatcher,
                &self.closed,
                AgentSessionState::Failed,
            );
            return Err(error);
        }
        Ok(())
    }

    pub fn send_prompt(&self, text: String, context_paths: Vec<String>) -> Result<(), AgentError> {
        let prompt = resolve_prompt(&self.config, text, context_paths)?;
        {
            let mut state = lock(&self.state);
            match *state {
                AgentSessionState::Ready => {
                    *state = AgentSessionState::Running;
                }
                AgentSessionState::Running
                | AgentSessionState::Starting
                | AgentSessionState::Cancelling => {
                    return Err(AgentError::new(
                        AgentErrorKind::Busy,
                        "Agent 正在执行其他任务。",
                    ));
                }
                AgentSessionState::Idle => {
                    return Err(AgentError::new(
                        AgentErrorKind::InvalidConfiguration,
                        "请先启动 Agent 会话。",
                    ));
                }
                AgentSessionState::Closed | AgentSessionState::Failed => {
                    return Err(closed_error());
                }
            }
        }
        self.dispatcher.emit(AgentSessionEvent::StateChanged {
            state: AgentSessionState::Running,
        });
        if let Err(error) = self.send_command(SessionCommand::Prompt(prompt)) {
            set_state(
                &self.state,
                &self.dispatcher,
                &self.closed,
                AgentSessionState::Failed,
            );
            return Err(error);
        }
        Ok(())
    }

    pub fn respond_permission(
        &self,
        request_id: String,
        option_id: Option<String>,
    ) -> Result<(), AgentError> {
        let state = *lock(&self.state);
        if !matches!(
            state,
            AgentSessionState::Running | AgentSessionState::Cancelling
        ) {
            return Err(AgentError::new(
                AgentErrorKind::InvalidConfiguration,
                "当前没有等待处理的 Agent 权限请求。",
            ));
        }
        self.send_command(SessionCommand::RespondPermission {
            request_id,
            option_id,
        })
    }

    pub fn cancel(&self) -> Result<(), AgentError> {
        {
            let mut state = lock(&self.state);
            match *state {
                AgentSessionState::Running => {
                    *state = AgentSessionState::Cancelling;
                }
                AgentSessionState::Ready | AgentSessionState::Idle => return Ok(()),
                AgentSessionState::Failed | AgentSessionState::Closed => {
                    return Err(closed_error());
                }
                AgentSessionState::Starting | AgentSessionState::Cancelling => {
                    return Err(AgentError::new(
                        AgentErrorKind::Busy,
                        "Agent 当前正在切换状态。",
                    ));
                }
            }
        }
        self.dispatcher.emit(AgentSessionEvent::StateChanged {
            state: AgentSessionState::Cancelling,
        });
        if let Err(error) = self.send_command(SessionCommand::Cancel) {
            set_state(
                &self.state,
                &self.dispatcher,
                &self.closed,
                AgentSessionState::Failed,
            );
            return Err(error);
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let mut state = lock(&self.state);
            if *state != AgentSessionState::Closed {
                *state = AgentSessionState::Closed;
                drop(state);
                self.dispatcher.emit(AgentSessionEvent::StateChanged {
                    state: AgentSessionState::Closed,
                });
            }
        }
        let _ = self.command_tx.send(SessionCommand::Shutdown);
        if let Some(process) = self.process_slot.get() {
            process.terminate_tree();
        }
    }

    pub fn state(&self) -> AgentSessionState {
        *lock(&self.state)
    }

    pub fn pid(&self) -> Option<u32> {
        self.process_slot.get().map(|process| process.pid())
    }

    pub fn subscribe(
        &self,
        callback: Box<dyn AgentSessionEventCallback>,
    ) -> Arc<AgentSessionEventSubscription> {
        self.dispatcher.subscribe(callback)
    }
}

impl AgentSession {
    fn send_command(&self, command: SessionCommand) -> Result<(), AgentError> {
        self.command_tx
            .send(command)
            .map_err(|_| AgentError::new(AgentErrorKind::Closed, "Agent 会话工作线程已经结束。"))
    }
}

impl Drop for AgentSession {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(worker) = lock(&self.worker).take() {
            let _ = worker.join();
        }
    }
}
fn run_worker(
    config: Arc<ValidatedSessionConfig>,
    command_rx: Receiver<SessionCommand>,
    dispatcher: Arc<EventDispatcher>,
    state: Arc<Mutex<AgentSessionState>>,
    process_slot: Arc<OnceLock<Arc<ProcessHandle>>>,
    closed: Arc<AtomicBool>,
) {
    let mut active: Option<ActiveAgent> = None;
    let redactor = config.redactor.clone();

    loop {
        let command = if active.is_some() {
            match command_rx.recv_timeout(Duration::from_millis(75)) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => Some(SessionCommand::Shutdown),
            }
        } else {
            match command_rx.recv() {
                Ok(command) => Some(command),
                Err(_) => Some(SessionCommand::Shutdown),
            }
        };

        let Some(command) = command else {
            if let Some(agent) = active.as_mut()
                && let Some(status) = agent.transport.try_wait()
            {
                dispatcher.emit(AgentSessionEvent::ProcessExited {
                    exit_code: status.code(),
                });
                let error = process_exit_error(status.code());
                emit_error(&dispatcher, &error, &redactor);
                set_state(&state, &dispatcher, &closed, AgentSessionState::Failed);
                break;
            }
            continue;
        };

        match command {
            SessionCommand::Start => {
                if active.is_some() {
                    emit_diagnostic(
                        &dispatcher,
                        AgentDiagnosticLevel::Warning,
                        "Agent 会话已经启动。",
                        &redactor,
                    );
                    continue;
                }
                if closed.load(Ordering::Acquire) {
                    set_state(&state, &dispatcher, &closed, AgentSessionState::Closed);
                    break;
                }
                match start_agent(&config, &dispatcher, &process_slot) {
                    Ok(agent) => {
                        active = Some(agent);
                        set_state(&state, &dispatcher, &closed, AgentSessionState::Ready);
                    }
                    Err(error) => {
                        emit_error(&dispatcher, &error, &redactor);
                        set_state(&state, &dispatcher, &closed, AgentSessionState::Failed);
                        break;
                    }
                }
            }
            SessionCommand::Prompt(prompt) => {
                let Some(agent) = active.as_mut() else {
                    let error = closed_error();
                    emit_error(&dispatcher, &error, &redactor);
                    set_state(&state, &dispatcher, &closed, AgentSessionState::Failed);
                    break;
                };
                let outcome = run_turn(agent, prompt, &command_rx, &dispatcher, &closed, &config);
                match outcome {
                    TurnOutcome::Completed {
                        stop_reason,
                        duration_ms,
                    } => {
                        dispatcher.emit(AgentSessionEvent::TurnCompleted {
                            stop_reason,
                            duration_ms,
                        });
                        if let Some(status) = agent.transport.try_wait() {
                            dispatcher.emit(AgentSessionEvent::ProcessExited {
                                exit_code: status.code(),
                            });
                            set_state(&state, &dispatcher, &closed, AgentSessionState::Failed);
                        } else {
                            set_state(&state, &dispatcher, &closed, AgentSessionState::Ready);
                        }
                    }
                    TurnOutcome::Cancelled {
                        duration_ms,
                        forced,
                    } => {
                        dispatcher.emit(AgentSessionEvent::TurnCancelled { duration_ms });
                        let exited = agent.transport.try_wait();
                        if let Some(status) = exited {
                            dispatcher.emit(AgentSessionEvent::ProcessExited {
                                exit_code: status.code(),
                            });
                        }
                        if forced || exited.is_some() {
                            set_state(&state, &dispatcher, &closed, AgentSessionState::Failed);
                        } else {
                            set_state(&state, &dispatcher, &closed, AgentSessionState::Ready);
                        }
                    }
                    TurnOutcome::Shutdown => {
                        agent.transport.shutdown();
                        set_state(&state, &dispatcher, &closed, AgentSessionState::Closed);
                        break;
                    }
                    TurnOutcome::Failed(error) => {
                        let terminal = is_terminal(&error);
                        if terminal {
                            agent.transport.shutdown();
                        }
                        emit_error(&dispatcher, &error, &redactor);
                        set_state(
                            &state,
                            &dispatcher,
                            &closed,
                            if terminal {
                                AgentSessionState::Failed
                            } else {
                                AgentSessionState::Ready
                            },
                        );
                    }
                }
            }
            SessionCommand::Cancel => {
                // Cancelling an idle session is intentionally a no-op. A running
                // turn is cancelled inside `run_turn` so the worker can still poll
                // permission responses and the ACP completion response.
            }
            SessionCommand::RespondPermission { .. } => {
                emit_diagnostic(
                    &dispatcher,
                    AgentDiagnosticLevel::Warning,
                    "当前没有等待处理的 Agent 权限请求。",
                    &redactor,
                );
            }
            SessionCommand::Shutdown => {
                if let Some(agent) = active.as_mut() {
                    agent.transport.shutdown();
                }
                set_state(&state, &dispatcher, &closed, AgentSessionState::Closed);
                break;
            }
        }
    }

    if let Some(agent) = active.as_mut() {
        agent.transport.shutdown();
    }
}

fn start_agent(
    config: &ValidatedSessionConfig,
    dispatcher: &Arc<EventDispatcher>,
    process_slot: &Arc<OnceLock<Arc<ProcessHandle>>>,
) -> Result<ActiveAgent, AgentError> {
    let started = Instant::now();
    let mut transport = transport_for(config.transport);
    let handshake = transport.start(
        config,
        dispatcher.clone(),
        process_slot,
        config.startup_timeout,
    )?;
    dispatcher.emit(AgentSessionEvent::ProtocolReady {
        protocol_version: handshake.protocol_version,
        agent_name: handshake.agent_name,
        agent_version: handshake.agent_version,
        capabilities: handshake.capabilities,
    });

    let elapsed = started.elapsed();
    let remaining = config
        .startup_timeout
        .checked_sub(elapsed)
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| AgentError::new(AgentErrorKind::Timeout, "启动 Agent 会话超时。"))?;
    let session_id = transport.create_session(
        &config.working_directory,
        &config.additional_directories,
        remaining,
    )?;
    dispatcher.emit(AgentSessionEvent::SessionCreated {
        protocol_session_id: session_id.clone(),
    });
    Ok(ActiveAgent {
        transport,
        session_id,
    })
}

fn run_turn(
    agent: &mut ActiveAgent,
    prompt: TransportPrompt,
    command_rx: &Receiver<SessionCommand>,
    dispatcher: &Arc<EventDispatcher>,
    closed: &Arc<AtomicBool>,
    config: &ValidatedSessionConfig,
) -> TurnOutcome {
    let started = Instant::now();
    if let Err(error) = agent.transport.send_prompt(&agent.session_id, &prompt) {
        return TurnOutcome::Failed(error);
    }

    let mut pending_permissions = HashSet::new();
    let mut cancel_started: Option<Instant> = None;
    let mut last_activity = Instant::now();
    let deadline = started + config.turn_timeout;

    loop {
        loop {
            match command_rx.try_recv() {
                Ok(SessionCommand::Cancel) => {
                    if cancel_started.is_none() {
                        cancel_started = Some(Instant::now());
                        pending_permissions.clear();
                        if let Err(error) = agent.transport.cancel(&agent.session_id) {
                            emit_diagnostic(
                                dispatcher,
                                AgentDiagnosticLevel::Warning,
                                &format!("发送 Agent 取消请求失败：{}", error.message()),
                                &config.redactor,
                            );
                        }
                    }
                }
                Ok(SessionCommand::RespondPermission {
                    request_id,
                    option_id,
                }) => match agent
                    .transport
                    .respond_permission(&request_id, option_id.as_deref())
                {
                    Ok(true) => {
                        pending_permissions.remove(&request_id);
                        last_activity = Instant::now();
                    }
                    Ok(false) => {
                        pending_permissions.remove(&request_id);
                        emit_diagnostic(
                            dispatcher,
                            AgentDiagnosticLevel::Warning,
                            "Agent 权限请求已失效。",
                            &config.redactor,
                        );
                    }
                    Err(error) => {
                        emit_diagnostic(
                            dispatcher,
                            AgentDiagnosticLevel::Warning,
                            &format!("Agent 权限响应失败：{}", error.message()),
                            &config.redactor,
                        );
                    }
                },
                Ok(SessionCommand::Shutdown) => {
                    agent.transport.shutdown();
                    return TurnOutcome::Shutdown;
                }
                Ok(SessionCommand::Prompt(_) | SessionCommand::Start) => {
                    emit_diagnostic(
                        dispatcher,
                        AgentDiagnosticLevel::Warning,
                        "Agent 正在执行任务，已忽略新的提示词。",
                        &config.redactor,
                    );
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    agent.transport.shutdown();
                    return TurnOutcome::Shutdown;
                }
            }
        }

        let now = Instant::now();
        if let Some(cancel_started) = cancel_started
            && now.saturating_duration_since(cancel_started) >= config.shutdown_timeout
        {
            agent.transport.shutdown();
            return TurnOutcome::Cancelled {
                duration_ms: elapsed_ms(started),
                forced: true,
            };
        }
        if now >= deadline {
            agent.transport.shutdown();
            return TurnOutcome::Failed(AgentError::new(
                AgentErrorKind::Timeout,
                "Agent 单轮任务超时。",
            ));
        }
        if cancel_started.is_none()
            && pending_permissions.is_empty()
            && let Some(idle_timeout) = config.idle_timeout
            && now.saturating_duration_since(last_activity) >= idle_timeout
        {
            agent.transport.shutdown();
            return TurnOutcome::Failed(AgentError::new(
                AgentErrorKind::Timeout,
                "Agent 长时间没有输出。",
            ));
        }

        match agent.transport.wait_for_event(Duration::from_millis(50)) {
            Ok(Some(TransportEvent::Activity)) => {
                last_activity = Instant::now();
            }
            Ok(Some(TransportEvent::PermissionRequested(request))) => {
                pending_permissions.insert(request.request_id.clone());
                last_activity = Instant::now();
                dispatcher.emit(AgentSessionEvent::PermissionRequested { request });
            }
            Ok(Some(TransportEvent::PromptCompleted(stop_reason))) => {
                let duration_ms = elapsed_ms(started);
                if cancel_started.is_some() || stop_reason == AgentStopReason::Cancelled {
                    return TurnOutcome::Cancelled {
                        duration_ms,
                        forced: false,
                    };
                }
                return TurnOutcome::Completed {
                    stop_reason,
                    duration_ms,
                };
            }
            Ok(Some(TransportEvent::ProcessExited(exit_code))) => {
                dispatcher.emit(AgentSessionEvent::ProcessExited { exit_code });
                let error = process_exit_error(exit_code);
                if cancel_started.is_some() {
                    return TurnOutcome::Cancelled {
                        duration_ms: elapsed_ms(started),
                        forced: true,
                    };
                }
                return TurnOutcome::Failed(error);
            }
            Ok(None) => {}
            Err(error) => {
                if cancel_started.is_some()
                    && matches!(
                        error.kind(),
                        AgentErrorKind::ProcessExited | AgentErrorKind::Io
                    )
                {
                    return TurnOutcome::Cancelled {
                        duration_ms: elapsed_ms(started),
                        forced: true,
                    };
                }
                return TurnOutcome::Failed(error);
            }
        }

        if closed.load(Ordering::Acquire) {
            agent.transport.shutdown();
            return TurnOutcome::Shutdown;
        }
    }
}

fn resolve_prompt(
    config: &ValidatedSessionConfig,
    text: String,
    context_paths: Vec<String>,
) -> Result<TransportPrompt, AgentError> {
    if text.trim().is_empty() && context_paths.is_empty() {
        return Err(AgentError::invalid_configuration("Agent 提示词不能为空。"));
    }
    let mut resolved = Vec::new();
    for context_path in context_paths {
        if context_path.trim().is_empty() {
            continue;
        }
        let path = PathBuf::from(&context_path);
        let path = if path.is_absolute() {
            path
        } else {
            config.working_directory.join(path)
        };
        let path = dunce::canonicalize(&path).map_err(|error| {
            AgentError::new(
                AgentErrorKind::InvalidConfiguration,
                "Agent 上下文路径不存在或不可访问。",
            )
            .with_detail(error.to_string())
        })?;
        if !resolved.contains(&path) {
            resolved.push(path);
        }
    }
    Ok(TransportPrompt {
        text,
        context_paths: resolved,
    })
}

fn set_state(
    state: &Arc<Mutex<AgentSessionState>>,
    dispatcher: &Arc<EventDispatcher>,
    closed: &Arc<AtomicBool>,
    next: AgentSessionState,
) -> bool {
    if closed.load(Ordering::Acquire) && next != AgentSessionState::Closed {
        return false;
    }
    let mut state = lock(state);
    let current = *state;
    if current == next {
        return true;
    }
    if !current.can_transition_to(next) {
        return false;
    }
    *state = next;
    drop(state);
    dispatcher.emit(AgentSessionEvent::StateChanged { state: next });
    true
}

fn emit_error(dispatcher: &EventDispatcher, error: &AgentError, redactor: &Redactor) {
    let mut failure = error.as_failure();
    failure.message = redactor.redact(&failure.message);
    failure.detail = failure.detail.map(|detail| redactor.redact(&detail));
    dispatcher.emit(AgentSessionEvent::Error { failure });
}

fn emit_diagnostic(
    dispatcher: &EventDispatcher,
    level: AgentDiagnosticLevel,
    message: &str,
    redactor: &Redactor,
) {
    dispatcher.emit(AgentSessionEvent::Diagnostic {
        level,
        message: redactor.redact(message),
    });
}

fn is_terminal(error: &AgentError) -> bool {
    matches!(
        error.kind(),
        AgentErrorKind::NotInstalled
            | AgentErrorKind::UnsupportedVersion
            | AgentErrorKind::Protocol
            | AgentErrorKind::Timeout
            | AgentErrorKind::OutputLimit
            | AgentErrorKind::ProcessExited
            | AgentErrorKind::Io
            | AgentErrorKind::UnsupportedTransport
    )
}

fn process_exit_error(exit_code: Option<i32>) -> AgentError {
    AgentError::new(
        AgentErrorKind::ProcessExited,
        match exit_code {
            Some(code) => format!("Agent 进程已退出，退出码 {code}。"),
            None => "Agent 进程已退出。".to_string(),
        },
    )
}

fn closed_error() -> AgentError {
    AgentError::new(AgentErrorKind::Closed, "Agent 会话已关闭。")
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
