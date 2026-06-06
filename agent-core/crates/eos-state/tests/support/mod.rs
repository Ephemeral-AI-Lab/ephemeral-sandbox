//! Shared in-crate test fakes (`#[cfg(test)]` only).
//!
//! `FakeTaskStore` is an in-memory [`TaskStore`] used by the outcome-projection
//! and store-contract tests to prove trait substitutability without sqlx
//! (`test-mock-traits`). It mirrors the Python store semantics for the methods
//! the tests exercise.
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use eos_types::{CoreError, JsonObject, RequestId, TaskId};

use crate::outcomes::ExecutionTaskOutcome;
use crate::store::{Sealed, TaskStore};
use crate::task::{Task, TaskStatus};

/// In-memory [`TaskStore`] fake backed by a `Mutex<HashMap<…>>`.
#[derive(Debug, Default)]
pub(crate) struct FakeTaskStore {
    tasks: Mutex<HashMap<TaskId, Task>>,
}

impl FakeTaskStore {
    /// A fresh empty fake.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Seed a task directly (test helper, not part of the trait).
    pub(crate) fn put(&self, task: Task) {
        self.tasks
            .lock()
            .expect("lock")
            .insert(task.id.clone(), task);
    }
}

/// Apply a status transition plus the two optional projection updates,
/// mirroring the Python store's set-status write shape. Shared by both
/// `set_task_status` and `set_task_status_if_current`.
fn apply_task_updates(
    task: &mut Task,
    status: TaskStatus,
    outcomes: Option<&[ExecutionTaskOutcome]>,
    terminal_tool_result: Option<&JsonObject>,
) {
    task.status = status;
    if let Some(o) = outcomes {
        task.outcomes = o.to_vec();
    }
    if let Some(r) = terminal_tool_result {
        task.terminal_tool_result = Some(r.clone());
    }
}

impl Sealed for FakeTaskStore {}

#[async_trait]
impl TaskStore for FakeTaskStore {
    async fn upsert_task(&self, task: &Task) -> Result<(), CoreError> {
        self.tasks
            .lock()
            .expect("lock")
            .insert(task.id.clone(), task.clone());
        Ok(())
    }

    async fn get(&self, id: &TaskId) -> Result<Option<Task>, CoreError> {
        Ok(self.tasks.lock().expect("lock").get(id).cloned())
    }

    async fn set_task_status(
        &self,
        id: &TaskId,
        status: TaskStatus,
        outcomes: Option<&[ExecutionTaskOutcome]>,
        terminal_tool_result: Option<&JsonObject>,
    ) -> Result<Task, CoreError> {
        let mut guard = self.tasks.lock().expect("lock");
        let task = guard
            .get_mut(id)
            .ok_or_else(|| CoreError::Store(format!("task {id} not found")))?;
        apply_task_updates(task, status, outcomes, terminal_tool_result);
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
        let mut guard = self.tasks.lock().expect("lock");
        let task = guard
            .get_mut(id)
            .ok_or_else(|| CoreError::Store(format!("task {id} not found")))?;
        if task.status != expected {
            return Ok(None);
        }
        apply_task_updates(task, status, outcomes, terminal_tool_result);
        Ok(Some(task.clone()))
    }

    async fn list_for_request(&self, request_id: &RequestId) -> Result<Vec<Task>, CoreError> {
        let mut tasks: Vec<Task> = self
            .tasks
            .lock()
            .expect("lock")
            .values()
            .filter(|task| &task.request_id == request_id)
            .cloned()
            .collect();
        tasks.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        Ok(tasks)
    }
}
