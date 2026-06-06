//! Subagent tools.

mod cancel_subagent;
mod check_subagent_progress;
mod lib;
mod run_subagent;

use std::sync::Arc;

use super::CallerScope;
use crate::ports::BackgroundSupervisorPort;

pub(crate) fn register(
    registry: &mut crate::registry::ToolRegistry,
    config: &crate::registry::config::ToolConfigSet,
    caller: &CallerScope,
    background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
) {
    run_subagent::register(registry, config, caller, background_supervisor.clone());
    check_subagent_progress::register(registry, config, background_supervisor.clone());
    cancel_subagent::register(registry, config, background_supervisor);
}
