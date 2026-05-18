"""Tool registry for context-aware tool instantiation."""

from __future__ import annotations

import logging
import os
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

from tools._framework.core.base import BaseTool

logger = logging.getLogger(__name__)


@dataclass
class ToolFactoryContext:
    """Runtime context passed to tool factories during agent construction."""

    metadata: dict[str, Any] = field(default_factory=dict)


ToolFactory = Callable[[ToolFactoryContext], BaseTool]

_factories: dict[str, ToolFactory] = {}
_builtins_registered: bool = False


def register_tool_factory(
    name: str, factory: ToolFactory, *, override: bool = False
) -> None:
    """Register a factory for a named tool.

    Raises ``ValueError`` if *name* is already registered unless
    ``override=True`` is passed. This prevents plugins from silently
    shadowing builtin tools (or each other).
    """
    if name in _factories and not override:
        raise ValueError(
            f"Tool factory {name!r} already registered; "
            f"pass override=True to replace."
        )
    _factories[name] = factory
    logger.debug("Registered tool factory: %s", name)


def register_tool_instance(tool: BaseTool, *, override: bool = False) -> None:
    """Register a reusable stateless tool instance."""

    def factory(ctx: ToolFactoryContext) -> BaseTool:
        del ctx
        return tool

    register_tool_factory(tool.name, factory, override=override)


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
    """Register built-in tool factories.

    ``load_skill_reference`` is registered as a per-agent factory
    (:func:`tools.skills.make_load_skill_reference_from_context`). It needs
    the spawning agent's name at create time to scope ``allowed_slugs`` to
    the agent's own ``AgentDefinition.skill`` folder — that scoping is what
    keeps the "at most one skill per launch" invariant load-bearing for
    Round 3. The factory consults the agent registry at create time so
    profile-level ``allowed_tools`` gating in
    ``engine/agent/factory.py:_register_requested_tools`` keeps deciding
    who gets the tool.

    Set ``EOS_SKIP_PLUGIN_IMPORTS_FOR_TESTS=1`` to skip plugin discovery —
    useful for unit tests that want to exercise the framework in
    isolation without triggering transitive plugin imports.
    """
    from tools.ask_helper import make_ask_helper_tools
    from tools.sandbox import make_sandbox_tools
    from tools.submission import make_submission_tools
    from tools.subagent import make_subagent_tool_from_context
    from tools.skills import make_load_skill_reference_from_context

    _register_many(make_sandbox_tools())
    _register_many(make_submission_tools())
    _register_many(make_ask_helper_tools())
    register_tool_factory("run_subagent", make_subagent_tool_from_context)
    register_tool_factory(
        "load_skill_reference", make_load_skill_reference_from_context
    )
    if not os.environ.get("EOS_SKIP_PLUGIN_IMPORTS_FOR_TESTS"):
        from plugins.core.loader import register_plugin_tools

        _register_many(register_plugin_tools())


def _ensure_builtins_registered() -> None:
    global _builtins_registered
    # If `_factories` was externally cleared (e.g. by a test fixture), the
    # flag is stale — fall through and re-register. This keeps the
    # idempotency guard from breaking test isolation.
    if _builtins_registered and _factories:
        return
    _register_builtins()
    _builtins_registered = True
