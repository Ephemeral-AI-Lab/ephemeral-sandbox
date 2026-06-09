//! Shared application state, the sandbox registry seam, and the axum router.
//!
//! The handlers read backend lifecycle/observability state directly (concrete
//! repositories from `eos-backend-store`/`-audit`) and call the concrete
//! `AgentCoreService` for agent-core request lifecycle operations. The sandbox
//! lifecycle remains behind [`SandboxRegistry`] because tests substitute it for
//! the production `SandboxManager` at this cross-crate boundary.

use std::sync::Arc;

use async_trait::async_trait;
use axum::routing::{get, post};
use axum::Router;

use eos_agent_core_server::AgentCoreService;
use eos_backend_audit::StatsReader;
use eos_backend_runtime::{EventBus, SandboxManager, SandboxManagerError};
use eos_backend_store::{EventLogRepo, RunMetaRepo};
use eos_backend_types::SandboxView;
use eos_engine::records::AgentRunRecordWriter as AgentMessageRecords;
use eos_types::{AgentRunStore, SandboxId, TaskAgentRunStore, TaskStore};

use crate::handlers;

/// Sandbox list/detail/delete capability over sanitized [`SandboxView`]s.
/// Implemented by [`SandboxManager`]; faked in API tests.
#[async_trait]
pub trait SandboxRegistry: Send + Sync {
    /// All tracked sandboxes as sanitized views, newest first.
    fn list(&self) -> Vec<SandboxView>;

    /// The sanitized view of one sandbox, if tracked.
    fn view(&self, sandbox_id: &SandboxId) -> Option<SandboxView>;

    /// Destroy a backend-owned sandbox, refused while it is referenced.
    ///
    /// # Errors
    /// [`SandboxManagerError`] when the sandbox is unknown, referenced, or
    /// teardown fails.
    async fn delete(&self, sandbox_id: &SandboxId) -> Result<(), SandboxManagerError>;
}

#[async_trait]
impl SandboxRegistry for SandboxManager {
    fn list(&self) -> Vec<SandboxView> {
        SandboxManager::list(self)
    }

    fn view(&self, sandbox_id: &SandboxId) -> Option<SandboxView> {
        SandboxManager::view(self, sandbox_id)
    }

    async fn delete(&self, sandbox_id: &SandboxId) -> Result<(), SandboxManagerError> {
        SandboxManager::delete(self, sandbox_id).await
    }
}

/// Shared, cheap-to-clone application state injected into every handler.
#[derive(Clone)]
pub struct AppState {
    pub(crate) agent_core: AgentCoreService,
    pub(crate) sandboxes: Arc<dyn SandboxRegistry>,
    pub(crate) run_meta: RunMetaRepo,
    pub(crate) event_bus: Arc<EventBus>,
    pub(crate) event_log: EventLogRepo,
    pub(crate) stats: StatsReader,
    pub(crate) task_store: Arc<dyn TaskStore>,
    pub(crate) agent_run_store: Arc<dyn AgentRunStore>,
    pub(crate) task_agent_run_store: Arc<dyn TaskAgentRunStore>,
    pub(crate) message_records: AgentMessageRecords,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState").finish_non_exhaustive()
    }
}

impl AppState {
    /// Assemble application state from the injected runtime capabilities and
    /// backend store handles. `event_bus` must be the same instance the launcher
    /// publishes through so the stream routes replay and tail one stream.
    #[must_use]
    pub fn new(
        agent_core: AgentCoreService,
        sandboxes: Arc<dyn SandboxRegistry>,
        run_meta: RunMetaRepo,
        event_bus: Arc<EventBus>,
        event_log: EventLogRepo,
        stats: StatsReader,
        task_store: Arc<dyn TaskStore>,
        agent_run_store: Arc<dyn AgentRunStore>,
        task_agent_run_store: Arc<dyn TaskAgentRunStore>,
        message_records: AgentMessageRecords,
    ) -> Self {
        Self {
            agent_core,
            sandboxes,
            run_meta,
            event_bus,
            event_log,
            stats,
            task_store,
            agent_run_store,
            task_agent_run_store,
            message_records,
        }
    }
}

/// Build the backend HTTP router over [`AppState`]. Resource names are plural and
/// id-segment paths use conventional `/collection/{id}` form (no
/// `/collection={id}` style).
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route(
            "/api/agent-core/requests",
            post(handlers::user_requests::create).get(handlers::user_requests::list),
        )
        .route(
            "/api/agent-core/requests/{request_id}",
            get(handlers::user_requests::detail).delete(handlers::user_requests::cancel),
        )
        .route(
            "/api/agent-core/requests/{request_id}/events",
            get(handlers::user_requests::events),
        )
        .route(
            "/api/agent-core/requests/{request_id}/stream",
            get(handlers::stream::stream),
        )
        .route(
            "/api/agent-core/requests/{request_id}/tasks",
            get(handlers::tasks::request_tasks),
        )
        .route(
            "/api/agent-core/tasks/{task_id}",
            get(handlers::tasks::detail),
        )
        .route(
            "/api/agent-core/tasks/{task_id}/transcript",
            get(handlers::tasks::transcript),
        )
        .route(
            "/api/agent-core/agent-runs/{agent_run_id}/messages",
            get(handlers::agent_runs::messages),
        )
        .route(
            "/api/agent-core/agent-runs/{agent_run_id}/events",
            get(handlers::agent_runs::events),
        )
        .route(
            "/api/agent-core/agent-runs/{agent_run_id}/stream",
            get(handlers::agent_runs::stream),
        )
        .route("/api/stats/performance", get(handlers::stats::performance))
        .route("/api/stats/correctness", get(handlers::stats::correctness))
        .route("/api/stats/agent-runs", get(handlers::stats::agent_runs))
        .route("/api/stats/events", get(handlers::stats::events))
        .route("/api/sandboxes", get(handlers::sandboxes::list))
        .route(
            "/api/sandboxes/{sandbox_id}",
            get(handlers::sandboxes::detail).delete(handlers::sandboxes::delete),
        )
        .route("/openapi.json", get(crate::openapi::openapi_doc))
        .with_state(state)
}
