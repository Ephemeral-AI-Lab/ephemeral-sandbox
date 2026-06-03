//! [`TerminalTool`] — the closed set of terminal tools — and the **total**
//! descriptor catalog.
//!
//! Ports `_terminals/registry.py`. The Python registry has only 4 of 6
//! descriptors (advisor + exploration rely on `render_terminal_catalog`'s generic
//! fallback). Resolution (GC-tools-03): the Rust domain is **all six**
//! `is_terminal_tool=True` tools; the advisor + exploration descriptors are
//! authored so the fallback branch disappears. Totality is a compile-time
//! exhaustive `match` over the [`TerminalTool`] enum.

use crate::name::ToolName;

/// The closed set of terminal tools. `#[non_exhaustive]` for additive growth,
/// but every variant has a descriptor (compile-time totality).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum TerminalTool {
    /// `submit_root_outcome`.
    Root,
    /// `submit_generator_outcome`.
    Generator,
    /// `submit_reducer_outcome`.
    Reducer,
    /// `submit_planner_outcome`.
    Planner,
    /// `submit_advisor_feedback`.
    AdvisorFeedback,
    /// `submit_exploration_result`.
    ExplorationResult,
}

impl TerminalTool {
    /// Every terminal tool.
    pub const ALL: [TerminalTool; 6] = [
        TerminalTool::Root,
        TerminalTool::Generator,
        TerminalTool::Reducer,
        TerminalTool::Planner,
        TerminalTool::AdvisorFeedback,
        TerminalTool::ExplorationResult,
    ];

    /// The wire [`ToolName`] this terminal submits with.
    #[must_use]
    pub const fn tool_name(self) -> ToolName {
        match self {
            TerminalTool::Root => ToolName::SubmitRootOutcome,
            TerminalTool::Generator => ToolName::SubmitGeneratorOutcome,
            TerminalTool::Reducer => ToolName::SubmitReducerOutcome,
            TerminalTool::Planner => ToolName::SubmitPlannerOutcome,
            TerminalTool::AdvisorFeedback => ToolName::SubmitAdvisorFeedback,
            TerminalTool::ExplorationResult => ToolName::SubmitExplorationResult,
        }
    }

    /// The [`TerminalTool`] for a terminal [`ToolName`], or `None` for a
    /// non-terminal tool.
    #[must_use]
    pub const fn from_tool_name(name: ToolName) -> Option<TerminalTool> {
        match name {
            ToolName::SubmitRootOutcome => Some(TerminalTool::Root),
            ToolName::SubmitGeneratorOutcome => Some(TerminalTool::Generator),
            ToolName::SubmitReducerOutcome => Some(TerminalTool::Reducer),
            ToolName::SubmitPlannerOutcome => Some(TerminalTool::Planner),
            ToolName::SubmitAdvisorFeedback => Some(TerminalTool::AdvisorFeedback),
            ToolName::SubmitExplorationResult => Some(TerminalTool::ExplorationResult),
            _ => None,
        }
    }
}

/// A terminal tool's catalog entry (Python `TerminalToolDescriptor`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalDescriptor {
    /// The submitting tool name.
    pub name: ToolName,
    /// When to select this terminal.
    pub selection_guidance: &'static str,
    /// What an advisor reviewing this terminal focuses on.
    pub advisor_review_focus: &'static str,
}

/// The total descriptor for a terminal tool (exhaustive `match`).
#[must_use]
pub const fn descriptor(terminal: TerminalTool) -> TerminalDescriptor {
    match terminal {
        // -- Verbatim from `_terminals/registry.py` (4 of 6). --
        TerminalTool::Root => TerminalDescriptor {
            name: ToolName::SubmitRootOutcome,
            selection_guidance: "Call with status=\"success\" when the user request is complete and verified; call with status=\"failed\" when it cannot be completed. The outcome is the user-facing request result.",
            advisor_review_focus: "Verify the root outcome is complete, factual, and supported by the work done. For failure, confirm the blocker is concrete.",
        },
        TerminalTool::Generator => TerminalDescriptor {
            name: ToolName::SubmitGeneratorOutcome,
            selection_guidance: "Call with status=\"success\" when the `<assigned_task>` deliverable is complete and verified; call with status=\"failed\" when the task cannot be completed in this attempt. The outcome must carry the concrete result, evidence, and artifact references.",
            advisor_review_focus: "Verify the chosen status matches the work. For success, confirm the deliverable exists, satisfies the task specification, and is consistent with dependencies. For failure, confirm the blocker is real, specific, and not a premature give-up.",
        },
        TerminalTool::Planner => TerminalDescriptor {
            name: ToolName::SubmitPlannerOutcome,
            selection_guidance: "Call with a generator/reducer DAG for this attempt. Omit `deferred_goal_for_next_iteration` when the plan covers all current-iteration goal items and leaves no remaining items; set it only for concrete current-iteration goal items intentionally deferred to the next iteration.",
            advisor_review_focus: "Review the DAG against `<iteration_goal>`: every required current item must have generator work or be explicitly listed in `deferred_goal_for_next_iteration`. Flag missing items, vague deferred goals, backlog dumps, mis-scoped tasks, and dependency mistakes.",
        },
        TerminalTool::Reducer => TerminalDescriptor {
            name: ToolName::SubmitReducerOutcome,
            selection_guidance: "Call with status=\"success\" when the assigned reducer work is finished from `<dependencies>` context; call with status=\"failed\" when the reducer work cannot be completed from the current context. The outcome must summarize the result or blocker.",
            advisor_review_focus: "Verify the chosen status matches `<assigned_task>` and `<dependencies>`. For success, confirm the assigned reducer work is actually complete. For failure, confirm the blocker prevents completion and is specific enough for retry or replanning.",
        },
        // -- Authored to close the GC-tools-03 gap (the 2 the Python registry
        //    leaves to the generic fallback). --
        TerminalTool::AdvisorFeedback => TerminalDescriptor {
            name: ToolName::SubmitAdvisorFeedback,
            selection_guidance: "Call with verdict=\"approve\" when the pending submission satisfies its contract and is ready to send; call with verdict=\"reject\" when it does not. The summary must give concrete, actionable reasons for the verdict.",
            advisor_review_focus: "Confirm the verdict follows from the pending submission's contract and that the summary names specific, fixable issues rather than vague concerns.",
        },
        TerminalTool::ExplorationResult => TerminalDescriptor {
            name: ToolName::SubmitExplorationResult,
            selection_guidance: "Call when the exploration goal is answered: provide a summary, the concrete findings, and the file/reference paths that support them. Report what was found, not a plan of further work.",
            advisor_review_focus: "Confirm the summary is supported by the listed findings and references, and that the findings answer the exploration goal without speculative or unsupported claims.",
        },
    }
}

/// Which descriptor field a rendered terminal catalog presents (Python
/// `CatalogFocus = Literal["selection_guidance", "advisor_review_focus"]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolInstructions {
    /// The parent-facing "when to call this terminal" guidance.
    SelectionGuidance,
    /// The advisor-facing "what to verify" focus.
    AdvisorReviewFocus,
}

/// Render a terminal-tool catalog for `terminals` at the given `focus` — the port
/// of `registry.render_terminal_catalog`. One backtick-wrapped row per terminal
/// (`` - `name` — text ``), blank-line separated; an unregistered terminal gets a
/// stub row. Empty input yields an empty string (callers add their own heading).
#[must_use]
pub fn render_tool_instruction(terminals: &[ToolName], focus: ToolInstructions) -> String {
    if terminals.is_empty() {
        return String::new();
    }
    terminals
        .iter()
        .map(|&name| match TerminalTool::from_tool_name(name) {
            Some(terminal) => {
                let d = descriptor(terminal);
                let text = match focus {
                    ToolInstructions::SelectionGuidance => d.selection_guidance,
                    ToolInstructions::AdvisorReviewFocus => d.advisor_review_focus,
                };
                format!("- `{}` — {text}", name.as_str())
            }
            None => format!(
                "- `{}` — (no descriptor registered for this terminal)",
                name.as_str()
            ),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name::ToolName;

    // AC-tools-07: every TerminalTool has non-empty descriptor fields; every
    // terminal ToolName maps to a TerminalTool (and back).
    #[test]
    fn descriptors_total() {
        for terminal in TerminalTool::ALL {
            let d = descriptor(terminal);
            assert_eq!(d.name, terminal.tool_name());
            assert!(!d.selection_guidance.is_empty(), "{terminal:?} guidance");
            assert!(
                !d.advisor_review_focus.is_empty(),
                "{terminal:?} review focus"
            );
            // round-trip name <-> terminal
            assert_eq!(
                TerminalTool::from_tool_name(terminal.tool_name()),
                Some(terminal)
            );
        }

        // Exactly the six submit_* tools are terminal; all others are not.
        let terminal_names: Vec<ToolName> = ToolName::ALL
            .into_iter()
            .filter(|n| TerminalTool::from_tool_name(*n).is_some())
            .collect();
        assert_eq!(terminal_names.len(), 6);
        assert!(TerminalTool::from_tool_name(ToolName::ReadFile).is_none());
    }
}
