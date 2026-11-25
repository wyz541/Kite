use crate::agent::{AgentPermissionDecision, AgentPermissionRequest};

pub fn permission_action_label(
    request: &AgentPermissionRequest,
    kind: &str,
    fallback: &str,
) -> String {
    request
        .options
        .iter()
        .find(|option| option.kind == kind)
        .map(|option| option.name.clone())
        .unwrap_or_else(|| fallback.to_string())
}

pub fn permission_action_enabled(
    request: &AgentPermissionRequest,
    decision: AgentPermissionDecision,
    kind: Option<&str>,
) -> bool {
    request.allowed_decisions.contains(&decision)
        && kind.is_none_or(|kind| request.options.iter().any(|option| option.kind == kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentPermissionOption, AgentPermissionRisk};

    fn permission_request() -> AgentPermissionRequest {
        AgentPermissionRequest {
            request_id: "permission-1".to_string(),
            title: "Fixture write".to_string(),
            description: "写入文件（ACP: edit）".to_string(),
            kind: "edit".to_string(),
            risk: AgentPermissionRisk::Write,
            options: vec![
                AgentPermissionOption {
                    option_id: "allow-always".to_string(),
                    name: "Always allow".to_string(),
                    kind: "allow_always".to_string(),
                },
                AgentPermissionOption {
                    option_id: "deny".to_string(),
                    name: "Reject once".to_string(),
                    kind: "reject_once".to_string(),
                },
            ],
            allowed_decisions: vec![
                AgentPermissionDecision::AllowTask,
                AgentPermissionDecision::RejectOnce,
                AgentPermissionDecision::Cancel,
            ],
            policy_notice: Some("Exact option test".to_string()),
        }
    }

    #[test]
    fn permission_buttons_use_exact_options_and_disable_missing_kinds() {
        let request = permission_request();

        assert_eq!(
            permission_action_label(&request, "allow_once", "仅本次允许"),
            "仅本次允许"
        );
        assert_eq!(
            permission_action_label(&request, "allow_always", "始终允许"),
            "Always allow"
        );
        assert!(!permission_action_enabled(
            &request,
            AgentPermissionDecision::AllowOnce,
            Some("allow_once"),
        ));
        assert!(permission_action_enabled(
            &request,
            AgentPermissionDecision::AllowTask,
            Some("allow_always"),
        ));
        assert!(permission_action_enabled(
            &request,
            AgentPermissionDecision::RejectOnce,
            Some("reject_once"),
        ));
        assert!(!permission_action_enabled(
            &request,
            AgentPermissionDecision::RejectAlways,
            Some("reject_always"),
        ));
        assert!(permission_action_enabled(
            &request,
            AgentPermissionDecision::Cancel,
            None,
        ));
    }

    #[test]
    fn policy_blocked_allow_buttons_stay_disabled() {
        let mut request = permission_request();
        request.allowed_decisions = vec![
            AgentPermissionDecision::RejectOnce,
            AgentPermissionDecision::Cancel,
        ];

        assert!(!permission_action_enabled(
            &request,
            AgentPermissionDecision::AllowOnce,
            Some("allow_once"),
        ));
        assert!(!permission_action_enabled(
            &request,
            AgentPermissionDecision::AllowTask,
            Some("allow_always"),
        ));
        assert!(permission_action_enabled(
            &request,
            AgentPermissionDecision::RejectOnce,
            Some("reject_once"),
        ));
    }
}
