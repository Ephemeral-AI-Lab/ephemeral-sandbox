"""CI Toolkit — read-only code intelligence queries for agents.

Lightweight toolkit for agents that need code grounding without write
access. All tools degrade gracefully if no CI service is configured.
"""

from ephemeralos.tools.base import BaseToolkit
from ephemeralos.tools.ci_toolkit.query_tools import (
    CIStatusTool,
    EditHotspotsTool,
    RecentChangesTool,
    SymbolQueryTool,
    SymbolReferencesTool,
    WorkspaceStructureTool,
)
from ephemeralos.tools.ci_toolkit.file_tools import CIReadFileTool


class CIToolkit(BaseToolkit):
    """Read-only code intelligence toolkit.

    Provides symbol queries, workspace structure, edit hotspots,
    and recent change awareness. Requires a CI gateway in the
    tool execution context.
    """

    def __init__(self) -> None:
        super().__init__(
            name="ci",
            description="Read-only code intelligence: symbols, structure, changes",
            tools=[
                CIStatusTool(),
                WorkspaceStructureTool(),
                SymbolQueryTool(),
                SymbolReferencesTool(),
                EditHotspotsTool(),
                RecentChangesTool(),
                CIReadFileTool(),
            ],
        )


__all__ = ["CIToolkit"]
