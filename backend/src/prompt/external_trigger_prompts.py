"""Prompt templates for constrained helper agents."""

from __future__ import annotations

from typing import Any


def build_parent_summary_prompt(parent: Any, children: list[Any]) -> str:
    """Build the user prompt text fed to the parent summarizer agent."""
    lines: list[str] = []
    completed_child_ids = [str(getattr(child, "id", "")) for child in children]
    completed_child_ids = [child_id for child_id in completed_child_ids if child_id]
    lines.append("# Parent summarizer task")
    lines.append(
        "All direct children of the parent task are terminal. Read the parent "
        "task detail and each terminal direct child task detail before you "
        "submit the parent roll-up."
    )
    lines.append("")
    lines.append("## Parent task id")
    lines.append(str(parent.id))
    lines.append("")
    lines.append("## Terminal direct child task ids to read")
    if completed_child_ids:
        for child_id in completed_child_ids:
            lines.append(f"- {child_id}")
    else:
        lines.append("(none)")
    lines.append("")
    lines.append(
        "Workflow: first call `read_task_details(task_id=\""
        f"{parent.id}"
        "\")` for the parent. Then call `read_task_details(task_id=...)` once "
        "for every terminal direct child id listed above. Only after every "
        "listed child has been read, produce exactly one terminal submission "
        "call: `submit_task_success(summary=...)` when the roll-up is delivered "
        "or `request_replan(reason=...)` when unresolved child evidence keeps "
        "the parent open. The terminal note body must report what the parent "
        "planned, one direct child line per child with status plus delivered/"
        "replanned/dropped/open-risk classification, and an overall roll-up. "
        "Cite child final summaries, commands, failing ids, exit codes, "
        "blockers, missing summaries, and trivial summaries when present. If "
        "a child claims success from invalid verification evidence — for "
        "example pytest config or warning overrides such as `-o`, "
        "`--override-ini`, `filterwarnings=`, `addopts=`, `-W ignore`, "
        "`PYTHONWARNINGS`, or `-p no:` — classify that child as `open risk` "
        "and call `request_replan(reason=...)` unless another direct child "
        "reran the required command without those overrides and passed. If "
        "`read_task_details` for a listed child returns \"Not found in task "
        "graph\" or the detail lacks a summary, record that child's line as "
        "`<id> (<agent>, <status>): missing detail` or `missing summary` — do "
        "not guess at what the child did and do not drop the child from the "
        "list. Do not collapse the result into \"all children done\" and do "
        "not invent next steps. This terminal submission is the completion "
        "signal for the parent task."
    )
    return "\n".join(lines)
