"""CI Toolkit — read-only code intelligence queries for agents.

Lightweight toolkit for agents that need code grounding without write
access. All tools degrade gracefully if no CI service is configured.
"""

from tools.base import BaseToolkit
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

    def __init__(self) -> None:
        super().__init__(
            name="code_intelligence",
            description="Read-only code intelligence: symbols, structure, changes",
            tools=[
                ci_status,
                ci_workspace_structure,
                ci_query_symbols,
                ci_query_references,
                ci_edit_hotspots,
                ci_recent_changes,
                ci_read_file,
            ],
            instructions=(
                "Read-only code intelligence for understanding codebases "
                "without modifying them. Use to ground your reasoning before making changes.\n\n"
                "- `ci_status` — check if the code intelligence service is available.\n"
                "- `ci_workspace_structure` — get a tree view of the project layout. "
                "Use first to orient yourself in an unfamiliar codebase.\n"
                "- `ci_query_symbols` — find functions, classes, or variables by name. "
                "Use to locate definitions across the project.\n"
                "- `ci_query_references` — find all usages of a symbol. "
                "Use to understand impact before renaming or refactoring.\n"
                "- `ci_edit_hotspots` — find frequently edited files. "
                "Use to identify areas of churn that may need attention.\n"
                "- `ci_recent_changes` — see recent commits and diffs. "
                "Use to understand what changed and why.\n"
                "- `ci_read_file` — read file contents via the CI service. "
                "Use when sandbox tools are not available."
            ),
        )


__all__ = ["CIToolkit"]
