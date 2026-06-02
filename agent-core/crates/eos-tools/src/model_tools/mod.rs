//! The model-facing tools: one module per tool family (sandbox, isolated,
//! submission, `ask_advisor`, workflow, subagent, skills). Each tool authors its
//! Input/Output DTOs, its colocated `ToolSpec` description, and a
//! [`ToolExecutor`](crate::ToolExecutor) impl; the registry builder wires them
//! with their intent / terminal flag / hooks (`meta.rs`).

mod advisor;
mod isolated;
mod sandbox;
mod skills;
mod subagent;
mod submission;
mod workflow;

use std::sync::Arc;

use eos_llm_client::ToolSpec;

use crate::executor::{RegisteredTool, ToolExecutor};
use crate::meta;
use crate::name::ToolName;
use crate::registry::ToolRegistry;
use crate::result::OutputShape;

/// The per-caller scope a tool registry is built for. Today this is the caller's
/// dispatchable-subagent allow-list, which patches the `run_subagent` input
/// schema's `agent_name` enum (§6.6).
#[derive(Debug, Clone, Default)]
pub struct CallerScope {
    /// The subagent profile names this caller may dispatch.
    pub dispatchable_subagents: Vec<String>,
}

/// Register one tool, attaching its canonical intent / terminal flag / hooks
/// (`meta.rs`) so each registration site is a single line.
pub(crate) fn register_tool(
    registry: &mut ToolRegistry,
    name: ToolName,
    spec: ToolSpec,
    output: OutputShape,
    executor: Arc<dyn ToolExecutor>,
) {
    registry.register(
        RegisteredTool::new(
            name,
            meta::tool_intent(name),
            meta::is_terminal(name),
            spec,
            output,
            executor,
        )
        .with_hooks(meta::tool_hooks(name)),
    );
}

/// Build the default tool registry for one caller scope. Insertion order matches
/// the Python factory (sandbox → isolated → submission → `ask_advisor` → workflow →
/// subagent → skills); the order backs the Phase-4 schema snapshot and is the
/// agent-spawn default before `restrict`/`remove`.
#[must_use]
pub fn build_default_registry(caller: &CallerScope) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    sandbox::register(&mut registry);
    isolated::register(&mut registry);
    submission::register(&mut registry);
    advisor::register(&mut registry);
    workflow::register(&mut registry);
    subagent::register(&mut registry, caller);
    skills::register(&mut registry);
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-tools-09: every registered tool has a typed ToolName + an intent (no
    // String keys; intent is mandatory). Covers GC-tools-04/05.
    #[test]
    fn all_tools_named_and_intented() {
        let registry = build_default_registry(&CallerScope {
            dispatchable_subagents: vec!["explorer".to_owned()],
        });
        // The 24-tool default set, all keyed by ToolName.
        assert_eq!(registry.len(), 24);
        let mut seen = std::collections::BTreeSet::new();
        for tool in registry.list() {
            assert!(seen.insert(tool.name), "duplicate {}", tool.name);
            // intent is a ToolIntent (not Option) — its presence is structural.
            let _ = tool.intent;
        }
        // Every ToolName is registered exactly once.
        assert_eq!(seen.len(), ToolName::ALL.len());
    }

    // GC-tools-02: every spec description is non-empty (single colocated source,
    // not a doc comment).
    #[test]
    fn every_spec_has_a_nonempty_description() {
        let registry = build_default_registry(&CallerScope::default());
        for tool in registry.list() {
            assert!(
                !tool.spec.description.trim().is_empty(),
                "{} has an empty description",
                tool.name
            );
        }
    }

    // GC-tools-03 (registry half): every terminal RegisteredTool maps to a
    // TerminalTool descriptor.
    #[test]
    fn terminal_tools_have_descriptors() {
        let registry = build_default_registry(&CallerScope::default());
        for tool in registry.list().filter(|t| t.is_terminal) {
            let terminal = crate::terminal::TerminalTool::from_tool_name(tool.name)
                .expect("terminal tool has a TerminalTool");
            let d = crate::terminal::descriptor(terminal);
            assert!(!d.selection_guidance.is_empty());
        }
    }

    // AC-tools-08: `registry.specs()` is a stable, ordered Vec<ToolSpec> for the
    // default tool set, snapshotted (the crate-owned Phase-4 snapshot). The
    // `run_subagent` spec reproduces the restricted schema: `agent_name` carries
    // the caller-scoped enum, so the fixture is built for a fixed allow-list.
    #[test]
    fn specs_snapshot() {
        let registry = build_default_registry(&CallerScope {
            dispatchable_subagents: vec!["explorer".to_owned(), "coder".to_owned()],
        });
        let specs = registry.specs();
        // Guard the run_subagent patched-enum invariant explicitly (it is easy to
        // miss in a large snapshot).
        let run_subagent = specs
            .iter()
            .find(|s| s.name == "run_subagent")
            .expect("run_subagent registered");
        let agent_enum = &run_subagent.input_schema["properties"]["agent_name"]["enum"];
        assert_eq!(agent_enum, &serde_json::json!(["explorer", "coder"]));

        insta::assert_json_snapshot!("default_tool_specs", specs);
    }
}
