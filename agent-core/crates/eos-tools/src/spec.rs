//! Spec-building helpers (anchor §10).
//!
//! Each model-facing tool's registration site builds **one** [`ToolSpec`] from
//! its externalized description (loaded from `.eos-agents/tools/*.md` via
//! [`ToolConfigSet`](crate::config::ToolConfigSet)) plus `schemars`-derived
//! input/output schemas (no docstring fallback, GC-tools-02/09). These helpers
//! convert a `schemars` schema into the `JsonObject` shape `ToolSpec` carries.

use eos_llm_client::ToolSpec;
use eos_types::JsonObject;
use schemars::schema::RootSchema;
use serde_json::Value;

use crate::name::ToolName;

/// Convert a `schemars` schema into the `ToolSpec` `input_schema`/`output_schema`
/// object shape.
#[must_use]
pub(crate) fn schema_to_object(schema: RootSchema) -> JsonObject {
    match serde_json::to_value(schema) {
        Ok(Value::Object(map)) => map,
        _ => JsonObject::new(),
    }
}

/// A spec for a plain-text-output tool (`TextToolOutput`): input schema only.
#[must_use]
pub(crate) fn text_spec(name: ToolName, description: &str, input: RootSchema) -> ToolSpec {
    ToolSpec::new(name.as_str(), description, schema_to_object(input), None)
}

/// A spec for a structured-output tool: input + output schema.
#[must_use]
pub(crate) fn json_spec(
    name: ToolName,
    description: &str,
    input: RootSchema,
    output: RootSchema,
) -> ToolSpec {
    ToolSpec::new(
        name.as_str(),
        description,
        schema_to_object(input),
        Some(schema_to_object(output)),
    )
}

/// Build a text-output spec whose already-built input-schema object has its
/// `agent_name` property `enum`-restricted to `allowed` — the per-caller
/// `RestrictedRunSubagentTool` patch (§6.6). The enum is injected into both the
/// top-level `properties.agent_name` and (when present) the `$defs`-free inline
/// schema so the emitted spec reflects the caller-scoped choices.
#[must_use]
pub(crate) fn text_spec_with_agent_enum(
    name: ToolName,
    description: &str,
    input: RootSchema,
    allowed: &[String],
) -> ToolSpec {
    let mut object = schema_to_object(input);
    patch_agent_enum(&mut object, allowed);
    ToolSpec::new(name.as_str(), description, object, None)
}

fn patch_agent_enum(schema: &mut JsonObject, allowed: &[String]) {
    if let Some(Value::Object(props)) = schema.get_mut("properties") {
        if let Some(Value::Object(agent)) = props.get_mut("agent_name") {
            agent.insert(
                "enum".to_owned(),
                Value::Array(allowed.iter().map(|a| Value::String(a.clone())).collect()),
            );
        }
    }
}
