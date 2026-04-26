"""Code intelligence tools for agents."""

from tools.core.base import BaseTool
from tools.ci_toolkit.ci_query_symbol import ci_query_symbol
from tools.ci_toolkit.ci_status import ci_status
from tools.ci_toolkit.ci_workspace_structure import ci_workspace_structure
from tools.ci_toolkit.ci_diagnostics import ci_diagnostics

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
