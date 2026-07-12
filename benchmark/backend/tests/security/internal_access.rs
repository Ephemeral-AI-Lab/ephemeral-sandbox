use sandbox_benchmark::model::WorkspaceAction;
use sandbox_operation_catalog::internal::runtime::{
    CREATE_WORKSPACE_SESSION, DESTROY_WORKSPACE_SESSION,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InternalRequest {
    action: WorkspaceAction,
}

#[derive(Debug, Default)]
struct ZeroCallBoundary {
    credential_lookups: u32,
    socket_opens: u32,
}

impl ZeroCallBoundary {
    fn invoke(&mut self, value: Value) -> Result<&'static str, serde_json::Error> {
        // The typed action boundary is deliberately evaluated before either
        // side-effect spy. This mirrors the production adapter's ordering.
        let request: InternalRequest = serde_json::from_value(value)?;
        let operation = match request.action {
            WorkspaceAction::CreateNoOpSession => CREATE_WORKSPACE_SESSION,
            WorkspaceAction::DestroySession => DESTROY_WORKSPACE_SESSION,
        };
        self.credential_lookups += 1;
        self.socket_opens += 1;
        Ok(operation)
    }
}

#[test]
fn unknown_internal_actions_are_rejected_before_credentials_or_sockets() {
    for invalid in [
        json!({"action": "future_admin_action"}),
        json!({"action": "exec_command"}),
        json!({"action": "create_no_op_session", "payload": {"command": "sh"}}),
    ] {
        let mut boundary = ZeroCallBoundary::default();
        assert!(boundary.invoke(invalid).is_err());
        assert_eq!(boundary.credential_lookups, 0);
        assert_eq!(boundary.socket_opens, 0);
    }
}

#[test]
fn both_closed_lifecycle_actions_use_the_fixed_product_operations() {
    let mut boundary = ZeroCallBoundary::default();
    assert_eq!(
        boundary
            .invoke(json!({"action": "create_no_op_session"}))
            .expect("known create action"),
        CREATE_WORKSPACE_SESSION
    );
    assert_eq!(
        boundary
            .invoke(json!({"action": "destroy_session"}))
            .expect("known destroy action"),
        DESTROY_WORKSPACE_SESSION
    );
    assert_eq!(boundary.credential_lookups, 2);
    assert_eq!(boundary.socket_opens, 2);
}
