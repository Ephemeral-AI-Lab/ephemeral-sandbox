//! Runtime service composition surface.

mod agent_core_registry;
mod audit;
mod builder;
mod db_store;
mod engine;
mod sandbox;

use std::sync::Arc;

use eos_tools::{RootSubmissionService, SandboxToolService, SkillToolService};

use crate::plugin_tools::register_plugin_tools;

pub(crate) use agent_core_registry::AgentCoreRegistryService;
pub(crate) use audit::AuditService;
pub use builder::RuntimeServicesBuilder;
pub(crate) use db_store::DbStoreService;
pub(crate) use engine::EngineService;
pub(crate) use sandbox::SandboxService;

// The per-agent event-source factory and per-run stream-event callback are owned
// by `eos-engine` (next to the loop they drive, so the engine-driven advisor run
// can resolve a source without a runtime back-edge) and re-exported here for the
// composition root and the `run_request` signature.
pub use eos_engine::{EventCallback, EventSourceFactory};

/// The runtime composition graph. Request/workspace data is supplied through
/// request-scoped inputs, not stored here.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct RuntimeServices {
    pub(crate) db: DbStoreService,
    pub(crate) agent_core: AgentCoreRegistryService,
    pub(crate) engine: EngineService,
    pub(crate) sandbox: SandboxService,
    pub(crate) audit: AuditService,
}

impl RuntimeServices {
    /// Start building runtime services.
    pub fn builder() -> RuntimeServicesBuilder {
        RuntimeServicesBuilder::default()
    }

    /// Bundle the explicit run handles `eos_engine::run_agent` needs.
    pub(crate) fn engine_run_handles(&self, workspace_root: &str) -> eos_engine::EngineRunHandles {
        let sandbox_service = SandboxToolService::new(self.sandbox.transport.clone());
        let plugin_sandbox_service = sandbox_service.clone();
        eos_engine::EngineRunHandles {
            agent_run_store: self.db.agent_run_store.clone(),
            model_store: self.db.model_store.clone(),
            llm_client: self.engine.llm_client.clone(),
            event_source_factory: self.engine.event_source_factory.clone(),
            agent_registry: self.agent_core.agent_registry.clone(),
            tool_config: self.agent_core.tool_config.clone(),
            sandbox_service,
            root_submission: Some(RootSubmissionService::new(
                self.db.task_store.clone(),
                self.db.request_store.clone(),
            )),
            skill_service: SkillToolService::new(self.agent_core.skill_registry.clone()),
            tool_registry_extender: Some(Arc::new(move |registry| {
                register_plugin_tools(registry, &plugin_sandbox_service);
            })),
            audit: self.audit.sink.clone(),
            workspace_root: workspace_root.to_owned(),
        }
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
#[path = "../../tests/unit/support.rs"]
pub(crate) mod support;
