import Foundation

struct KiteAgentPermissionAction: Identifiable {
    let decision: AgentPermissionDecision
    let title: String
    let enabled: Bool
    let isPrimary: Bool

    var id: String {
        switch decision {
        case .allowOnce: return "allow-once"
        case .allowTask: return "allow-always"
        case .rejectOnce: return "reject-once"
        case .rejectAlways: return "reject-always"
        case .cancel: return "cancel"
        }
    }
}

enum KiteAgentPermissionPresentation {
    static func actions(for request: AgentPermissionRequest) -> [KiteAgentPermissionAction] {
        [
            action(
                request: request,
                decision: .allowOnce,
                kind: "allow_once",
                fallback: "仅本次允许",
                isPrimary: true
            ),
            action(
                request: request,
                decision: .allowTask,
                kind: "allow_always",
                fallback: "始终允许",
                isPrimary: false
            ),
            action(
                request: request,
                decision: .rejectOnce,
                kind: "reject_once",
                fallback: "仅本次拒绝",
                isPrimary: false
            ),
            action(
                request: request,
                decision: .rejectAlways,
                kind: "reject_always",
                fallback: "始终拒绝",
                isPrimary: false
            ),
            KiteAgentPermissionAction(
                decision: .cancel,
                title: "取消任务",
                enabled: isAllowed(.cancel, in: request),
                isPrimary: false
            ),
        ]
    }

    static func option(
        in request: AgentPermissionRequest,
        kind: String
    ) -> AgentPermissionOption? {
        request.options.first { $0.kind == kind }
    }

    static func actionLabel(
        for request: AgentPermissionRequest,
        kind: String,
        fallback: String
    ) -> String {
        option(in: request, kind: kind)?.name ?? fallback
    }

    private static func action(
        request: AgentPermissionRequest,
        decision: AgentPermissionDecision,
        kind: String,
        fallback: String,
        isPrimary: Bool
    ) -> KiteAgentPermissionAction {
        KiteAgentPermissionAction(
            decision: decision,
            title: actionLabel(for: request, kind: kind, fallback: fallback),
            enabled: isAllowed(decision, in: request) && option(in: request, kind: kind) != nil,
            isPrimary: isPrimary
        )
    }

    private static func isAllowed(
        _ expected: AgentPermissionDecision,
        in request: AgentPermissionRequest
    ) -> Bool {
        request.allowedDecisions.contains { decision in
            switch (decision, expected) {
            case (.allowOnce, .allowOnce),
                 (.allowTask, .allowTask),
                 (.rejectOnce, .rejectOnce),
                 (.rejectAlways, .rejectAlways),
                 (.cancel, .cancel):
                return true
            default:
                return false
            }
        }
    }
}
