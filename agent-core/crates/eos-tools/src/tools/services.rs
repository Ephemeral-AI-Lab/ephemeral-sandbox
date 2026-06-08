//! Local service structs captured by tool executors and hook wiring.
//!
//! These are intentionally small, family-specific dependency sets. Runtime
//! provider boundaries remain `dyn Trait`; closed groupings stay concrete.

use std::{fmt, sync::Arc};

use async_trait::async_trait;
use eos_sandbox_port::{
    DaemonOp, SandboxCommandApi, SandboxCommandService, SandboxPortError, SandboxTransport,
};
use eos_skills::SkillRegistry;
pub use eos_tool_ports::services::{
    CommandSessionToolService, HookServices, SubagentToolService, WorkflowToolService,
};
use eos_types::{JsonObject, SandboxId};
use eos_types::{RequestStore, TaskStore};

use crate::AttemptSubmissionPort;

/// Store access for the root terminal.
#[derive(Clone)]
pub struct RootSubmissionService {
    pub(crate) task_store: Arc<dyn TaskStore>,
    pub(crate) request_store: Arc<dyn RequestStore>,
}

impl RootSubmissionService {
    /// Build the root-submission service over persisted request/task stores.
    #[must_use]
    pub fn new(task_store: Arc<dyn TaskStore>, request_store: Arc<dyn RequestStore>) -> Self {
        Self {
            task_store,
            request_store,
        }
    }
}

impl fmt::Debug for RootSubmissionService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RootSubmissionService")
            .finish_non_exhaustive()
    }
}

/// Attempt terminal submission behavior.
#[derive(Clone)]
pub struct AttemptSubmissionService {
    pub(crate) port: Arc<dyn AttemptSubmissionPort>,
}

impl AttemptSubmissionService {
    /// Build the attempt-submission service.
    #[must_use]
    pub fn new(port: Arc<dyn AttemptSubmissionPort>) -> Self {
        Self { port }
    }
}

impl fmt::Debug for AttemptSubmissionService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttemptSubmissionService")
            .finish_non_exhaustive()
    }
}

/// Sandbox RPC access for file, shell, plugin, and isolated-workspace tools.
#[derive(Clone)]
pub struct SandboxToolService {
    pub(crate) transport: Arc<dyn SandboxTransport>,
}

impl SandboxToolService {
    /// Build the sandbox tool service over the daemon transport.
    #[must_use]
    pub fn new(transport: Arc<dyn SandboxTransport>) -> Self {
        Self { transport }
    }

    /// Clone the underlying sandbox transport for related service wiring.
    #[must_use]
    pub fn transport(&self) -> Arc<dyn SandboxTransport> {
        self.transport.clone()
    }
}

impl fmt::Debug for SandboxToolService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SandboxToolService").finish_non_exhaustive()
    }
}

/// Command-session tool dependencies.
#[derive(Clone)]
pub struct CommandToolService {
    pub(crate) command_service: Arc<dyn SandboxCommandApi>,
    pub(crate) command_sessions: Option<CommandSessionToolService>,
}

impl CommandToolService {
    /// Build command tool services from the daemon sandbox transport.
    #[must_use]
    pub fn new(
        transport: Arc<dyn SandboxTransport>,
        command_sessions: Option<CommandSessionToolService>,
    ) -> Self {
        Self::with_command_service(
            Arc::new(SandboxCommandService::new(transport)),
            command_sessions,
        )
    }

    /// Build command tool services from the command resource service.
    #[must_use]
    pub fn with_command_service(
        command_service: Arc<dyn SandboxCommandApi>,
        command_sessions: Option<CommandSessionToolService>,
    ) -> Self {
        Self {
            command_service,
            command_sessions,
        }
    }
}

impl fmt::Debug for CommandToolService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandToolService")
            .field("has_command_sessions", &self.command_sessions.is_some())
            .finish_non_exhaustive()
    }
}

/// Skill registry access for skill-reference tools.
#[derive(Clone)]
pub struct SkillToolService {
    pub(crate) skill_registry: Arc<SkillRegistry>,
}

impl SkillToolService {
    /// Build skill tool services.
    #[must_use]
    pub fn new(skill_registry: Arc<SkillRegistry>) -> Self {
        Self { skill_registry }
    }
}

impl fmt::Debug for SkillToolService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SkillToolService").finish_non_exhaustive()
    }
}

/// Inert transport used only when building a static registry without executable
/// services, such as schema snapshots and registry validation.
#[derive(Debug)]
pub(crate) struct InertSandboxTransport;

#[async_trait]
impl SandboxTransport for InertSandboxTransport {
    async fn call(
        &self,
        _sandbox_id: &SandboxId,
        _op: DaemonOp,
        _payload: JsonObject,
        _timeout_s: u32,
    ) -> Result<JsonObject, SandboxPortError> {
        Ok(JsonObject::new())
    }
}
