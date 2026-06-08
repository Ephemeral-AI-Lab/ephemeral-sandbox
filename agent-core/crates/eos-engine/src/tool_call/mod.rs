//! Post-message tool dispatch.

pub(crate) mod batch;
mod dispatch;
pub(crate) mod execution;
mod hooks;

pub(crate) use batch::{lifecycle_batch_decision, reject_terminal_batch, DispatchCall};
pub use dispatch::{dispatch_assistant_tools, AssistantToolDispatchOutcome, ToolUseRequest};
pub(crate) use execution::execute_tool_once;
