//! Background dispatch helpers.

use eos_tools::{ToolName, ToolResult};
use eos_types::JsonObject;

use crate::background::{BackgroundTaskKind, BackgroundTaskSupervisor};

/// Register a background tool launch with the supervisor and return an immediate
/// model-facing acknowledgement.
#[must_use]
pub fn launch_background_tool(
    supervisor: &mut BackgroundTaskSupervisor,
    name: ToolName,
    input: JsonObject,
) -> ToolResult {
    let task_id = supervisor.register_running(name.as_str(), input, BackgroundTaskKind::Agent);
    ToolResult::ok(format!(
        "Started background task `{}` for `{}`.",
        task_id,
        name.as_str()
    ))
    .meta("background_task_id", serde_json::json!(task_id))
}
