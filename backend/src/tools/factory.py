"""Tool registry for context-aware tool instantiation."""

from __future__ import annotations

import logging
from collections.abc import Callable

from tools.core.base import BaseTool
from tools.factory_context import ToolFactoryContext

logger = logging.getLogger(__name__)

ToolFactory = Callable[[ToolFactoryContext], BaseTool]

_factories: dict[str, ToolFactory] = {}


def register_tool_factory(name: str, factory: ToolFactory) -> None:
    """Register a factory for a named tool."""
    _factories[name] = factory
    logger.debug("Registered tool factory: %s", name)


def register_tool_instance(tool: BaseTool) -> None:
    """Register a reusable stateless tool instance."""

    def factory(ctx: ToolFactoryContext) -> BaseTool:
        del ctx
        return tool

    register_tool_factory(tool.name, factory)


def create_tool(name: str, ctx: ToolFactoryContext) -> BaseTool:
    """Create a tool instance by name."""
    _ensure_builtins_registered()
    factory = _factories.get(name)
    if factory is None:
        raise KeyError(f"Tool '{name}' not registered. Tools: {list(_factories)}")
    tool = factory(ctx)
    if tool.name != name:
        raise ValueError(f"Tool factory for {name!r} returned {tool.name!r}")
    return tool


def create_tools(names: list[str], ctx: ToolFactoryContext) -> list[BaseTool]:
    """Create tool instances, deduplicating by tool name while preserving order."""
    tools: list[BaseTool] = []
    seen: set[str] = set()
    for name in names:
        clean_name = str(name).strip()
        if not clean_name or clean_name in seen:
            continue
        tools.append(create_tool(clean_name, ctx))
        seen.add(clean_name)
    return tools


def has_tool(name: str) -> bool:
    """Return True if a tool factory is registered for *name*."""
    _ensure_builtins_registered()
    return name in _factories


def list_available_tools() -> list[str]:
    """List all registered tool names."""
    _ensure_builtins_registered()
    return list(_factories.keys())


def _register_many(tools: list[BaseTool]) -> None:
    for tool in tools:
        register_tool_instance(tool)


def _register_builtins() -> None:
    """Register built-in tool factories."""
    from tools.sandbox_toolkit import make_sandbox_tools
    from tools.submission import make_submission_tools
    from tools.subagent import make_subagent_tool_from_context

    _register_many(make_sandbox_tools())
    _register_many(make_submission_tools())
    register_tool_factory("run_subagent", make_subagent_tool_from_context)


def _ensure_builtins_registered() -> None:
    if "run_subagent" in _factories and "read_file" in _factories:
        return
    _register_builtins()
