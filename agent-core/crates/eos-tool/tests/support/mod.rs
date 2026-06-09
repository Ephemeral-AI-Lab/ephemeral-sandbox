//! Shared `#[cfg(test)]` fakes and builders: a configurable [`SandboxTransport`],
//! in-memory `TaskStore`/`RequestStore`, and an [`ExecutionMetadata`] / registry
//! constructor used across the crate's unit tests (`test-mock-traits`).

#![allow(clippy::unwrap_used)]
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use eos_sandbox_port::{DaemonOp, SandboxPortError, SandboxTransport};
use eos_types::{
    AgentRunId, AttemptId, CoreError, JsonObject, RequestId, SandboxId, TaskId, UtcDateTime,
};
use eos_types::{
    ExecutionTaskOutcome, Request, RequestStatus, RequestStore, Sealed, Task, TaskStatus, TaskStore,
};

use crate::ExecutionMetadata;

type Handler = dyn Fn(DaemonOp, &JsonObject) -> Result<JsonObject, SandboxPortError> + Send + Sync;

/// A `SandboxTransport` driven by a closure over `(op, payload)`.
pub(crate) struct FakeTransport {
    handler: Box<Handler>,
}

impl FakeTransport {
    pub(crate) fn new(
        handler: impl Fn(DaemonOp, &JsonObject) -> Result<JsonObject, SandboxPortError>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            handler: Box::new(handler),
        }
    }

    /// A transport that returns an empty object for every op (count→0,
    /// isolated→false, success→false): the inert default.
    pub(crate) fn inert() -> Self {
        Self::new(|_, _| Ok(JsonObject::new()))
    }
}

#[async_trait]
impl SandboxTransport for FakeTransport {
    async fn call(
        &self,
        _sandbox_id: &SandboxId,
        op: DaemonOp,
        payload: JsonObject,
        _timeout_s: u32,
    ) -> Result<JsonObject, SandboxPortError> {
        (self.handler)(op, &payload)
    }
}

/// An in-memory `TaskStore`.
#[derive(Default)]
pub(crate) struct FakeTaskStore {
    tasks: Mutex<HashMap<String, Task>>,
}

impl FakeTaskStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn put(&self, task: Task) {
        self.tasks
            .lock()
            .unwrap()
            .insert(task.id.as_str().to_owned(), task);
    }
}

impl Sealed for FakeTaskStore {}

#[async_trait]
impl TaskStore for FakeTaskStore {
    async fn insert_task(&self, task: &Task) -> Result<(), CoreError> {
        let mut tasks = self.tasks.lock().unwrap();
        if tasks.contains_key(task.id.as_str()) {
            return Err(CoreError::Store(format!(
                "task {} already exists",
                task.id.as_str()
            )));
        }
        tasks.insert(task.id.as_str().to_owned(), task.clone());
        Ok(())
    }

    async fn get(&self, id: &TaskId) -> Result<Option<Task>, CoreError> {
        Ok(self.tasks.lock().unwrap().get(id.as_str()).cloned())
    }

    async fn set_task_status_if_current(
        &self,
        id: &TaskId,
        expected: TaskStatus,
        status: TaskStatus,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        terminal_payload: Option<&JsonObject>,
    ) -> Result<Option<Task>, CoreError> {
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks
            .get_mut(id.as_str())
            .ok_or_else(|| CoreError::Store("task not found".to_owned()))?;
        if task.status != expected {
            return Ok(None);
        }
        task.status = status;
        if let Some(o) = outcomes {
            task.outcomes = o.to_vec();
        }
        if let Some(t) = terminal_payload {
            task.terminal_payload = Some(t.clone());
        }
        Ok(Some(task.clone()))
    }

    async fn latch_attempt_tasks_cancelled(
        &self,
        attempt_id: &AttemptId,
        ids: &[TaskId],
    ) -> Result<(), CoreError> {
        let mut tasks = self.tasks.lock().unwrap();
        let mut terminal = JsonObject::new();
        terminal.insert("fail_reason".to_owned(), "cancelled".into());
        for id in ids {
            if let Some(task) = tasks.get_mut(id.as_str()) {
                if task.attempt_id.as_ref() == Some(attempt_id)
                    && matches!(task.status, TaskStatus::Pending | TaskStatus::Running)
                {
                    task.status = TaskStatus::Cancelled;
                    task.terminal_payload = Some(terminal.clone());
                }
            }
        }
        Ok(())
    }

    async fn list_for_request(&self, request_id: &RequestId) -> Result<Vec<Task>, CoreError> {
        let mut tasks: Vec<Task> = self
            .tasks
            .lock()
            .unwrap()
            .values()
            .filter(|task| &task.request_id == request_id)
            .cloned()
            .collect();
        tasks.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        Ok(tasks)
    }
}

/// An in-memory `RequestStore` that records `finish_request` calls.
#[derive(Default)]
pub(crate) struct FakeRequestStore {
    finished: Mutex<Vec<(String, RequestStatus)>>,
}

impl FakeRequestStore {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn finished(&self) -> Vec<(String, RequestStatus)> {
        self.finished.lock().unwrap().clone()
    }
}

impl Sealed for FakeRequestStore {}

fn synthetic_request(id: &RequestId, status: RequestStatus) -> Request {
    let now = UtcDateTime::now();
    Request {
        id: id.clone(),
        cwd: String::new(),
        sandbox_id: None,
        request_prompt: String::new(),
        root_task_id: None,
        status,
        created_at: now,
        updated_at: now,
        finished_at: Some(now),
    }
}

#[async_trait]
impl RequestStore for FakeRequestStore {
    async fn create_request(
        &self,
        _request_id: &RequestId,
        _cwd: &str,
        _sandbox_id: Option<&SandboxId>,
        _request_prompt: &str,
    ) -> Result<(), CoreError> {
        Ok(())
    }

    async fn get(&self, id: &RequestId) -> Result<Option<Request>, CoreError> {
        Ok(Some(synthetic_request(id, RequestStatus::Running)))
    }

    async fn set_root_task_id(
        &self,
        id: &RequestId,
        _root_task_id: &TaskId,
    ) -> Result<Request, CoreError> {
        Ok(synthetic_request(id, RequestStatus::Running))
    }

    async fn finish_request(
        &self,
        id: &RequestId,
        status: RequestStatus,
    ) -> Result<Option<Request>, CoreError> {
        self.finished
            .lock()
            .unwrap()
            .push((id.as_str().to_owned(), status));
        Ok(Some(synthetic_request(id, status)))
    }

    async fn list(&self) -> Result<Vec<Request>, CoreError> {
        // This fake records only `finish_request`; the list surface is unused by
        // the tool tests, so an empty list is the honest result.
        Ok(Vec::new())
    }
}

pub(crate) fn test_agent_run_id() -> AgentRunId {
    "agent-run-test".parse().expect("agent run id")
}

/// A default [`ExecutionMetadata`] backed by inert fakes (no ports wired).
pub(crate) fn metadata() -> ExecutionMetadata {
    let agent_run_id = test_agent_run_id();
    ExecutionMetadata {
        agent_name: "tester".to_owned(),
        agent_run_id: Some(agent_run_id),
        request_id: None,
        task_id: None,
        attempt_id: None,
        workflow_id: None,
        tool_use_id: None,
        sandbox_invocation_id: None,
        sandbox_id: None,
        is_isolated_workspace_mode: false,
        workspace_root: String::new(),
        conversation: Arc::from(Vec::new()),
    }
}
