import Foundation

@main
struct AgentPermissionPresentationSmoke {
    static func main() {
        let options = [
            AgentPermissionOption(
                optionId: "allow-always",
                name: "Always allow",
                kind: "allow_always"
            ),
            AgentPermissionOption(
                optionId: "reject-once",
                name: "Reject once",
                kind: "reject_once"
            ),
        ]
        let request = AgentPermissionRequest(
            requestId: "permission-1",
            title: "Fixture write",
            description: "写入文件（ACP: edit）",
            kind: "edit",
            risk: .write,
            options: options,
            allowedDecisions: [.allowTask, .rejectOnce, .cancel],
            policyNotice: "Exact option test"
        )

        let actions = KiteAgentPermissionPresentation.actions(for: request)
        precondition(actions.count == 5)
        precondition(action("allow-once", in: actions).title == "仅本次允许")
        precondition(!action("allow-once", in: actions).enabled)
        precondition(action("allow-always", in: actions).title == "Always allow")
        precondition(action("allow-always", in: actions).enabled)
        precondition(action("reject-once", in: actions).title == "Reject once")
        precondition(action("reject-once", in: actions).enabled)
        precondition(!action("reject-always", in: actions).enabled)
        precondition(action("cancel", in: actions).enabled)

        let blocked = AgentPermissionRequest(
            requestId: "permission-2",
            title: "Blocked write",
            description: "写入文件（ACP: edit）",
            kind: "edit",
            risk: .write,
            options: options,
            allowedDecisions: [.rejectOnce, .cancel],
            policyNotice: "只读策略已禁用允许操作"
        )
        let blockedActions = KiteAgentPermissionPresentation.actions(for: blocked)
        precondition(!action("allow-once", in: blockedActions).enabled)
        precondition(!action("allow-always", in: blockedActions).enabled)
        precondition(action("reject-once", in: blockedActions).enabled)

        print("AgentPermissionPresentationSmoke passed")
    }

    private static func action(
        _ id: String,
        in actions: [KiteAgentPermissionAction]
    ) -> KiteAgentPermissionAction {
        actions.first { $0.id == id }!
    }
}
