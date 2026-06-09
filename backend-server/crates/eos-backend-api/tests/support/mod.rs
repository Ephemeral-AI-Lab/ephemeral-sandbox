//! Shared test doubles and builders for the `eos-backend-api` contract/stream
//! tests. Backend store state is real (a temp `backend.db`); agent-core service
//! dependencies and sandbox registry are fakes so routes can be driven in
//! isolation.
#![allow(dead_code)]
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::future;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use axum::Router;
use tempfile::TempDir;

use eos_agent_core_server::{
    AgentCoreService, AgentCoreServiceDependencies, AgentCoreServiceSettings,
};
use eos_agent_run::AgentRunService;
use eos_backend_api::{AppState, SandboxRegistry};
use eos_backend_audit::StatsReader;
use eos_backend_runtime::{CancelOutcome, EventBus, SandboxManagerError};
use eos_backend_store::{BackendStore, RunMetaRepo};
use eos_backend_types::{BackendRunStatus, CreateUserRequest, SandboxState, SandboxView};
use eos_engine::records::AgentRunRecordWriter as AgentMessageRecords;
use eos_sandbox_port::{
    DaemonOp, RequestProvisioner, RequestSandboxBinding, SandboxGateway, SandboxPortError,
    SandboxProvisionError, SandboxTransport,
};
use eos_types::{
    format_record_dir, root_task_id, AgentDefinition, AgentLoopCancellation, AgentLoopCompletion,
    AgentLoopLauncher, AgentName, AgentRegistry, AgentRegistryBuilder, AgentRun, AgentRunId,
    AgentRunRecordIndex, AgentRunRecordTarget, AgentType, Attempt, AttemptBudget, AttemptClosure,
    AttemptId, CoreError, CreatedTaskAgentRun, ExecutionTaskOutcome, Iteration,
    IterationCreationReason, IterationId, IterationStatus, JsonObject, MaterializedPlan, Page,
    PageResult, ParentAgentRunAnchor, ParentedAgentRunKind, ParentedRun, Request, RequestId,
    RequestListFilter, RequestStatus, RunningRequestAgentRun, SandboxId, Sealed,
    StartAgentLoopRequest, StartedAgentLoop, Task, TaskAgentRunKind, TaskExecutionIndex, TaskId,
    TaskRole, TaskRun, TaskStatus, ToolUseId, UtcDateTime, Workflow, WorkflowCoordinates, WorkflowId,
    WorkflowNodeId, WorkflowStatus,
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
    runs: Arc<FakeRunControl>,
    sandboxes: Arc<dyn SandboxRegistry>,
    reads: AgentCoreReads,
) -> Router {
    router_with_message_records(
        store,
        runs,
        sandboxes,
        reads,
        AgentMessageRecords::new(std::env::temp_dir().join(format!(
            "eos_backend_api_message_records_{}",
            std::process::id()
        ))),
    )
}

/// Build a router using an explicit message-record service.
pub fn router_with_message_records(
    store: &BackendStore,
    _runs: Arc<FakeRunControl>,
    sandboxes: Arc<dyn SandboxRegistry>,
    reads: AgentCoreReads,
    records: AgentMessageRecords,
) -> Router {
    let event_bus = Arc::new(EventBus::new(store.event_log().clone()));
    let stats = StatsReader::new(store.obs_events().clone(), store.audit_cursors().clone());
    let request_store = Arc::new(FakeRequestStore::new(
        reads.request_status,
        store.run_meta().clone(),
    ));
    let task_store = Arc::new(FakeTaskStore {
        tasks: Mutex::new(reads.tasks.clone()),
    });
    let agent_run_store = Arc::new(FakeAgentRunStore {
        run: Mutex::new(reads.run.clone()),
        created: Mutex::new(HashMap::new()),
    });
    let task_agent_run_store = Arc::new(FakeTaskAgentRunStore {
        tasks: Mutex::new(reads.tasks),
        run: Mutex::new(reads.run),
        indexes: Mutex::new(HashMap::new()),
    });
    let workflow_stores = Arc::new(FakeWorkflowStores);
    let agent_core = AgentCoreService::new(AgentCoreServiceDependencies {
        request_store: request_store.clone(),
        task_store: task_store.clone(),
        agent_run_store: agent_run_store.clone(),
        task_agent_run_store: task_agent_run_store.clone(),
        workflow_store: workflow_stores.clone(),
        iteration_store: workflow_stores.clone(),
        attempt_store: workflow_stores,
        agent_run_service: AgentRunService::new(
            Arc::new(agent_registry()),
            Arc::new(PendingLauncher),
            agent_run_store.clone(),
            task_agent_run_store.clone(),
        ),
        sandbox_gateway: Arc::new(FakeSandboxGateway),
        settings: AgentCoreServiceSettings {
            workspace_root: "/workspace".to_owned(),
            root_agent_name: AgentName::new("root").expect("root name"),
        },
    });
    let state = AppState::new(
        agent_core,
        sandboxes,
        store.run_meta().clone(),
        event_bus,
        store.event_log().clone(),
        stats,
        task_store,
        agent_run_store,
        task_agent_run_store,
        records,
    );
    eos_backend_api::build_router(state)
}

/// Agent-core fake read state.
#[derive(Debug, Clone)]
pub struct AgentCoreReads {
    request_status: Option<RequestStatus>,
    tasks: Vec<Task>,
    run: Option<AgentRun>,
}

/// Agent-core reads backed by configurable fakes.
pub fn fake_reads(
    request_status: Option<RequestStatus>,
    tasks: Vec<Task>,
    run: Option<AgentRun>,
) -> AgentCoreReads {
    AgentCoreReads {
        request_status,
        tasks,
        run,
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
        root_task_id: Some(root_task_id(id)),
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
        terminal_payload: None,
    }
}

/// A synthetic agent run for `task_id`.
pub fn make_agent_run(task_id: &TaskId, _messages: Vec<JsonObject>) -> AgentRun {
    let now = UtcDateTime::now();
    AgentRun {
        id: AgentRunId::new_v4(),
        task_id: Some(task_id.clone()),
        agent_name: "planner".to_owned(),
        terminal_payload: None,
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

/// Compatibility fake retained for tests that still pass a run-control object to
/// the support router.
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
        match lock(&self.delete).take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

#[derive(Debug)]
struct FakeSandboxGateway;

impl SandboxGateway for FakeSandboxGateway {
    fn transport(&self) -> Arc<dyn SandboxTransport> {
        Arc::new(FakeSandboxTransport)
    }

    fn provisioner(&self) -> Arc<dyn RequestProvisioner> {
        Arc::new(FakeProvisioner)
    }
}

#[derive(Debug)]
struct FakeSandboxTransport;

#[async_trait]
impl SandboxTransport for FakeSandboxTransport {
    async fn call(
        &self,
        _sandbox_id: &SandboxId,
        _op: DaemonOp,
        _payload: JsonObject,
        _timeout_s: u32,
    ) -> Result<JsonObject, SandboxPortError> {
        Err(SandboxPortError::transport(None, "fake transport not used"))
    }
}

#[derive(Debug)]
struct FakeProvisioner;

#[async_trait]
impl RequestProvisioner for FakeProvisioner {
    async fn prepare_for_run(
        &self,
        request_id: &RequestId,
        sandbox_id: Option<&str>,
    ) -> Result<RequestSandboxBinding, SandboxProvisionError> {
        let sandbox_id = sandbox_id
            .unwrap_or("sbx-test")
            .parse()
            .map_err(|err: CoreError| SandboxProvisionError::new(err.to_string()))?;
        Ok(RequestSandboxBinding {
            sandbox_id,
            request_id: request_id.clone(),
        })
    }
}

#[derive(Debug)]
struct PendingLauncher;

impl AgentLoopLauncher for PendingLauncher {
    fn start_agent_loop(
        &self,
        _request: StartAgentLoopRequest,
        _agent_run_api: Arc<dyn eos_types::AgentRunApi>,
    ) -> StartedAgentLoop {
        StartedAgentLoop {
            completion: AgentLoopCompletion::new(future::pending()),
            cancellation: Arc::new(TestCancellation),
        }
    }
}

#[derive(Debug)]
struct TestCancellation;

impl AgentLoopCancellation for TestCancellation {
    fn cancel(&self, _reason: &str) {}
}

fn agent_registry() -> AgentRegistry {
    let mut registry = AgentRegistryBuilder::new();
    registry.add(AgentDefinition {
        name: AgentName::new("root").expect("root name"),
        description: "root test agent".to_owned(),
        system_prompt: None,
        model: None,
        tool_call_limit: NonZeroU32::new(4).expect("non-zero"),
        agent_type: AgentType::Agent,
        allowed_tools: Vec::new(),
        terminals: Vec::new(),
        notification_triggers: Vec::new(),
        skill: None,
        context_recipe: None,
    });
    registry.build()
}

// --- agent-core store fakes ------------------------------------------------

#[derive(Debug)]
pub struct FakeRequestStore {
    status: Option<RequestStatus>,
    run_meta: RunMetaRepo,
    created: Mutex<HashMap<RequestId, Request>>,
}

impl FakeRequestStore {
    fn new(status: Option<RequestStatus>, run_meta: RunMetaRepo) -> Self {
        Self {
            status,
            run_meta,
            created: Mutex::new(HashMap::new()),
        }
    }
}

impl Sealed for FakeRequestStore {}

#[async_trait]
impl eos_types::RequestStore for FakeRequestStore {
    async fn create_request(
        &self,
        request_id: &RequestId,
        cwd: &str,
        sandbox_id: Option<&SandboxId>,
        request_prompt: &str,
    ) -> Result<(), CoreError> {
        let now = UtcDateTime::now();
        lock(&self.created).insert(
            request_id.clone(),
            Request {
                id: request_id.clone(),
                cwd: cwd.to_owned(),
                sandbox_id: sandbox_id.cloned(),
                request_prompt: request_prompt.to_owned(),
                root_task_id: None,
                status: RequestStatus::Running,
                created_at: now,
                updated_at: now,
                finished_at: None,
            },
        );
        Ok(())
    }

    async fn get(&self, id: &RequestId) -> Result<Option<Request>, CoreError> {
        if let Some(request) = lock(&self.created).get(id).cloned() {
            return Ok(Some(request));
        }
        if let Some(status) = self.status {
            return Ok(Some(make_request(id, status)));
        }
        Ok(self
            .run_meta
            .get(id)
            .await
            .map_err(|err| CoreError::Store(err.to_string()))?
            .map(|meta| make_request(id, request_status_from_backend(meta.status))))
    }

    async fn set_root_task_id(
        &self,
        id: &RequestId,
        root_task_id: &TaskId,
    ) -> Result<Request, CoreError> {
        let mut created = lock(&self.created);
        let request = created
            .get_mut(id)
            .ok_or_else(|| CoreError::Store(format!("request {id} not found")))?;
        request.root_task_id = Some(root_task_id.clone());
        request.updated_at = UtcDateTime::now();
        Ok(request.clone())
    }

    async fn finish_request(
        &self,
        id: &RequestId,
        status: RequestStatus,
    ) -> Result<Option<Request>, CoreError> {
        if let Some(request) = lock(&self.created).get_mut(id) {
            request.status = status;
            request.updated_at = UtcDateTime::now();
            request.finished_at = status.is_terminal().then_some(UtcDateTime::now());
            return Ok(Some(request.clone()));
        }
        Ok(Some(make_request(id, status)))
    }

    async fn list(
        &self,
        _filter: RequestListFilter,
        page: Page,
    ) -> Result<PageResult<Request>, CoreError> {
        let backend_page = eos_backend_types::Page::new(page.limit, page.offset);
        let page_result = self
            .run_meta
            .list(backend_page)
            .await
            .map_err(|err| CoreError::Store(err.to_string()))?;
        Ok(PageResult {
            items: page_result
                .items
                .into_iter()
                .map(|meta| {
                    let status = self
                        .status
                        .unwrap_or_else(|| request_status_from_backend(meta.status));
                    make_request(&meta.request_id, status)
                })
                .collect(),
            total: page_result.total,
        })
    }
}

#[derive(Debug)]
pub struct FakeTaskStore {
    pub tasks: Mutex<Vec<Task>>,
}

impl Sealed for FakeTaskStore {}

#[async_trait]
impl eos_types::TaskStore for FakeTaskStore {
    async fn insert_task(&self, task: &Task) -> Result<(), CoreError> {
        lock(&self.tasks).push(task.clone());
        Ok(())
    }

    async fn get(&self, id: &TaskId) -> Result<Option<Task>, CoreError> {
        Ok(lock(&self.tasks).iter().find(|task| &task.id == id).cloned())
    }

    async fn set_task_status_if_current(
        &self,
        _id: &TaskId,
        _expected: TaskStatus,
        _status: TaskStatus,
        _outcomes: Option<&[ExecutionTaskOutcome]>,
        _terminal_payload: Option<&JsonObject>,
    ) -> Result<Option<Task>, CoreError> {
        Ok(None)
    }

    async fn latch_attempt_tasks_cancelled(
        &self,
        _attempt_id: &AttemptId,
        _ids: &[TaskId],
    ) -> Result<(), CoreError> {
        Ok(())
    }

    async fn list_for_request(&self, request_id: &RequestId) -> Result<Vec<Task>, CoreError> {
        Ok(lock(&self.tasks)
            .iter()
            .filter(|task| &task.request_id == request_id)
            .cloned()
            .collect())
    }
}

#[derive(Debug)]
pub struct FakeAgentRunStore {
    pub run: Mutex<Option<AgentRun>>,
    pub created: Mutex<HashMap<AgentRunId, AgentRun>>,
}

impl Sealed for FakeAgentRunStore {}

#[async_trait]
impl eos_types::AgentRunStore for FakeAgentRunStore {
    async fn create_run(
        &self,
        agent_run_id: &AgentRunId,
        task_id: Option<&TaskId>,
        agent_name: &str,
    ) -> Result<AgentRun, CoreError> {
        let now = UtcDateTime::now();
        let run = AgentRun {
            id: agent_run_id.clone(),
            task_id: task_id.cloned(),
            agent_name: agent_name.to_owned(),
            terminal_payload: None,
            token_count: 0,
            error: None,
            created_at: now,
            finished_at: None,
        };
        lock(&self.created).insert(agent_run_id.clone(), run.clone());
        Ok(run)
    }

    async fn finish_run(
        &self,
        agent_run_id: &AgentRunId,
        terminal_payload: Option<&JsonObject>,
        token_count: i64,
        error: Option<&str>,
    ) -> Result<Option<AgentRun>, CoreError> {
        let mut created = lock(&self.created);
        let Some(run) = created.get_mut(agent_run_id) else {
            return Ok(None);
        };
        run.terminal_payload = terminal_payload.cloned();
        run.token_count = token_count;
        run.error = error.map(str::to_owned);
        run.finished_at = Some(UtcDateTime::now());
        Ok(Some(run.clone()))
    }

    async fn get(&self, agent_run_id: &AgentRunId) -> Result<Option<AgentRun>, CoreError> {
        Ok(lock(&self.created)
            .get(agent_run_id)
            .cloned()
            .or_else(|| lock(&self.run).as_ref().filter(|run| &run.id == agent_run_id).cloned()))
    }

    async fn get_for_task(&self, task_id: &TaskId) -> Result<Option<AgentRun>, CoreError> {
        Ok(lock(&self.created)
            .values()
            .find(|run| run.task_id.as_ref() == Some(task_id))
            .cloned()
            .or_else(|| {
                lock(&self.run)
                    .as_ref()
                    .filter(|run| run.task_id.as_ref() == Some(task_id))
                    .cloned()
            }))
    }
}

#[derive(Debug)]
pub struct FakeTaskAgentRunStore {
    tasks: Mutex<Vec<Task>>,
    run: Mutex<Option<AgentRun>>,
    indexes: Mutex<HashMap<AgentRunId, AgentRunRecordIndex>>,
}

impl Sealed for FakeTaskAgentRunStore {}

#[async_trait]
impl eos_types::TaskAgentRunStore for FakeTaskAgentRunStore {
    async fn create_root_task_agent_run(
        &self,
        request_id: &RequestId,
        agent_run_id: &AgentRunId,
        _agent_name: &AgentName,
    ) -> Result<CreatedTaskAgentRun, CoreError> {
        let index = AgentRunRecordIndex {
            request_id: request_id.clone(),
            agent_run_id: agent_run_id.clone(),
            task_id: root_task_id(request_id),
            kind: TaskAgentRunKind::Root,
            parent_record_dir: None,
        };
        lock(&self.indexes).insert(agent_run_id.clone(), index.clone());
        Ok(created_from_index(&index))
    }

    async fn create_workflow_task_agent_run(
        &self,
        _request_id: &RequestId,
        _agent_run_id: &AgentRunId,
        _workflow: &WorkflowCoordinates,
        _workflow_node_id: &WorkflowNodeId,
        _agent_name: &AgentName,
    ) -> Result<CreatedTaskAgentRun, CoreError> {
        Err(CoreError::Store("workflow fake not implemented".to_owned()))
    }

    async fn create_parented_task_agent_run(
        &self,
        _agent_run_id: &AgentRunId,
        _parent: &ParentAgentRunAnchor,
        _kind: ParentedAgentRunKind,
        _tool_use_id: Option<&ToolUseId>,
        _agent_name: &AgentName,
    ) -> Result<CreatedTaskAgentRun, CoreError> {
        Err(CoreError::Store("parented fake not implemented".to_owned()))
    }

    async fn finish_task_run(
        &self,
        agent_run_id: &AgentRunId,
        status: TaskStatus,
        terminal_payload: Option<&JsonObject>,
        token_count: i64,
        error: Option<&str>,
    ) -> Result<Option<TaskRun>, CoreError> {
        let Some(index) = lock(&self.indexes).get(agent_run_id).cloned() else {
            return Ok(None);
        };
        Ok(Some(task_run_from_index(
            &index,
            status,
            terminal_payload,
            token_count,
            error,
        )))
    }

    async fn finish_parented_run(
        &self,
        _agent_run_id: &AgentRunId,
        _status: TaskStatus,
        _terminal_payload: Option<&JsonObject>,
        _token_count: i64,
        _error: Option<&str>,
    ) -> Result<Option<ParentedRun>, CoreError> {
        Ok(None)
    }

    async fn record_index_for_agent_run(
        &self,
        agent_run_id: &AgentRunId,
    ) -> Result<Option<AgentRunRecordIndex>, CoreError> {
        if let Some(index) = lock(&self.indexes).get(agent_run_id).cloned() {
            return Ok(Some(index));
        }
        let run = lock(&self.run).clone();
        let task_id = run
            .as_ref()
            .and_then(|run| run.task_id.clone())
            .unwrap_or_else(|| "task-1".parse().expect("task id"));
        let request_id = lock(&self.tasks)
            .iter()
            .find(|task| task.id == task_id)
            .map(|task| task.request_id.clone())
            .unwrap_or_else(|| "req-1".parse().expect("request id"));
        Ok(Some(AgentRunRecordIndex {
            request_id,
            agent_run_id: agent_run_id.clone(),
            task_id,
            kind: TaskAgentRunKind::Root,
            parent_record_dir: None,
        }))
    }

    async fn get_task_run(&self, task_id: &TaskId) -> Result<Option<TaskRun>, CoreError> {
        Ok(lock(&self.tasks)
            .iter()
            .find(|task| &task.id == task_id)
            .map(task_run_from_task))
    }

    async fn list_task_runs_for_request(
        &self,
        request_id: &RequestId,
    ) -> Result<Vec<TaskRun>, CoreError> {
        Ok(lock(&self.tasks)
            .iter()
            .filter(|task| &task.request_id == request_id)
            .map(task_run_from_task)
            .collect())
    }

    async fn list_running_agent_runs_for_request(
        &self,
        request_id: &RequestId,
    ) -> Result<Vec<RunningRequestAgentRun>, CoreError> {
        Ok(lock(&self.indexes)
            .values()
            .filter(|index| &index.request_id == request_id)
            .map(|index| RunningRequestAgentRun {
                request_id: index.request_id.clone(),
                task_id: index.task_id.clone(),
                agent_run_id: index.agent_run_id.clone(),
                status: TaskStatus::Running,
            })
            .collect())
    }

    async fn list_parented_runs_for_parent_task(
        &self,
        _parent_task_id: &TaskId,
        _kind: ParentedAgentRunKind,
    ) -> Result<Vec<ParentedRun>, CoreError> {
        Ok(Vec::new())
    }

    async fn task_execution_index(
        &self,
        _task_id: &TaskId,
    ) -> Result<Option<TaskExecutionIndex>, CoreError> {
        Ok(None)
    }
}

#[derive(Debug)]
struct FakeWorkflowStores;

impl Sealed for FakeWorkflowStores {}

#[async_trait]
impl eos_types::WorkflowStore for FakeWorkflowStores {
    async fn insert(
        &self,
        _request_id: &RequestId,
        _parent_task_id: &TaskId,
        _launched_by_agent_run_id: &AgentRunId,
        _tool_use_id: Option<&ToolUseId>,
        _workflow_goal: &str,
    ) -> Result<Workflow, CoreError> {
        Err(CoreError::Store("workflow fake not implemented".to_owned()))
    }

    async fn get(&self, _id: &WorkflowId) -> Result<Option<Workflow>, CoreError> {
        Ok(None)
    }

    async fn append_iteration_id(
        &self,
        _id: &WorkflowId,
        _iteration_id: &IterationId,
    ) -> Result<Workflow, CoreError> {
        Err(CoreError::Store("workflow fake not implemented".to_owned()))
    }

    async fn set_status(
        &self,
        _id: &WorkflowId,
        _status: WorkflowStatus,
        _closed_at: Option<UtcDateTime>,
        _outcomes: Option<&str>,
    ) -> Result<Workflow, CoreError> {
        Err(CoreError::Store("workflow fake not implemented".to_owned()))
    }

    async fn list_for_parent_task(&self, _parent_task_id: &TaskId) -> Result<Vec<Workflow>, CoreError> {
        Ok(Vec::new())
    }

    async fn list_for_launching_agent_run(
        &self,
        _launched_by_agent_run_id: &AgentRunId,
    ) -> Result<Vec<Workflow>, CoreError> {
        Ok(Vec::new())
    }

    async fn cancel_open_workflows_for_request(
        &self,
        _request_id: &RequestId,
        _reason: &str,
    ) -> Result<usize, CoreError> {
        Ok(0)
    }
}

#[async_trait]
impl eos_types::IterationStore for FakeWorkflowStores {
    async fn insert(
        &self,
        _workflow_id: &WorkflowId,
        _sequence_no: i64,
        _creation_reason: IterationCreationReason,
        _iteration_goal: &str,
        _attempt_budget: AttemptBudget,
    ) -> Result<Iteration, CoreError> {
        Err(CoreError::Store("iteration fake not implemented".to_owned()))
    }

    async fn get(&self, _id: &IterationId) -> Result<Option<Iteration>, CoreError> {
        Ok(None)
    }

    async fn append_attempt_id(
        &self,
        _id: &IterationId,
        _attempt_id: &AttemptId,
    ) -> Result<Iteration, CoreError> {
        Err(CoreError::Store("iteration fake not implemented".to_owned()))
    }

    async fn set_status(
        &self,
        _id: &IterationId,
        _status: IterationStatus,
        _closed_at: Option<UtcDateTime>,
        _outcomes: Option<&str>,
    ) -> Result<Iteration, CoreError> {
        Err(CoreError::Store("iteration fake not implemented".to_owned()))
    }

    async fn set_deferred_goal_for_next_iteration(
        &self,
        _id: &IterationId,
        _deferred_goal_for_next_iteration: Option<&eos_types::DeferredGoal>,
    ) -> Result<Iteration, CoreError> {
        Err(CoreError::Store("iteration fake not implemented".to_owned()))
    }

    async fn close_succeeded(
        &self,
        _id: &IterationId,
        _outcomes: &str,
        _closed_at: Option<UtcDateTime>,
    ) -> Result<Iteration, CoreError> {
        Err(CoreError::Store("iteration fake not implemented".to_owned()))
    }

    async fn list_for_workflow(&self, _workflow_id: &WorkflowId) -> Result<Vec<Iteration>, CoreError> {
        Ok(Vec::new())
    }

    async fn cancel_open_iterations_for_request(
        &self,
        _request_id: &RequestId,
        _reason: &str,
    ) -> Result<usize, CoreError> {
        Ok(0)
    }
}

#[async_trait]
impl eos_types::AttemptStore for FakeWorkflowStores {
    async fn insert(
        &self,
        _iteration_id: &IterationId,
        _workflow_id: &WorkflowId,
        _attempt_sequence_no: i64,
    ) -> Result<Attempt, CoreError> {
        Err(CoreError::Store("attempt fake not implemented".to_owned()))
    }

    async fn get(&self, _id: &AttemptId) -> Result<Option<Attempt>, CoreError> {
        Ok(None)
    }

    async fn record_planner_task(
        &self,
        _id: &AttemptId,
        _planner_task_id: &TaskId,
    ) -> Result<Attempt, CoreError> {
        Err(CoreError::Store("attempt fake not implemented".to_owned()))
    }

    async fn record_plan(
        &self,
        _id: &AttemptId,
        _plan: &MaterializedPlan,
    ) -> Result<Attempt, CoreError> {
        Err(CoreError::Store("attempt fake not implemented".to_owned()))
    }

    async fn close(&self, _id: &AttemptId, _closure: AttemptClosure) -> Result<Attempt, CoreError> {
        Err(CoreError::Store("attempt fake not implemented".to_owned()))
    }

    async fn list_for_iteration(&self, _iteration_id: &IterationId) -> Result<Vec<Attempt>, CoreError> {
        Ok(Vec::new())
    }

    async fn cancel_open_attempts_for_request(
        &self,
        _request_id: &RequestId,
        _reason: &str,
    ) -> Result<usize, CoreError> {
        Ok(0)
    }
}

fn created_from_index(index: &AgentRunRecordIndex) -> CreatedTaskAgentRun {
    CreatedTaskAgentRun {
        agent_run_id: index.agent_run_id.clone(),
        task_id: index.task_id.clone(),
        record_target: AgentRunRecordTarget {
            request_id: index.request_id.clone(),
            agent_run_id: index.agent_run_id.clone(),
            task_id: index.task_id.clone(),
            task_agent_run_kind: index.kind.clone(),
            record_dir: format_record_dir(index),
        },
    }
}

fn task_run_from_task(task: &Task) -> TaskRun {
    TaskRun {
        task_id: task.id.clone(),
        agent_run_id: "run-1".parse().expect("run id"),
        request_id: task.request_id.clone(),
        role: task.role,
        status: task.status,
        workflow_id: task.workflow_id.clone(),
        iteration_id: task.iteration_id.clone(),
        attempt_id: task.attempt_id.clone(),
        agent_name: AgentName::new(task.agent_name.as_deref().unwrap_or("root")).expect("agent"),
        terminal_payload: task.terminal_payload.clone(),
        token_count: 0,
        error: None,
        created_at: UtcDateTime::now(),
        updated_at: UtcDateTime::now(),
        finished_at: task.status.is_terminal_generator().then_some(UtcDateTime::now()),
    }
}

fn task_run_from_index(
    index: &AgentRunRecordIndex,
    status: TaskStatus,
    terminal_payload: Option<&JsonObject>,
    token_count: i64,
    error: Option<&str>,
) -> TaskRun {
    TaskRun {
        task_id: index.task_id.clone(),
        agent_run_id: index.agent_run_id.clone(),
        request_id: index.request_id.clone(),
        role: TaskRole::Root,
        status,
        workflow_id: None,
        iteration_id: None,
        attempt_id: None,
        agent_name: AgentName::new("root").expect("root"),
        terminal_payload: terminal_payload.cloned(),
        token_count,
        error: error.map(str::to_owned),
        created_at: UtcDateTime::now(),
        updated_at: UtcDateTime::now(),
        finished_at: status.is_terminal_generator().then_some(UtcDateTime::now()),
    }
}

fn request_status_from_backend(status: BackendRunStatus) -> RequestStatus {
    match status {
        BackendRunStatus::Accepted | BackendRunStatus::Running => RequestStatus::Running,
        BackendRunStatus::Done => RequestStatus::Done,
        BackendRunStatus::Failed => RequestStatus::Failed,
        BackendRunStatus::Cancelled => RequestStatus::Cancelled,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
