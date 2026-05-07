"""Helpers for enumerating tools exposed by the runtime."""

from __future__ import annotations

from dataclasses import dataclass

from tools.core.base import BaseTool
from tools.core.registry import ToolRegistry
from tools.factory import ToolFactoryContext, create_tool, list_available_tools


@dataclass(frozen=True)
class ToolCatalogEntry:
    """UI/API-safe tool metadata."""

    name: str
    description: str


def _iter_available_tools() -> list[BaseTool]:
    ctx = ToolFactoryContext()
    return [create_tool(name, ctx) for name in list_available_tools()]


def _background_tool_names() -> list[str]:
    return sorted(
        {
            tool.name
            for tool in _iter_available_tools()
            if getattr(tool, "background", "forbidden") != "forbidden"
        }
    )


def collect_tool_catalog(
    tool_registry: ToolRegistry | None = None,
    *,
    include_runtime_tools: bool = False,
) -> list[ToolCatalogEntry]:
    """Return deduplicated tool metadata suitable for API responses."""

    by_name: dict[str, ToolCatalogEntry] = {}

    def _merge_tool(tool: BaseTool) -> None:
        if tool.name not in by_name:
            by_name[tool.name] = ToolCatalogEntry(
                name=tool.name,
                description=tool.description,
            )

    for tool in tool_registry.list_tools() if tool_registry is not None else []:
        _merge_tool(tool)
    for tool in _iter_available_tools():
        _merge_tool(tool)

    if include_runtime_tools:
        from tools.builtins.background import make_background_tools

        background_tool_names = _background_tool_names()
        if background_tool_names:
            for tool in make_background_tools(background_tool_names):
                _merge_tool(tool)

    return sorted(by_name.values(), key=lambda entry: entry.name)
