//! Shared engine test fakes.

#![allow(clippy::expect_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use eos_sandbox_api::{DaemonOp, SandboxApiError, SandboxCaller, SandboxTransport};
use eos_skills::SkillRegistry;
use eos_state::{ExecutionTaskOutcome, Request, RequestStore, Sealed, Task, TaskStatus, TaskStore};
use eos_tools::ExecutionMetadata;
use eos_types::{CoreError, JsonObject, RequestId, SandboxId, TaskId, UtcDateTime};

#[derive(Debug, Default)]
struct FakeTransport;

#[async_trait]
impl SandboxTransport for FakeTransport {
    async fn call(
        &self,
        _sandbox_id: &SandboxId,
        _op: DaemonOp,
        _payload: JsonObject,
        _timeout_s: u32,
    ) -> Result<JsonObject, SandboxApiError> {
        Ok(JsonObject::new())
    }
}

#[derive(Debug, Default)]
struct FakeTaskStore {
    tasks: Mutex<HashMap<String, Task>>,
}

impl Sealed for FakeTaskStore {}

#[async_trait]
impl TaskStore for FakeTaskStore {
    async fn upsert_task(&self, task: &Task) -> Result<(), CoreError> {
        self.tasks
            .lock()
            .expect("lock")
            .insert(task.id.as_str().to_owned(), task.clone());
        Ok(())
    }

    async fn get(&self, id: &TaskId) -> Result<Option<Task>, CoreError> {
        Ok(self.tasks.lock().expect("lock").get(id.as_str()).cloned())
    }

    async fn set_task_status(
        &self,
        id: &TaskId,
        status: TaskStatus,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        terminal_tool_result: Option<&JsonObject>,
    ) -> Result<Task, CoreError> {
        let mut tasks = self.tasks.lock().expect("lock");
        let task = tasks
            .get_mut(id.as_str())
            .ok_or_else(|| CoreError::Store(format!("task {} not found", id.as_str())))?;
        task.status = status;
        if let Some(outcomes) = outcomes {
            task.outcomes = outcomes.to_vec();
        }
        if let Some(result) = terminal_tool_result {
            task.terminal_tool_result = Some(result.clone());
        }
        Ok(task.clone())
    }

    async fn set_task_status_if_current(
        &self,
        id: &TaskId,
        expected: TaskStatus,
        status: TaskStatus,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        terminal_tool_result: Option<&JsonObject>,
    ) -> Result<Option<Task>, CoreError> {
        let current = self.get(id).await?;
        match current {
            Some(task) if task.status == expected => self
                .set_task_status(id, status, outcomes, terminal_tool_result)
                .await
                .map(Some),
            Some(_) => Ok(None),
            None => Err(CoreError::Store(format!("task {} not found", id.as_str()))),
        }
    }
}

#[derive(Debug, Default)]
struct FakeRequestStore;

impl Sealed for FakeRequestStore {}

fn synthetic_request(id: &RequestId, status: &str) -> Request {
    let now = UtcDateTime::now();
    Request {
        id: id.clone(),
        cwd: String::new(),
        sandbox_id: None,
        request_prompt: String::new(),
        root_task_id: None,
        status: status.to_owned(),
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
        Ok(Some(synthetic_request(id, "running")))
    }

    async fn set_root_task_id(
        &self,
        id: &RequestId,
        _root_task_id: &TaskId,
    ) -> Result<Request, CoreError> {
        Ok(synthetic_request(id, "running"))
    }

    async fn finish_request(
        &self,
        id: &RequestId,
        status: &str,
    ) -> Result<Option<Request>, CoreError> {
        Ok(Some(synthetic_request(id, status)))
    }
}

fn caller() -> SandboxCaller {
    SandboxCaller {
        agent_id: String::new(),
        run_id: String::new(),
        agent_run_id: String::new(),
        task_id: String::new(),
        request_id: String::new(),
        attempt_id: String::new(),
        workflow_id: String::new(),
        tool_id: None,
    }
}

pub(crate) fn metadata() -> ExecutionMetadata {
    ExecutionMetadata {
        sandbox_id: None,
        agent_run_id: None,
        agent_name: "tester".to_owned(),
        cwd: String::new(),
        repo_root: String::new(),
        exec_cwd: String::new(),
        request_id: None,
        task_id: None,
        attempt_id: None,
        workflow_id: None,
        tool_use_id: None,
        sandbox_invocation_id: None,
        caller: caller(),
        transport: Arc::new(FakeTransport),
        task_store: Arc::new(FakeTaskStore::default()),
        request_store: Arc::new(FakeRequestStore),
        skill_registry: Arc::new(SkillRegistry::new()),
        workflow_control: None,
        plan_submission: None,
        background_supervisor: None,
        command_session_supervisor: None,
        isolated_workspace: None,
        notifications: None,
        conversation: Arc::from(Vec::new()),
    }
}
