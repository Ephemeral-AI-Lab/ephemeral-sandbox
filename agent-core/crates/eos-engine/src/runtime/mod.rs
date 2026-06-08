mod advisor;
mod agent_run_service;
mod agent_loop;
mod cancel;
mod control;
mod factory;
mod foreground;
mod persistence;
mod registry;
mod setup;
mod types;

pub use agent_run_service::AgentRunService;
pub(crate) use advisor::run_advisor;
pub use agent_loop::run_agent;
pub use cancel::EngineCancelPort;
pub use control::{AgentRunCancellation, AgentRunControl, AgentRunFinalization};
pub use factory::AgentRunControlFactory;
pub use foreground::{ForegroundExecutor, ForegroundExecutorFactory, ForegroundResourceId};
pub use registry::AgentRunRegistry;
pub use types::{
    AgentRunInput, AgentRunResult, EngineRunHandles, EventCallback, EventSourceFactory,
    ToolRegistryExtender,
};
