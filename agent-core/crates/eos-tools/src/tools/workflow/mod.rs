//! Workflow delegation tools.

mod cancel_workflow;
mod check_workflow_status;
mod delegate_workflow;
mod lib;

use std::sync::Arc;

use crate::ports::{BackgroundSupervisorPort, WorkflowControlPort};

pub(crate) fn register(
    registry: &mut crate::registry::ToolRegistry,
    config: &crate::registry::config::ToolConfigSet,
    workflow_control: Option<Arc<dyn WorkflowControlPort>>,
    background_supervisor: Option<Arc<dyn BackgroundSupervisorPort>>,
) {
    delegate_workflow::register(
        registry,
        config,
        workflow_control.clone(),
        background_supervisor.clone(),
    );
    check_workflow_status::register(registry, config, workflow_control.clone());
    cancel_workflow::register(registry, config, workflow_control, background_supervisor);
}
