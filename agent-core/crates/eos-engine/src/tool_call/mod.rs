//! Post-message tool dispatch.

pub(crate) mod batch;
pub(crate) mod execution;
mod hooks;

pub(crate) use batch::{lifecycle_batch_decision, reject_terminal_batch, DispatchCall};
pub(crate) use execution::execute_tool_once;
pub(crate) use hooks::ToolCallHooks;
