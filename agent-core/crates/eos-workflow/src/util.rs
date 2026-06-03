//! Small crate-internal helpers shared across modules.

use eos_state::JsonObject;
use serde_json::Value;

/// Build a single-key [`JsonObject`] (the `terminal_tool_result` markers the
/// orchestrator / scheduler / cancel paths stamp, e.g. `{"fail_reason": ...}`).
pub(crate) fn json_object(key: &str, value: impl Into<Value>) -> JsonObject {
    let mut object = JsonObject::new();
    object.insert(key.to_owned(), value.into());
    object
}
