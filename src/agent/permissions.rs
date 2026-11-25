use serde::{Deserialize, Serialize};

use super::{AgentCapabilities, PermissionPolicy};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentPermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum AgentPermissionRisk {
    Read,
    Write,
    Execute,
    Network,
    Other,
}

impl AgentPermissionRisk {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Read => "读取",
            Self::Write => "写入文件",
            Self::Execute => "执行命令",
            Self::Network => "网络访问",
            Self::Other => "未识别操作",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum AgentPermissionDecision {
    AllowOnce,
    AllowTask,
    RejectOnce,
    RejectAlways,
    Cancel,
}

impl AgentPermissionDecision {
    /// ACP option kind selected by this decision. There is intentionally no
    /// cross-kind fallback: `allow_once` never becomes `allow_always`.
    pub(crate) const fn option_kind(self) -> Option<&'static str> {
        match self {
            Self::AllowOnce => Some("allow_once"),
            Self::AllowTask => Some("allow_always"),
            Self::RejectOnce => Some("reject_once"),
            Self::RejectAlways => Some("reject_always"),
            Self::Cancel => None,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::AllowOnce => "仅本次允许",
            Self::AllowTask => "始终允许",
            Self::RejectOnce => "仅本次拒绝",
            Self::RejectAlways => "始终拒绝",
            Self::Cancel => "取消任务",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, uniffi::Record)]
pub struct AgentPermissionRequest {
    pub request_id: String,
    pub title: String,
    pub description: String,
    pub kind: String,
    pub risk: AgentPermissionRisk,
    pub options: Vec<AgentPermissionOption>,
    /// Decisions that exactly match an Agent-provided option and are permitted
    /// by the active profile policy. `Cancel` is always present.
    pub allowed_decisions: Vec<AgentPermissionDecision>,
    /// User-visible explanation when Kite blocks an allow decision or when the
    /// permission boundary is owned by the external Agent.
    pub policy_notice: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PermissionEvaluation {
    pub risk: AgentPermissionRisk,
    pub allowed_decisions: Vec<AgentPermissionDecision>,
    pub policy_notice: Option<String>,
}

pub(crate) fn evaluate_permission_request(
    policy: PermissionPolicy,
    capabilities: &AgentCapabilities,
    tool_kind: Option<&str>,
    options: &[AgentPermissionOption],
) -> PermissionEvaluation {
    let risk = classify_permission_risk(tool_kind);
    let has = |kind: &str| options.iter().any(|option| option.kind == kind);
    let block_reason = allow_block_reason(policy, capabilities, risk);
    let allow_enabled = block_reason.is_none();

    let mut allowed_decisions = Vec::new();
    if allow_enabled {
        if has("allow_once") {
            allowed_decisions.push(AgentPermissionDecision::AllowOnce);
        }
        if has("allow_always") {
            allowed_decisions.push(AgentPermissionDecision::AllowTask);
        }
    }
    if has("reject_once") {
        allowed_decisions.push(AgentPermissionDecision::RejectOnce);
    }
    if has("reject_always") {
        allowed_decisions.push(AgentPermissionDecision::RejectAlways);
    }
    allowed_decisions.push(AgentPermissionDecision::Cancel);

    let mut notices = Vec::new();
    if policy == PermissionPolicy::AgentManaged {
        notices.push(
            "Kite 未强制限制此请求；Profile 能力仅作声明，实际权限边界由 Agent 自身管理。"
                .to_string(),
        );
    } else if let Some(reason) = block_reason {
        notices.push(reason);
    } else if has("allow_always") {
        notices.push("“始终允许”由 Agent 按其自身策略解释，可能影响后续同类请求。".to_string());
    }
    if !capabilities.permission_requests {
        notices.push(
            "Profile 声明不请求权限，但 Agent 仍发起了请求；Kite 仍按当前策略处理。".to_string(),
        );
    }

    PermissionEvaluation {
        risk,
        allowed_decisions,
        policy_notice: (!notices.is_empty()).then(|| notices.join(" ")),
    }
}

pub(crate) fn decision_is_allowed(
    request: &AgentPermissionRequest,
    decision: AgentPermissionDecision,
) -> bool {
    request.allowed_decisions.contains(&decision)
}

pub(crate) fn exact_option_id(
    options: &[AgentPermissionOption],
    decision: AgentPermissionDecision,
) -> Option<&str> {
    let option_kind = decision.option_kind()?;
    options
        .iter()
        .find(|option| option.kind == option_kind)
        .map(|option| option.option_id.as_str())
}

pub(crate) fn blocked_decision_message(
    request: &AgentPermissionRequest,
    decision: AgentPermissionDecision,
) -> String {
    if let Some(option_kind) = decision.option_kind()
        && !request
            .options
            .iter()
            .any(|option| option.kind == option_kind)
    {
        return format!(
            "Agent 未提供 {} 对应的 `{option_kind}` 选项，Kite 不会使用其他授权范围替代。",
            decision.label()
        );
    }
    request
        .policy_notice
        .clone()
        .unwrap_or_else(|| format!("当前权限请求不允许{}。", decision.label()))
}

pub(crate) fn classify_permission_risk(tool_kind: Option<&str>) -> AgentPermissionRisk {
    let kind = tool_kind.unwrap_or_default().trim().to_ascii_lowercase();
    match kind.as_str() {
        "read" | "search" | "think" => AgentPermissionRisk::Read,
        "edit" | "delete" | "move" | "write" | "create" => AgentPermissionRisk::Write,
        "execute" | "command" | "terminal" | "shell" => AgentPermissionRisk::Execute,
        "fetch" | "network" | "http" | "https" => AgentPermissionRisk::Network,
        _ => AgentPermissionRisk::Other,
    }
}

fn allow_block_reason(
    policy: PermissionPolicy,
    capabilities: &AgentCapabilities,
    risk: AgentPermissionRisk,
) -> Option<String> {
    match policy {
        PermissionPolicy::AgentManaged => None,
        PermissionPolicy::ReadOnly if risk == AgentPermissionRisk::Read => None,
        PermissionPolicy::ReadOnly => Some(
            "只读策略已禁用允许操作。Kite 不会授权写入文件、执行命令或网络访问；外部 Agent 仍可能在其自身权限范围内运行。"
                .to_string(),
        ),
        PermissionPolicy::ConfirmWrites => match risk {
            AgentPermissionRisk::Read => None,
            AgentPermissionRisk::Write if capabilities.write_files => None,
            AgentPermissionRisk::Write => Some(
                "Profile 未声明“写入文件”能力，Kite 已禁用写入授权。".to_string(),
            ),
            AgentPermissionRisk::Execute if capabilities.execute_commands => None,
            AgentPermissionRisk::Execute => Some(
                "Profile 未声明“执行命令”能力，Kite 已禁用命令授权。".to_string(),
            ),
            AgentPermissionRisk::Network if capabilities.network => None,
            AgentPermissionRisk::Network => Some(
                "Profile 未声明“网络访问”能力，Kite 已禁用网络授权。".to_string(),
            ),
            AgentPermissionRisk::Other => Some(
                "Kite 无法识别该工具调用的风险类型，已禁用允许操作。".to_string(),
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(option_id: &str, kind: &str) -> AgentPermissionOption {
        AgentPermissionOption {
            option_id: option_id.to_string(),
            name: option_id.to_string(),
            kind: kind.to_string(),
        }
    }

    #[test]
    fn read_only_blocks_write_execute_and_network_allowments() {
        let options = [
            option("once", "allow_once"),
            option("always", "allow_always"),
            option("reject", "reject_once"),
        ];

        for kind in ["edit", "execute", "fetch"] {
            let evaluation = evaluate_permission_request(
                PermissionPolicy::ReadOnly,
                &AgentCapabilities::default(),
                Some(kind),
                &options,
            );
            assert!(
                !evaluation
                    .allowed_decisions
                    .contains(&AgentPermissionDecision::AllowOnce)
            );
            assert!(
                !evaluation
                    .allowed_decisions
                    .contains(&AgentPermissionDecision::AllowTask)
            );
            assert!(
                evaluation
                    .allowed_decisions
                    .contains(&AgentPermissionDecision::RejectOnce)
            );
            assert!(
                evaluation
                    .allowed_decisions
                    .contains(&AgentPermissionDecision::Cancel)
            );
            assert!(evaluation.policy_notice.is_some());
        }
    }

    #[test]
    fn read_only_keeps_read_decisions_available() {
        let options = [
            option("once", "allow_once"),
            option("reject", "reject_once"),
        ];
        let evaluation = evaluate_permission_request(
            PermissionPolicy::ReadOnly,
            &AgentCapabilities::default(),
            Some("read"),
            &options,
        );
        assert!(
            evaluation
                .allowed_decisions
                .contains(&AgentPermissionDecision::AllowOnce)
        );
        assert!(evaluation.policy_notice.is_none());
    }

    #[test]
    fn confirm_writes_requires_matching_declared_capability() {
        let options = [
            option("once", "allow_once"),
            option("always", "allow_always"),
            option("reject", "reject_once"),
        ];
        let mut capabilities = AgentCapabilities::default();
        let blocked = evaluate_permission_request(
            PermissionPolicy::ConfirmWrites,
            &capabilities,
            Some("edit"),
            &options,
        );
        assert!(
            !blocked
                .allowed_decisions
                .contains(&AgentPermissionDecision::AllowOnce)
        );

        capabilities.write_files = true;
        let allowed = evaluate_permission_request(
            PermissionPolicy::ConfirmWrites,
            &capabilities,
            Some("edit"),
            &options,
        );
        assert!(
            allowed
                .allowed_decisions
                .contains(&AgentPermissionDecision::AllowOnce)
        );
        assert!(
            allowed
                .allowed_decisions
                .contains(&AgentPermissionDecision::AllowTask)
        );
    }

    #[test]
    fn agent_managed_keeps_exact_options_and_declares_its_boundary() {
        let options = [
            option("once", "allow_once"),
            option("reject-always", "reject_always"),
        ];
        let evaluation = evaluate_permission_request(
            PermissionPolicy::AgentManaged,
            &AgentCapabilities::default(),
            Some("execute"),
            &options,
        );
        assert_eq!(
            evaluation.allowed_decisions,
            vec![
                AgentPermissionDecision::AllowOnce,
                AgentPermissionDecision::RejectAlways,
                AgentPermissionDecision::Cancel,
            ]
        );
        assert!(
            evaluation
                .policy_notice
                .as_deref()
                .is_some_and(|notice| notice.contains("Agent 自身管理"))
        );
    }

    #[test]
    fn allow_once_and_allow_always_never_fall_back_to_each_other() {
        let options = [option("always", "allow_always")];
        let evaluation = evaluate_permission_request(
            PermissionPolicy::AgentManaged,
            &AgentCapabilities::default(),
            Some("read"),
            &options,
        );
        assert!(
            !evaluation
                .allowed_decisions
                .contains(&AgentPermissionDecision::AllowOnce)
        );
        assert_eq!(
            exact_option_id(&options, AgentPermissionDecision::AllowOnce),
            None
        );
        assert_eq!(
            exact_option_id(&options, AgentPermissionDecision::AllowTask),
            Some("always")
        );
    }

    #[test]
    fn request_serializes_exact_decisions_for_the_windows_core_bridge() {
        let request = AgentPermissionRequest {
            request_id: "900".to_string(),
            title: "Fixture write".to_string(),
            description: "写入文件（ACP: edit）".to_string(),
            kind: "edit".to_string(),
            risk: AgentPermissionRisk::Write,
            options: vec![option("always", "allow_always")],
            allowed_decisions: vec![
                AgentPermissionDecision::AllowTask,
                AgentPermissionDecision::Cancel,
            ],
            policy_notice: None,
        };
        let value = serde_json::to_value(request).expect("serialize permission request");
        assert_eq!(value["risk"], "write");
        assert_eq!(value["allowed_decisions"][0], "allow_task");
        assert_eq!(value["allowed_decisions"][1], "cancel");
    }
}
