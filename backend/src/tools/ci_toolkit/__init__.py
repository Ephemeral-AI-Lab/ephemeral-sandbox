"""CI Toolkit — read-only code intelligence queries for agents.

Lightweight toolkit for agents that need code grounding without write
access. All tools degrade gracefully if no CI service is configured.
"""

from tools.core.base import BaseToolkit
from tools.ci_toolkit.query_tools import (
    ci_status,
    ci_edit_hotspots,
    ci_recent_changes,
    ci_query_symbols,
    ci_query_references,
    ci_workspace_structure,
)
from tools.ci_toolkit.file_tools import ci_read_file


class CIToolkit(BaseToolkit):
    """Read-only code intelligence toolkit.

    Provides symbol queries, workspace structure, edit hotspots,
    and recent change awareness. Requires a CI service in the
    tool execution context.
    """

    _NO_FILE_READ_AGENTS = frozenset({"team_planner"})
    _NO_CHANGE_AWARENESS_AGENTS = frozenset({"team_planner"})

    def __init__(
        self,
        *,
        include_file_reads: bool = True,
        include_change_awareness: bool = True,
    ) -> None:
        tools = [
            ci_status,
            ci_workspace_structure,
            ci_query_symbols,
            ci_query_references,
        ]
        if include_change_awareness:
            tools.extend([ci_edit_hotspots, ci_recent_changes])
        instructions = (
            "Read-only code intelligence for understanding codebases "
            "without modifying them. Use to ground your reasoning before making changes.\n\n"
            "- `ci_status` — check if the code intelligence service is available.\n"
            "- `ci_workspace_structure` — get a tree view of the project layout. "
            "Use first to orient yourself in an unfamiliar codebase.\n"
            "- `ci_query_symbols` — find functions, classes, or variables by name. "
            "Use to locate definitions across the project.\n"
            "- `ci_query_references` — find all usages of a symbol. "
            "Use to understand impact before renaming or refactoring.\n"
        )
        if include_change_awareness:
            instructions += (
                "- `ci_edit_hotspots` — find frequently edited files. "
                "Use to identify areas of churn that may need attention.\n"
                "- `ci_recent_changes` — see recent commits and diffs. "
                "Use to understand what changed and why.\n"
            )
        else:
            instructions += (
                "- `ci_edit_hotspots` and `ci_recent_changes` are intentionally unavailable "
                "for planner-style agents. Use them only from execution or collision-aware lanes, "
                "not while mapping initial ownership.\n"
            )
        if include_file_reads:
            tools.append(ci_read_file)
            instructions += (
                "- `ci_read_file` — read file contents via the CI service. "
                "Use when sandbox tools are not available."
            )
        else:
            instructions += (
                "- `ci_read_file` is intentionally unavailable in planner mode. "
                "Use `run_subagent(agent_name=\"scout\", input={\"target_paths\": [...]})` "
                "when file contents are needed for exploration."
            )
        super().__init__(
            name="code_intelligence",
            description="Read-only code intelligence: symbols, structure, changes",
            tools=tools,
            instructions=instructions,
        )

    @classmethod
    def from_context(cls, ctx):  # type: ignore[override]
        agent_name = str((ctx.metadata or {}).get("agent_name") or "").strip()
        return cls(
            include_file_reads=agent_name not in cls._NO_FILE_READ_AGENTS,
            include_change_awareness=agent_name not in cls._NO_CHANGE_AWARENESS_AGENTS,
        )


__all__ = ["CIToolkit"]
