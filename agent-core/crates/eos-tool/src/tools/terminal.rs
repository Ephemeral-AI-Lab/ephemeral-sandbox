//! [`TerminalTool`] — the closed set of terminal tools — and the **total**
//! descriptor catalog.
//!
//! Totality is a compile-time exhaustive `match` over the [`TerminalTool`] enum.

use crate::ToolName;

/// The closed set of terminal tools. `#[non_exhaustive]` for additive growth,
/// but every variant has a descriptor (compile-time totality).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum TerminalTool {
    /// `submit_root_task_outcome`.
    RootTask,
    /// `submit_plan_outcome`.
    Plan,
    /// `submit_worker_outcome`.
    Worker,
    /// `submit_advisor_outcome`.
    Advisor,
    /// `submit_subagent_outcome`.
    Subagent,
}

impl TerminalTool {
    /// Every terminal tool.
    pub const ALL: [TerminalTool; 5] = [
        TerminalTool::RootTask,
        TerminalTool::Plan,
        TerminalTool::Worker,
        TerminalTool::Advisor,
        TerminalTool::Subagent,
    ];

    /// The wire [`ToolName`] this terminal submits with.
    #[must_use]
    pub const fn tool_name(self) -> ToolName {
        match self {
            TerminalTool::RootTask => ToolName::SubmitRootTaskOutcome,
            TerminalTool::Plan => ToolName::SubmitPlanOutcome,
            TerminalTool::Worker => ToolName::SubmitWorkerOutcome,
            TerminalTool::Advisor => ToolName::SubmitAdvisorOutcome,
            TerminalTool::Subagent => ToolName::SubmitSubagentOutcome,
        }
    }

    /// The [`TerminalTool`] for a terminal [`ToolName`], or `None` for a
    /// non-terminal tool.
    #[must_use]
    pub const fn from_tool_name(name: ToolName) -> Option<TerminalTool> {
        match name {
            ToolName::SubmitRootTaskOutcome => Some(TerminalTool::RootTask),
            ToolName::SubmitPlanOutcome => Some(TerminalTool::Plan),
            ToolName::SubmitWorkerOutcome => Some(TerminalTool::Worker),
            ToolName::SubmitAdvisorOutcome => Some(TerminalTool::Advisor),
            ToolName::SubmitSubagentOutcome => Some(TerminalTool::Subagent),
            _ => None,
        }
    }
}

/// A terminal tool's catalog entry (Rust `TerminalToolDescriptor`).
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
        TerminalTool::RootTask => TerminalDescriptor {
            name: ToolName::SubmitRootTaskOutcome,
            selection_guidance: "Call with status=\"success\" when the user request is complete and verified; call with status=\"failed\" when it cannot be completed. The outcome is the user-facing request result.",
            advisor_review_focus: "Verify the root outcome is complete, factual, and supported by the work done. For failure, confirm the blocker is concrete.",
        },
        TerminalTool::Plan => TerminalDescriptor {
            name: ToolName::SubmitPlanOutcome,
            selection_guidance: "Call with a work item plan for this attempt. Omit `deferred_goal_for_next_iteration` when the plan covers all current-iteration goal items and leaves no remaining items; set it only for concrete current-iteration goal items intentionally deferred to the next iteration.",
            advisor_review_focus: "Review the plan against `<iteration_goal>`: every required current item must have worker work or be explicitly listed in `deferred_goal_for_next_iteration`. Flag missing items, vague deferred goals, backlog dumps, mis-scoped work items, and dependency mistakes.",
        },
        TerminalTool::Worker => TerminalDescriptor {
            name: ToolName::SubmitWorkerOutcome,
            selection_guidance: "Call with status=\"success\" when the assigned work item is complete and verified; call with status=\"failed\" when it cannot be completed in this attempt. The outcome must summarize the result or blocker.",
            advisor_review_focus: "Verify the chosen status matches the work item and direct needs. For success, confirm the deliverable exists and satisfies the work specification. For failure, confirm the blocker prevents completion and is specific enough for retry or replanning.",
        },
        TerminalTool::Advisor => TerminalDescriptor {
            name: ToolName::SubmitAdvisorOutcome,
            selection_guidance: "Call with verdict=\"approve\" when the pending submission satisfies its contract and is ready to send; call with verdict=\"reject\" when it does not. The outcome must give concrete, actionable reasons for the verdict.",
            advisor_review_focus: "Confirm the verdict follows from the pending submission's contract and that the outcome names specific, fixable issues rather than vague concerns.",
        },
        TerminalTool::Subagent => TerminalDescriptor {
            name: ToolName::SubmitSubagentOutcome,
            selection_guidance: "Call when the subagent goal is answered. The outcome must report what was found, not a plan of further work.",
            advisor_review_focus: "Confirm the outcome answers the subagent goal without speculative or unsupported claims.",
        },
    }
}

/// Which descriptor field a rendered terminal catalog presents (Rust
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
    use crate::ToolName;

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

        // Exactly the five submit_* tools are terminal; all others are not.
        let terminal_names: Vec<ToolName> = ToolName::ALL
            .into_iter()
            .filter(|n| TerminalTool::from_tool_name(*n).is_some())
            .collect();
        assert_eq!(terminal_names.len(), 5);
        assert!(TerminalTool::from_tool_name(ToolName::ReadFile).is_none());
    }
}
