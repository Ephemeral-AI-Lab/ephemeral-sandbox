//! Runtime composition surface.

mod agent_core_registry;
mod agent_loop;
mod agent_state;
pub mod audit;
mod audit_runtime;
mod builder;
mod cancel_port;
mod cancel_registry;
pub(crate) mod config;
mod db_store;
mod engine;
mod message_records;
mod plugins;
mod sandbox;
mod state_reader;

use eos_agent_run::AgentMessageRecords;

pub(crate) use agent_core_registry::AgentCoreRegistryService;
pub(crate) use agent_loop::build_agent_loop_launcher;
pub(crate) use agent_state::RuntimeAgentStateService;
pub(crate) use audit_runtime::AuditRuntime;
pub use builder::AgentCoreRuntimeBuilder;
pub(crate) use cancel_port::RuntimeAgentCoreCancellation;
pub(crate) use cancel_registry::RequestCancelRegistry;
pub use config::RuntimeConfig;
pub(crate) use db_store::DbStoreService;
pub(crate) use engine::EngineService;
pub(crate) use message_records::MessageRecordService;
pub(crate) use sandbox::SandboxService;
pub use state_reader::StateReader;

// The per-agent provider-stream factory and per-run stream-event callback are owned
// by `eos-engine` (next to the loop they drive, so the engine-driven advisor run
// can resolve a source without a runtime back-edge) and re-exported here for the
// composition root and the `run_request` signature.
pub use eos_engine::{EngineEventSink, ProviderStreamSourceFactory};

/// The runtime composition graph. Request/workspace data is supplied through
/// request-scoped inputs, not stored here.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AgentCoreRuntime {
    pub(crate) db: DbStoreService,
    pub(crate) agent_core: AgentCoreRegistryService,
    pub(crate) engine: EngineService,
    pub(crate) sandbox: SandboxService,
    pub(crate) audit: AuditRuntime,
    pub(crate) message_records: MessageRecordService,
    pub(crate) agent_state: RuntimeAgentStateService,
    /// Per-request cancellation APIs, so `cancel_agent_core_user_request` can
    /// reach a live request's recursive `AgentCoreCancellationApi` from another task.
    pub(crate) cancel_registry: RequestCancelRegistry,
}

impl AgentCoreRuntime {
    /// Start building runtime services.
    pub fn builder() -> AgentCoreRuntimeBuilder {
        AgentCoreRuntimeBuilder::default()
    }

    /// Narrow read-side store handles for the backend composition root (spec
    /// §State Reader): the request, task, and agent-run stores only, exposed as
    /// typed trait objects — never a `sqlx` pool or the agent-core table layout.
    #[must_use]
    pub fn state_reader(&self) -> StateReader {
        StateReader::new(
            self.db.request_store.clone(),
            self.db.task_store.clone(),
            self.db.agent_run_store.clone(),
        )
    }

    /// File-backed agent-node message record reader/writer, when configured.
    #[must_use]
    pub fn message_records(&self) -> Option<AgentMessageRecords> {
        self.message_records.message_records.clone()
    }

    /// Flush and join the buffered audit writer thread, if any.
    pub fn flush_audit(&self) {
        if let Ok(mut guard) = self.audit.shutdown.lock() {
            if let Some(shutdown) = guard.take() {
                shutdown.shutdown();
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/support.rs"]
pub(crate) mod support;
