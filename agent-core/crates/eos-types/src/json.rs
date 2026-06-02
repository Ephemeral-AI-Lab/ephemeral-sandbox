//! Transitional JSON container aliases.
//!
//! These are deliberately *untyped* transitional containers
//! (`terminal_tool_result`, audit `payload`, tool args) that downstream crates
//! parse into typed shapes at their boundaries (`api-parse-dont-validate`).
//! They are aliases, not newtypes (YAGNI — no wrapper methods until a caller
//! needs one).

/// Untyped JSON value, mirroring the Python `JsonValue = Any` in `audit/base.py`.
pub type JsonValue = serde_json::Value;

/// Untyped JSON object map used for transitional metadata (plan §1). The owned
/// transitional-metadata contract enumerated in spec-conventions §5.
pub type JsonObject = serde_json::Map<String, serde_json::Value>;
