//! Phase-8 acceptance: `cancel_agent_core_user_request` reaches the request's
//! live `CancelPort` and marks the request `Cancelled`, idempotently.

use std::sync::Mutex;

use async_trait::async_trait;
use eos_types::{AgentRunId, CancelError, CancelPort, TaskId};

use super::*;
use crate::cancel::cancel_agent_core_user_request;

/// A `CancelPort` fake that records the `cancel_task` ids it receives.
#[derive(Default)]
struct RecordingCancelPort {
    cancelled_tasks: Mutex<Vec<TaskId>>,
}

#[async_trait]
impl CancelPort for RecordingCancelPort {
    async fn cancel_task(&self, task_id: &TaskId, _reason: &str) -> Result<(), CancelError> {
        self.cancelled_tasks.lock().unwrap().push(task_id.clone());
        Ok(())
    }

    async fn cancel_agent_run(
        &self,
        _agent_run_id: &AgentRunId,
        _reason: &str,
    ) -> Result<(), CancelError> {
        Ok(())
    }
}

/// A live request: cancellation reaches the request's port with the root task id
/// and flips the request to `Cancelled`.
#[tokio::test]
async fn cancel_request_reaches_live_port_and_marks_cancelled() {
    let (services, _dir) = build_test_state(None, vec![]).await;
    let request_id: RequestId = "req-cancel".parse().unwrap();
    services
        .db
        .request_store
        .create_request(&request_id, "/tmp", None, "do work")
        .await
        .unwrap();

    let port = std::sync::Arc::new(RecordingCancelPort::default());
    let _guard = services
        .cancel_registry
        .register(request_id.clone(), port.clone());

    let report = cancel_agent_core_user_request(&services, &request_id, "user aborted")
        .await
        .unwrap();

    assert!(report.had_live_run, "a registered port is a live run");
    assert_eq!(
        port.cancelled_tasks.lock().unwrap().as_slice(),
        &[root_task_id_for(&request_id)],
        "cancellation recurses from the root task"
    );
    let request = services
        .db
        .request_store
        .get(&request_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(request.status, RequestStatus::Cancelled);
}

/// An already-finished request (no live port): cancellation is an idempotent
/// no-op that never clobbers a terminal `Done` outcome with `Cancelled`.
#[tokio::test]
async fn cancel_finished_request_is_noop() {
    let (services, _dir) = build_test_state(None, vec![]).await;
    let request_id: RequestId = "req-done".parse().unwrap();
    services
        .db
        .request_store
        .create_request(&request_id, "/tmp", None, "done work")
        .await
        .unwrap();
    services
        .db
        .request_store
        .finish_request(&request_id, RequestStatus::Done)
        .await
        .unwrap();

    let report = cancel_agent_core_user_request(&services, &request_id, "late cancel")
        .await
        .unwrap();

    assert!(
        !report.had_live_run,
        "no port registered for a finished request"
    );
    let request = services
        .db
        .request_store
        .get(&request_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        request.status,
        RequestStatus::Done,
        "finish_request must not clobber a terminal outcome"
    );
}
