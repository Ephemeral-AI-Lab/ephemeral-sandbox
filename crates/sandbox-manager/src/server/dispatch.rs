use std::sync::Arc;

use sandbox_protocol::{OperationScope, SandboxRequest};

use super::{forward::forward_sandbox_request, SandboxManagerServer};

impl SandboxManagerServer {
    pub(super) async fn dispatch_request(&self, request: SandboxRequest) -> serde_json::Value {
        let manager_owned = crate::operation_specs()
            .iter()
            .any(|spec| spec.name == request.op);
        match (&request.scope, manager_owned) {
            (OperationScope::System, true) => self.dispatch_manager_request(request).await,
            (OperationScope::System, false) => {
                sandbox_protocol::SandboxResponse::unknown_op(&request.as_request())
                    .into_json_value()
            }
            (OperationScope::Sandbox { .. }, true) => super::error::error_response(
                sandbox_protocol::error_kind::INVALID_REQUEST,
                format!("manager operation {} requires system scope", request.op),
                serde_json::json!({ "op": request.op }),
            ),
            (OperationScope::Sandbox { .. }, false) => self.forward_sandbox_request(request).await,
        }
    }

    async fn dispatch_manager_request(&self, request: SandboxRequest) -> serde_json::Value {
        let services = Arc::clone(&self.services);
        let op = request.op.clone();
        match tokio::task::spawn_blocking(move || {
            crate::dispatch_operation(&services, request.as_request()).into_json_value()
        })
        .await
        {
            Ok(response) => response,
            Err(error) => super::error::error_response(
                sandbox_protocol::error_kind::INTERNAL_ERROR,
                format!("manager operation task failed: {error}"),
                serde_json::json!({ "op": op }),
            ),
        }
    }

    async fn forward_sandbox_request(&self, request: SandboxRequest) -> serde_json::Value {
        let services = Arc::clone(&self.services);
        let op = request.op.clone();
        match tokio::task::spawn_blocking(move || {
            forward_sandbox_request(&services, request)
                .map(sandbox_protocol::SandboxResponse::into_json_value)
        })
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => error.into_response().into_json_value(),
            Err(error) => super::error::error_response(
                sandbox_protocol::error_kind::INTERNAL_ERROR,
                format!("manager forwarding task failed: {error}"),
                serde_json::json!({ "op": op }),
            ),
        }
    }
}
