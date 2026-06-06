//! Shared test doubles and builders for the `eos-backend-api` contract/stream
//! tests. Backend store state is real (a temp `backend.db`); the runtime
//! capabilities and agent-core reads are fakes so the routes can be driven in
//! isolation.
#![allow(dead_code)]

use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use axum::Router;
use tempfile::TempDir;

use eos_backend_api::{AgentCoreReads, AppState, RunControl, SandboxRegistry};
use eos_backend_obs::StatsReader;
use eos_backend_runtime::{CancelOutcome, EventBus, LaunchError, SandboxManagerError};
use eos_backend_store::BackendStore;
use eos_backend_types::{CreateUserRequest, SandboxState, SandboxView};
use eos_state::{
    AgentRun, ExecutionTaskOutcome, Page, PageResult, Request, RequestListFilter, RequestStatus,
    Sealed, Task, TaskRole, TaskStatus,
};
use eos_types::{
    AgentRunId, JsonObject, RequestId, SandboxId, TaskId, UtcDateTime,
};

/// A temp-backed [`BackendStore`]; keep the [`TempDir`] alive for the test's life.
pub async fn test_store() -> (BackendStore, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let store = BackendStore::open(dir.path().join("backend.db"))
        .await
        .expect("open store");
    (store, dir)
}

/// Build a router over real store state and the supplied fakes.
pub fn router(
    store: &BackendStore,
    runs: Arc<dyn RunControl>,
    sandboxes: Arc<dyn SandboxRegistry>,
    reads: AgentCoreReads,
) -> Router {
    let event_bus = Arc::new(EventBus::new(store.event_log().clone()));
    let stats = StatsReader::new(store.obs_events().clone(), store.audit_cursors().clone());
    let state = AppState::new(
        runs,
        sandboxes,
        store.run_meta().clone(),
        event_bus,
        store.event_log().clone(),
        stats,
        reads,
    );
    eos_backend_api::build_router(state)
}

/// Agent-core reads backed by the three configurable fakes.
pub fn fake_reads(
    request_status: Option<RequestStatus>,
    tasks: Vec<Task>,
    run: Option<AgentRun>,
) -> AgentCoreReads {
    AgentCoreReads {
        requests: Arc::new(FakeRequestStore { status: request_status }),
        tasks: Arc::new(FakeTaskStore { tasks }),
        agent_runs: Arc::new(FakeAgentRunStore { run }),
    }
}

// --- domain builders -------------------------------------------------------

/// A synthetic agent-core request row in `status`.
pub fn make_request(id: &RequestId, status: RequestStatus) -> Request {
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
        finished_at: status.is_terminal().then_some(now),
    }
}

/// A synthetic generator task owned by `request_id`.
pub fn make_task(id: &TaskId, request_id: &RequestId) -> Task {
    Task {
        id: id.clone(),
        request_id: request_id.clone(),
        role: TaskRole::Generator,
        instruction: "do the thing".to_owned(),
        status: TaskStatus::Done,
        workflow_id: None,
        iteration_id: None,
        attempt_id: None,
        agent_name: Some("planner".to_owned()),
        needs: Vec::new(),
        outcomes: Vec::new(),
        terminal_tool_result: None,
    }
}

/// A synthetic agent run for `task_id` with the given transcript message blocks.
pub fn make_agent_run(task_id: &TaskId, messages: Vec<JsonObject>) -> AgentRun {
    let now = UtcDateTime::now();
    AgentRun {
        id: AgentRunId::new_v4(),
        task_id: task_id.clone(),
        initial_messages: None,
        agent_name: "planner".to_owned(),
        message_history: Some(messages),
        terminal_tool_result: None,
        token_count: 42,
        error: None,
        created_at: now,
        finished_at: Some(now),
    }
}

/// A sanitized sandbox view in `state`.
pub fn make_sandbox_view(id: &SandboxId, state: SandboxState) -> SandboxView {
    let now = UtcDateTime::now();
    SandboxView {
        sandbox_id: id.clone(),
        state,
        owner_request_id: None,
        active_request_ids: Vec::new(),
        ref_count: 0,
        created_at: now,
        last_used_at: now,
        destroy_on_finish: true,
    }
}

// --- runtime-capability fakes ---------------------------------------------

/// Records launches and returns a configurable cancel outcome.
#[derive(Debug)]
pub struct FakeRunControl {
    pub launched: Mutex<Vec<CreateUserRequest>>,
    pub cancel_outcome: CancelOutcome,
    pub launch_id: RequestId,
}

impl FakeRunControl {
    pub fn new(cancel_outcome: CancelOutcome) -> Self {
        Self {
            launched: Mutex::new(Vec::new()),
            cancel_outcome,
            launch_id: RequestId::new_v4(),
        }
    }
}

#[async_trait]
impl RunControl for FakeRunControl {
    async fn launch(&self, request: CreateUserRequest) -> Result<RequestId, LaunchError> {
        self.launched.lock().expect("poisoned").push(request);
        Ok(self.launch_id.clone())
    }

    fn cancel(&self, _request_id: &RequestId, _reason: &str) -> CancelOutcome {
        self.cancel_outcome
    }
}

/// Serves configured sandbox views and a configurable delete result.
#[derive(Debug)]
pub struct FakeSandboxRegistry {
    pub views: Vec<SandboxView>,
    pub delete: Mutex<Option<SandboxManagerError>>,
}

impl FakeSandboxRegistry {
    pub fn new(views: Vec<SandboxView>) -> Self {
        Self {
            views,
            delete: Mutex::new(None),
        }
    }

    /// Configure `delete` to fail with `err`.
    pub fn with_delete_error(views: Vec<SandboxView>, err: SandboxManagerError) -> Self {
        Self {
            views,
            delete: Mutex::new(Some(err)),
        }
    }
}

#[async_trait]
impl SandboxRegistry for FakeSandboxRegistry {
    fn list(&self) -> Vec<SandboxView> {
        self.views.clone()
    }

    fn view(&self, sandbox_id: &SandboxId) -> Option<SandboxView> {
        self.views
            .iter()
            .find(|view| &view.sandbox_id == sandbox_id)
            .cloned()
    }

    async fn delete(&self, _sandbox_id: &SandboxId) -> Result<(), SandboxManagerError> {
        match self.delete.lock().expect("poisoned").take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

// --- agent-core store fakes ------------------------------------------------

/// `RequestStore` whose `get` returns a synthetic row in the configured status.
#[derive(Debug)]
pub struct FakeRequestStore {
    pub status: Option<RequestStatus>,
}

impl Sealed for FakeRequestStore {}

#[async_trait]
impl eos_state::RequestStore for FakeRequestStore {
    async fn create_request(
        &self,
        _request_id: &RequestId,
        _cwd: &str,
        _sandbox_id: Option<&SandboxId>,
        _request_prompt: &str,
    ) -> Result<(), eos_types::CoreError> {
        Ok(())
    }

    async fn get(&self, id: &RequestId) -> Result<Option<Request>, eos_types::CoreError> {
        Ok(self.status.map(|status| make_request(id, status)))
    }

    async fn set_root_task_id(
        &self,
        _id: &RequestId,
        _root_task_id: &TaskId,
    ) -> Result<Request, eos_types::CoreError> {
        unimplemented!("not used by api tests")
    }

    async fn finish_request(
        &self,
        _id: &RequestId,
        _status: RequestStatus,
    ) -> Result<Option<Request>, eos_types::CoreError> {
        unimplemented!("not used by api tests")
    }

    async fn list(
        &self,
        _filter: RequestListFilter,
        _page: Page,
    ) -> Result<PageResult<Request>, eos_types::CoreError> {
        unimplemented!("not used by api tests")
    }
}

/// `TaskStore` serving a fixed task list (and `get` by id within it).
#[derive(Debug)]
pub struct FakeTaskStore {
    pub tasks: Vec<Task>,
}

impl Sealed for FakeTaskStore {}

#[async_trait]
impl eos_state::TaskStore for FakeTaskStore {
    async fn upsert_task(&self, _task: &Task) -> Result<(), eos_types::CoreError> {
        unimplemented!("not used by api tests")
    }

    async fn get(&self, id: &TaskId) -> Result<Option<Task>, eos_types::CoreError> {
        Ok(self.tasks.iter().find(|task| &task.id == id).cloned())
    }

    async fn set_task_status(
        &self,
        _id: &TaskId,
        _status: TaskStatus,
        _outcomes: Option<&[ExecutionTaskOutcome]>,
        _terminal_tool_result: Option<&JsonObject>,
    ) -> Result<Task, eos_types::CoreError> {
        unimplemented!("not used by api tests")
    }

    async fn set_task_status_if_current(
        &self,
        _id: &TaskId,
        _expected: TaskStatus,
        _status: TaskStatus,
        _outcomes: Option<&[ExecutionTaskOutcome]>,
        _terminal_tool_result: Option<&JsonObject>,
    ) -> Result<Option<Task>, eos_types::CoreError> {
        unimplemented!("not used by api tests")
    }

    async fn list_for_request(
        &self,
        _request_id: &RequestId,
    ) -> Result<Vec<Task>, eos_types::CoreError> {
        Ok(self.tasks.clone())
    }
}

/// `AgentRunStore` whose `get_for_task` returns the configured run.
#[derive(Debug)]
pub struct FakeAgentRunStore {
    pub run: Option<AgentRun>,
}

impl Sealed for FakeAgentRunStore {}

#[async_trait]
impl eos_state::AgentRunStore for FakeAgentRunStore {
    async fn create_run(
        &self,
        _agent_run_id: &AgentRunId,
        _task_id: &TaskId,
        _agent_name: &str,
        _initial_messages: Option<&[JsonObject]>,
    ) -> Result<AgentRun, eos_types::CoreError> {
        unimplemented!("not used by api tests")
    }

    async fn finish_run(
        &self,
        _agent_run_id: &AgentRunId,
        _message_history: Option<&[JsonObject]>,
        _terminal_tool_result: Option<&JsonObject>,
        _token_count: i64,
        _error: Option<&str>,
    ) -> Result<Option<AgentRun>, eos_types::CoreError> {
        unimplemented!("not used by api tests")
    }

    async fn get(&self, _agent_run_id: &AgentRunId) -> Result<Option<AgentRun>, eos_types::CoreError> {
        unimplemented!("not used by api tests")
    }

    async fn get_for_task(&self, _task_id: &TaskId) -> Result<Option<AgentRun>, eos_types::CoreError> {
        Ok(self.run.clone())
    }
}
