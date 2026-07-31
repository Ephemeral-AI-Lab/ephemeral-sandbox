use std::time::Duration;

use sandbox_operation_catalog::internal;
use sandbox_operation_catalog::runtime::{
    ATTACH_MPLA_PREPARED_FIXTURE_SPEC, CREATE_MPLA_WORKSPACE_SESSION_SPEC,
    CREATE_WORKSPACE_SESSION_SPEC, DESTROY_WORKSPACE_SESSION_SPEC, EXEC_COMMAND_SPEC,
    FILE_EDIT_SPEC, FILE_WRITE_SPEC, MPLA_STORAGE_ADMIN_SPEC, PUBLISH_MPLA_WORKSPACE_SESSION_SPEC,
    PUBLISH_WORKSPACE_SESSION_SPEC, WRITE_STDIN_SPEC,
};
use sandbox_operation_contract::{OperationRequest, OperationResponse, OperationScope};

use crate::{
    ManagerError, ManagerServices, SandboxDaemonClient, SandboxDaemonEndpoint, SandboxId,
    SandboxState,
};

const MPLA_PUBLICATION_FORWARDING_TIMEOUT: Duration = Duration::from_secs(600);
const DEFAULT_EXEC_YIELD_MS: u64 = 1_000;
const DEFAULT_FORWARDING_TIMEOUT_MS: u64 = 30_000;
const EXEC_FORWARDING_GRACE_MS: u64 = 5_000;
const MAX_EXEC_COMMAND_WAIT_MS: u64 = 600_000;

pub(crate) fn forward_sandbox_request(
    services: &ManagerServices,
    request: OperationRequest,
) -> Result<OperationResponse, ManagerError> {
    let id = sandbox_id(&request.scope)?;
    let endpoint = daemon_endpoint(services, &id)?;
    let operation = request.op.clone();
    let timeout_override = daemon_forwarding_timeout(&request);
    let response = invoke_daemon_with_reply_recovery(
        services.daemon_client.as_ref(),
        &endpoint,
        request,
        timeout_override,
    )?;
    if advances_activity_revision(&response, &operation) {
        services.store.advance_activity_revision(&id)?;
    }
    Ok(response)
}

/// Recover one lost reply only for the MPLA publication whose request id is
/// its durable idempotency key. Other operations preserve normal one-shot
/// forwarding semantics.
fn invoke_daemon_with_reply_recovery(
    daemon_client: &dyn SandboxDaemonClient,
    endpoint: &SandboxDaemonEndpoint,
    request: OperationRequest,
    timeout_override: Option<Duration>,
) -> Result<OperationResponse, ManagerError> {
    let retryable_publication = request.op == PUBLISH_MPLA_WORKSPACE_SESSION_SPEC.name;
    match daemon_client.invoke(endpoint, request.clone(), timeout_override) {
        Ok(response) => Ok(response),
        Err(ManagerError::ForwardingFailed { .. }) if retryable_publication => {
            daemon_client.invoke(endpoint, request, timeout_override)
        }
        Err(error) => Err(error),
    }
}

fn daemon_forwarding_timeout(request: &OperationRequest) -> Option<Duration> {
    if request.op == PUBLISH_MPLA_WORKSPACE_SESSION_SPEC.name {
        return Some(MPLA_PUBLICATION_FORWARDING_TIMEOUT);
    }
    if request.op != EXEC_COMMAND_SPEC.name {
        return None;
    }
    let timeout_ms = request.optional_u64("timeout_ms").ok().flatten()?;
    let yield_time_ms = request
        .optional_u64("yield_time_ms")
        .ok()
        .flatten()
        .unwrap_or(DEFAULT_EXEC_YIELD_MS);
    let synchronous_wait_ms = timeout_ms.min(yield_time_ms).min(MAX_EXEC_COMMAND_WAIT_MS);
    let forwarding_timeout_ms = synchronous_wait_ms.saturating_add(EXEC_FORWARDING_GRACE_MS);
    (forwarding_timeout_ms > DEFAULT_FORWARDING_TIMEOUT_MS)
        .then_some(Duration::from_millis(forwarding_timeout_ms))
}

fn advances_activity_revision(response: &OperationResponse, operation: &str) -> bool {
    if !is_mutation(operation) {
        return false;
    }
    let value = response.as_json_value();
    if value.get("error").is_none() {
        return true;
    }
    operation == PUBLISH_WORKSPACE_SESSION_SPEC.name
        && value["error"]["details"]["stage"].as_str() == Some("destroy")
        && value["error"]["details"]["publish_completed"].as_bool() == Some(true)
}

fn is_mutation(operation: &str) -> bool {
    [
        EXEC_COMMAND_SPEC.name,
        WRITE_STDIN_SPEC.name,
        FILE_WRITE_SPEC.name,
        FILE_EDIT_SPEC.name,
        CREATE_WORKSPACE_SESSION_SPEC.name,
        CREATE_MPLA_WORKSPACE_SESSION_SPEC.name,
        ATTACH_MPLA_PREPARED_FIXTURE_SPEC.name,
        PUBLISH_WORKSPACE_SESSION_SPEC.name,
        DESTROY_WORKSPACE_SESSION_SPEC.name,
        MPLA_STORAGE_ADMIN_SPEC.name,
        internal::runtime::SQUASH_LAYERSTACK,
    ]
    .contains(&operation)
}

fn sandbox_id(scope: &OperationScope) -> Result<SandboxId, ManagerError> {
    match scope {
        OperationScope::Sandbox { sandbox_id } => SandboxId::new(sandbox_id.clone()),
        OperationScope::System => Err(ManagerError::InvalidSandboxId {
            value: "system".to_owned(),
        }),
    }
}

fn daemon_endpoint(
    services: &ManagerServices,
    id: &SandboxId,
) -> Result<SandboxDaemonEndpoint, ManagerError> {
    let record = services.store.inspect(id)?;
    if record.state != SandboxState::Ready {
        return Err(ManagerError::InvalidStateTransition {
            id: id.clone(),
            from: record.state,
            to: SandboxState::Ready,
        });
    }
    record
        .daemon
        .ok_or_else(|| ManagerError::DaemonUnavailable { id: id.clone() })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;

    fn sandbox_request(operation: &str) -> OperationRequest {
        sandbox_request_with_args(operation, json!({}))
    }

    fn sandbox_request_with_args(operation: &str, args: serde_json::Value) -> OperationRequest {
        OperationRequest::new(
            operation,
            "request-1",
            OperationScope::Sandbox {
                sandbox_id: "eos-1".to_owned(),
            },
            args,
        )
    }

    #[test]
    fn long_exec_wait_and_mpla_publication_receive_bounded_forwarding_timeouts() {
        assert_eq!(
            daemon_forwarding_timeout(&sandbox_request(PUBLISH_MPLA_WORKSPACE_SESSION_SPEC.name)),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            daemon_forwarding_timeout(&sandbox_request(EXEC_COMMAND_SPEC.name)),
            None
        );
        assert_eq!(
            daemon_forwarding_timeout(&sandbox_request_with_args(
                EXEC_COMMAND_SPEC.name,
                json!({"timeout_ms": 180_000, "yield_time_ms": 180_000}),
            )),
            Some(Duration::from_secs(185))
        );
        assert_eq!(
            daemon_forwarding_timeout(&sandbox_request_with_args(
                EXEC_COMMAND_SPEC.name,
                json!({"timeout_ms": 180_000, "yield_time_ms": 120_000}),
            )),
            Some(Duration::from_secs(125))
        );
        assert_eq!(
            daemon_forwarding_timeout(&sandbox_request_with_args(
                EXEC_COMMAND_SPEC.name,
                json!({"timeout_ms": 180_000, "yield_time_ms": 0}),
            )),
            None
        );
        assert_eq!(
            daemon_forwarding_timeout(&sandbox_request_with_args(
                EXEC_COMMAND_SPEC.name,
                json!({"timeout_ms": u64::MAX, "yield_time_ms": u64::MAX}),
            )),
            Some(Duration::from_secs(605))
        );
    }

    #[test]
    fn malformed_or_unbounded_exec_requests_keep_default_forwarding_timeout() {
        for args in [
            json!({"yield_time_ms": 180_000}),
            json!({"timeout_ms": "180000", "yield_time_ms": 180_000}),
            json!({"timeout_ms": -1, "yield_time_ms": 180_000}),
            json!({"timeout_ms": 1.5, "yield_time_ms": 180_000}),
            json!({"timeout_ms": 180_000}),
        ] {
            assert_eq!(
                daemon_forwarding_timeout(
                    &sandbox_request_with_args(EXEC_COMMAND_SPEC.name, args,)
                ),
                None
            );
        }
        assert_eq!(
            daemon_forwarding_timeout(&sandbox_request_with_args(
                FILE_WRITE_SPEC.name,
                json!({"timeout_ms": 180_000, "yield_time_ms": 180_000}),
            )),
            None
        );
    }

    struct LostFirstPublishReplyClient {
        request_ids: Mutex<Vec<String>>,
    }

    impl SandboxDaemonClient for LostFirstPublishReplyClient {
        fn invoke(
            &self,
            _endpoint: &SandboxDaemonEndpoint,
            request: OperationRequest,
            _timeout_override: Option<Duration>,
        ) -> Result<OperationResponse, ManagerError> {
            let mut request_ids = self.request_ids.lock().expect("request ids lock");
            request_ids.push(request.request_id);
            if request_ids.len() == 1 {
                return Err(ManagerError::ForwardingFailed {
                    message: "response connection closed before delivery".to_owned(),
                });
            }
            Ok(OperationResponse::ok(json!({"idempotent_replay": true})))
        }
    }

    #[test]
    fn mpla_publish_replays_once_after_a_lost_reply_with_the_same_request_id() {
        let client = LostFirstPublishReplyClient {
            request_ids: Mutex::new(Vec::new()),
        };
        let request = sandbox_request(PUBLISH_MPLA_WORKSPACE_SESSION_SPEC.name);
        let response = invoke_daemon_with_reply_recovery(
            &client,
            &SandboxDaemonEndpoint::new("127.0.0.1", 7000, "token"),
            request,
            Some(MPLA_PUBLICATION_FORWARDING_TIMEOUT),
        )
        .expect("the durable publication replay returns its result");

        assert_eq!(
            response.as_json_value(),
            &json!({"idempotent_replay": true})
        );
        assert_eq!(
            *client.request_ids.lock().expect("request ids lock"),
            vec!["request-1", "request-1"],
            "the retry must replay the exact durable request"
        );
    }

    #[test]
    fn non_mpla_requests_do_not_retry_after_a_lost_reply() {
        let client = LostFirstPublishReplyClient {
            request_ids: Mutex::new(Vec::new()),
        };
        let result = invoke_daemon_with_reply_recovery(
            &client,
            &SandboxDaemonEndpoint::new("127.0.0.1", 7000, "token"),
            sandbox_request(FILE_WRITE_SPEC.name),
            None,
        );

        assert!(matches!(result, Err(ManagerError::ForwardingFailed { .. })));
        assert_eq!(
            client.request_ids.lock().expect("request ids lock").len(),
            1,
            "ordinary mutations preserve at-most-once forwarding"
        );
    }
}
