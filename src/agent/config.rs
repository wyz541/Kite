use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use crate::agent::model::{AgentError, AgentErrorKind, AgentSessionConfig};
use crate::agent::redact::{Redactor, is_sensitive_environment_key};
use crate::agent::{AgentCapabilities, AgentEnvironmentVariable, AgentTransport, PermissionPolicy};

#[derive(Clone, Debug)]
pub(crate) struct ValidatedSessionConfig {
    pub transport: AgentTransport,
    pub program: String,
    pub args: Vec<String>,
    pub working_directory: PathBuf,
    pub additional_directories: Vec<PathBuf>,
    pub environment: BTreeMap<String, String>,
    pub startup_timeout: Duration,
    pub turn_timeout: Duration,
    pub idle_timeout: Option<Duration>,
    pub shutdown_timeout: Duration,
    pub max_frame_bytes: usize,
    pub max_output_bytes: u64,
    pub permission_policy: PermissionPolicy,
    pub capabilities: AgentCapabilities,
    pub redactor: Arc<Redactor>,
}

impl ValidatedSessionConfig {
    pub(crate) fn from_public(config: AgentSessionConfig) -> Result<Self, AgentError> {
        let program = config.program.trim().to_string();
        if program.is_empty() {
            return Err(AgentError::invalid_configuration(
                "Agent 可执行文件不能为空。",
            ));
        }
        if program.contains('\0') {
            return Err(AgentError::invalid_configuration(
                "Agent 可执行文件路径包含无效字符。",
            ));
        }
        if config.args.iter().any(|arg| arg.contains('\0')) {
            return Err(AgentError::invalid_configuration(
                "Agent 启动参数包含无效字符。",
            ));
        }

        let working_directory = canonical_directory(&config.working_directory, "工作目录")?;
        let mut additional_directories = Vec::with_capacity(config.additional_directories.len());
        for directory in &config.additional_directories {
            let canonical = canonical_directory(directory, "附加工作目录")?;
            if !additional_directories.contains(&canonical) {
                additional_directories.push(canonical);
            }
        }

        let environment = validate_environment(&config.environment)?;
        let secret_values = environment
            .iter()
            .filter(|(key, _)| is_sensitive_environment_key(key))
            .map(|(_, value)| value.clone())
            .collect::<Vec<_>>();
        let redactor = Arc::new(Redactor::new(secret_values));

        if config.startup_timeout_ms == 0 {
            return Err(AgentError::invalid_configuration(
                "Agent 启动超时必须大于 0。",
            ));
        }
        if config.turn_timeout_ms == 0 {
            return Err(AgentError::invalid_configuration(
                "Agent 单轮任务超时必须大于 0。",
            ));
        }
        if config.shutdown_timeout_ms == 0 {
            return Err(AgentError::invalid_configuration(
                "Agent 关闭超时必须大于 0。",
            ));
        }
        if config.max_frame_bytes < 1024 {
            return Err(AgentError::invalid_configuration(
                "Agent 单帧输出上限不能小于 1024 字节。",
            ));
        }
        let max_frame_bytes = config.max_frame_bytes as usize;
        if config.max_output_bytes < config.max_frame_bytes as u64 {
            return Err(AgentError::invalid_configuration(
                "Agent 总输出上限不能小于单帧输出上限。",
            ));
        }

        Ok(Self {
            transport: config.transport,
            program,
            args: config.args,
            working_directory,
            additional_directories,
            environment,
            startup_timeout: Duration::from_millis(config.startup_timeout_ms),
            turn_timeout: Duration::from_millis(config.turn_timeout_ms),
            idle_timeout: (config.idle_timeout_ms > 0)
                .then(|| Duration::from_millis(config.idle_timeout_ms)),
            shutdown_timeout: Duration::from_millis(config.shutdown_timeout_ms),
            max_frame_bytes,
            max_output_bytes: config.max_output_bytes,
            permission_policy: config.permission_policy,
            capabilities: config.capabilities,
            redactor,
        })
    }

    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command.current_dir(&self.working_directory);
        command.env_clear();

        for key in inherited_environment_keys() {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        for (key, value) in &self.environment {
            command.env(key, value);
        }

        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        command
    }
}

fn canonical_directory(raw: &str, label: &str) -> Result<PathBuf, AgentError> {
    if raw.trim().is_empty() {
        return Err(AgentError::invalid_configuration(format!(
            "{label}不能为空。"
        )));
    }
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(AgentError::invalid_configuration(format!(
            "{label}必须是绝对路径。"
        )));
    }
    let canonical = dunce::canonicalize(&path).map_err(|error| {
        AgentError::new(
            AgentErrorKind::InvalidConfiguration,
            format!("{label}不存在或不可访问。"),
        )
        .with_detail(error.to_string())
    })?;
    if !canonical.is_dir() {
        return Err(AgentError::invalid_configuration(format!(
            "{label}不是目录。"
        )));
    }
    Ok(canonical)
}

fn validate_environment(
    variables: &[AgentEnvironmentVariable],
) -> Result<BTreeMap<String, String>, AgentError> {
    let mut environment: BTreeMap<String, String> = BTreeMap::new();
    for variable in variables {
        let key = variable.name.trim();
        if key.is_empty() || key.contains('=') || key.contains('\0') {
            return Err(AgentError::invalid_configuration(
                "Agent 环境变量名称无效。",
            ));
        }
        if variable.value.contains('\0') {
            return Err(AgentError::invalid_configuration(format!(
                "Agent 环境变量 {key} 的值包含无效字符。"
            )));
        }
        #[cfg(windows)]
        let duplicate = environment
            .keys()
            .any(|existing| existing.eq_ignore_ascii_case(key));
        #[cfg(not(windows))]
        let duplicate = environment.contains_key(key);
        if duplicate {
            return Err(AgentError::invalid_configuration(format!(
                "Agent 环境变量 {key} 重复。"
            )));
        }
        environment.insert(key.to_string(), variable.value.clone());
    }
    Ok(environment)
}

fn inherited_environment_keys() -> &'static [&'static str] {
    #[cfg(windows)]
    {
        &[
            "PATH",
            "PATHEXT",
            "SystemRoot",
            "WINDIR",
            "COMSPEC",
            "TEMP",
            "TMP",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "USERNAME",
            "USERDOMAIN",
            "HOMEDRIVE",
            "HOMEPATH",
        ]
    }
    #[cfg(unix)]
    {
        &[
            "PATH",
            "HOME",
            "TMPDIR",
            "TMP",
            "TEMP",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TERM",
            "USER",
            "LOGNAME",
            "SHELL",
            "SSH_AUTH_SOCK",
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> AgentSessionConfig {
        AgentSessionConfig {
            program: "agent".to_string(),
            working_directory: std::env::temp_dir().to_string_lossy().into_owned(),
            ..AgentSessionConfig::default()
        }
    }

    #[test]
    fn rejects_relative_working_directory() {
        let mut config = base_config();
        config.working_directory = "relative".to_string();
        let error = ValidatedSessionConfig::from_public(config).unwrap_err();
        assert_eq!(error.kind(), AgentErrorKind::InvalidConfiguration);
    }

    #[test]
    fn rejects_duplicate_environment_keys() {
        let mut config = base_config();
        config.environment = vec![
            AgentEnvironmentVariable {
                name: "TOKEN".into(),
                value: "one".into(),
            },
            AgentEnvironmentVariable {
                name: "TOKEN".into(),
                value: "two".into(),
            },
        ];
        let error = ValidatedSessionConfig::from_public(config).unwrap_err();
        assert_eq!(error.kind(), AgentErrorKind::InvalidConfiguration);
    }

    #[test]
    fn rejects_oversized_frame_relative_to_total() {
        let mut config = base_config();
        config.max_frame_bytes = 1024;
        config.max_output_bytes = 1000;
        let error = ValidatedSessionConfig::from_public(config).unwrap_err();
        assert_eq!(error.kind(), AgentErrorKind::InvalidConfiguration);
    }
}
