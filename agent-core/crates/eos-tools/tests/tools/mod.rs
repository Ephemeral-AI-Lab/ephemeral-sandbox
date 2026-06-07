use super::*;
use crate::hooks::Hook;

mod helper_terminals;
mod isolated_workspace;
mod submission;

// AC-tools-09: every registered tool has a typed ToolName + an intent (no
// String keys; intent is mandatory). Covers GC-tools-04/05.
#[test]
fn all_tools_named_and_intented() {
    let registry = build_default_registry(
        &repo_tools_config(),
        &CallerScope {
            dispatchable_subagents: vec!["explorer".to_owned()],
            skill_slug: None,
        },
    );
    // The default built-in set, all keyed by ToolKey.
    assert_eq!(registry.len(), ToolName::ALL.len());
    let mut seen = std::collections::BTreeSet::new();
    for tool in registry.list() {
        assert!(seen.insert(tool.name.clone()), "duplicate {}", tool.name);
        // intent is a ToolIntent (not Option) — its presence is structural.
        let _ = tool.intent;
    }
    // Every ToolName is registered exactly once.
    assert_eq!(seen.len(), ToolName::ALL.len());
}

// GC-tools-02: every spec description is non-empty (loaded from the
// externalized config, validated at load).
#[test]
fn every_spec_has_a_nonempty_description() {
    let registry = build_default_registry(&repo_tools_config(), &CallerScope::default());
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
    let registry = build_default_registry(&repo_tools_config(), &CallerScope::default());
    for tool in registry.list().filter(|t| t.is_terminal) {
        let terminal = tool
            .name
            .as_builtin()
            .and_then(crate::tools::terminal::TerminalTool::from_tool_name)
            .expect("terminal tool has a TerminalTool");
        let d = crate::tools::terminal::descriptor(terminal);
        assert!(!d.selection_guidance.is_empty());
    }
}

fn advisor_hook_count(registry: &ToolRegistry, name: ToolName) -> usize {
    registry
        .get(name)
        .expect("tool registered")
        .hooks
        .iter()
        .filter(|hook| matches!(hook, Hook::AdvisorApproval { .. }))
        .count()
}

// Exactly the four main submission terminals carry one `AdvisorApproval` hook;
// helper/explorer terminals and `ask_advisor` carry none.
#[test]
fn advisor_gate_wired_on_exactly_the_four_main_terminals() {
    let registry = build_default_registry(&repo_tools_config(), &CallerScope::default());
    for gated in [
        ToolName::SubmitRootOutcome,
        ToolName::SubmitPlannerOutcome,
        ToolName::SubmitGeneratorOutcome,
        ToolName::SubmitReducerOutcome,
    ] {
        assert_eq!(
            advisor_hook_count(&registry, gated),
            1,
            "{gated:?} must carry exactly one AdvisorApproval hook"
        );
        assert!(
            registry
                .get(gated)
                .expect("registered")
                .hooks
                .iter()
                .any(|hook| matches!(
                    hook,
                    Hook::AdvisorApproval { tool } if *tool == gated
                )),
            "{gated:?}'s AdvisorApproval hook must target itself"
        );
    }
    for ungated in [
        ToolName::AskAdvisor,
        ToolName::SubmitAdvisorFeedback,
        ToolName::SubmitExplorationResult,
    ] {
        assert_eq!(
            advisor_hook_count(&registry, ungated),
            0,
            "{ungated:?} must NOT be advisor-gated (else ask_advisor self-gates / deadlocks)"
        );
    }
}

// `RequireNoBackgroundSessions` precedes `AdvisorApproval` on every gated
// terminal so a background rejection surfaces before the advisor gate.
#[test]
fn no_background_sessions_precedes_advisor_on_gated_terminals() {
    let registry = build_default_registry(&repo_tools_config(), &CallerScope::default());
    for gated in [
        ToolName::SubmitRootOutcome,
        ToolName::SubmitPlannerOutcome,
        ToolName::SubmitGeneratorOutcome,
        ToolName::SubmitReducerOutcome,
    ] {
        let hooks = &registry.get(gated).expect("registered").hooks;
        let no_background_sessions = hooks
            .iter()
            .position(|hook| matches!(hook, Hook::RequireNoBackgroundSessions { .. }));
        let advisor = hooks
            .iter()
            .position(|hook| matches!(hook, Hook::AdvisorApproval { .. }));
        assert!(
            matches!((no_background_sessions, advisor), (Some(n), Some(a)) if n < a),
            "{gated:?}: RequireNoBackgroundSessions must precede AdvisorApproval"
        );
    }
}

// Lock the externalized security/policy wiring for the non-terminal tools
// (the `submit_*` advisor gates are covered above). Because intent + hooks now
// live in editable `.eos-agents/tools/*.md`, this is the regression guard that
// a future markdown edit cannot silently drop a destructive-shell gate, the
// isolated-mode block, or a no-inflight guard, or flip an intent.
#[test]
fn security_policy_wiring_is_locked() {
    use crate::core::intent::ToolIntent;
    let registry = build_default_registry(&repo_tools_config(), &CallerScope::default());
    let hooks = |name: ToolName| registry.get(name).expect("registered").hooks.clone();
    let intent = |name: ToolName| registry.get(name).expect("registered").intent;

    assert_eq!(
        hooks(ToolName::ExecCommand),
        vec![
            Hook::DestructiveGitShell {
                tool: ToolName::ExecCommand
            },
            Hook::DestructiveShell {
                tool: ToolName::ExecCommand
            },
        ],
        "exec_command must keep both destructive-shell gates, in order"
    );
    assert_eq!(
        hooks(ToolName::AskAdvisor),
        vec![Hook::BlockInIsolatedMode {
            tool: ToolName::AskAdvisor
        }],
        "ask_advisor must be blocked in isolated mode"
    );
    for iso in [
        ToolName::EnterIsolatedWorkspace,
        ToolName::ExitIsolatedWorkspace,
    ] {
        assert_eq!(
            hooks(iso),
            vec![Hook::RequireNoBackgroundSessions { tool: iso }],
            "{iso:?} must reject while background work is in flight"
        );
    }

    assert_eq!(intent(ToolName::ReadFile), ToolIntent::ReadOnly);
    assert_eq!(intent(ToolName::WriteFile), ToolIntent::WriteAllowed);
    assert_eq!(intent(ToolName::ExecCommand), ToolIntent::WriteAllowed);
    assert_eq!(intent(ToolName::RunSubagent), ToolIntent::WriteAllowed);
    assert_eq!(intent(ToolName::DelegateWorkflow), ToolIntent::Lifecycle);
    assert_eq!(intent(ToolName::CancelWorkflow), ToolIntent::Lifecycle);
}

// AC-tools-08: `registry.specs()` is a stable, ordered Vec<ToolSpec> for the
// default tool set, snapshotted. The `run_subagent` spec reproduces the
// restricted schema: `agent_name` carries the caller-scoped enum, so the
// fixture is built for a fixed allow-list.
#[test]
fn specs_snapshot() {
    let registry = build_default_registry(
        &repo_tools_config(),
        &CallerScope {
            dispatchable_subagents: vec!["explorer".to_owned(), "coder".to_owned()],
            skill_slug: None,
        },
    );
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
