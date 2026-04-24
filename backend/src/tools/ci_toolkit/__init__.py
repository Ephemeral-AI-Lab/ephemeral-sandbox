"""Code intelligence tools for agents."""

from tools.core.base import BaseTool
from tools.ci_toolkit.query_tools import (
    ci_status,
    ci_query_symbol,
    ci_workspace_structure,
)
from tools.ci_toolkit.lsp_tools import ci_diagnostics

CODE_INTELLIGENCE_TOOLS: list[BaseTool] = [
    ci_status,
    ci_workspace_structure,
    ci_query_symbol,
    ci_diagnostics,
]


def make_code_intelligence_tools() -> list[BaseTool]:
    """Return code intelligence tools."""
    return list(CODE_INTELLIGENCE_TOOLS)


__all__ = ["CODE_INTELLIGENCE_TOOLS", "make_code_intelligence_tools"]
