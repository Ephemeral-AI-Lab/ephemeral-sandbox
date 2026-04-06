"""Toolkit factory registry — context-aware toolkit instantiation.

Each toolkit can self-register a factory function that accepts a ToolkitContext
and returns a BaseToolkit instance. The builder calls ``create_toolkit(name, ctx)``
instead of hardcoded if/elif chains.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Any, Callable

from tools.core.base import BaseToolkit

logger = logging.getLogger(__name__)

ToolkitFactoryFn = Callable[["ToolkitContext"], BaseToolkit]

_factories: dict[str, ToolkitFactoryFn] = {}


@dataclass
class ToolkitContext:
    """Runtime context passed to toolkit factories during agent construction."""

    agent_name: str = ""
    cwd: str = ""
    metadata: dict[str, Any] = field(default_factory=dict)


def register_toolkit_factory(name: str, factory: ToolkitFactoryFn) -> None:
    """Register a factory for a named toolkit."""
    _factories[name] = factory
    logger.debug("Registered toolkit factory: %s", name)


def create_toolkit(name: str, ctx: ToolkitContext) -> BaseToolkit:
    """Create a toolkit instance by name via its registered factory.

    Raises KeyError if no factory is registered for *name*.
    """
    factory = _factories.get(name)
    if factory is not None:
        return factory(ctx)
    raise KeyError(f"Toolkit factory '{name}' not registered. Available: {list(_factories.keys())}")


def list_factories() -> list[str]:
    """List all registered factory names."""
    return list(_factories.keys())


def has_factory(name: str) -> bool:
    """Check if a factory is registered for the given name."""
    return name in _factories


# ---------------------------------------------------------------------------
# Self-register built-in toolkit factories
# ---------------------------------------------------------------------------


def _register_builtins() -> None:
    """Register factories for toolkits that need runtime context."""

    def _create_daytona(ctx: ToolkitContext) -> BaseToolkit:
        from tools.daytona_toolkit import DaytonaToolkit

        sandbox_id = ctx.metadata.get("sandbox_id", "")
        return DaytonaToolkit(sandbox_id=sandbox_id or None)

    def _create_ci(ctx: ToolkitContext) -> BaseToolkit:
        from tools.ci_toolkit import CIToolkit

        return CIToolkit()

    def _create_coordination_planner(ctx: ToolkitContext) -> BaseToolkit:
        from tools.coordination_planner import CoordinationPlannerToolkit

        return CoordinationPlannerToolkit(
            agent_names=ctx.metadata.get("agent_names"),
            phase_outputs=ctx.metadata.get("phase_outputs"),
        )

    def _create_coordination_worker(ctx: ToolkitContext) -> BaseToolkit:
        from tools.coordination_worker import CoordinationWorkerToolkit

        return CoordinationWorkerToolkit(
            task_id=ctx.metadata.get("task_id", ""),
            run_id=ctx.metadata.get("run_id", ""),
            store=ctx.metadata.get("store"),
            replan_handler=ctx.metadata.get("replan_handler"),
            trigger_dispatch_fn=ctx.metadata.get("trigger_dispatch_fn"),
        )

    def _create_subagent(ctx: ToolkitContext) -> BaseToolkit:
        from tools.subagent import SubagentToolkit

        return SubagentToolkit(
            run_agent_fn=ctx.metadata.get("run_agent_fn"),
        )

    def _create_pipeline_context(ctx: ToolkitContext) -> BaseToolkit:
        from tools.pipeline_context import PipelineContextToolkit

        return PipelineContextToolkit(
            context_map=ctx.metadata.get("pipeline_context_map"),
            pipeline_meta=ctx.metadata.get("pipeline_meta"),
            current_step=ctx.metadata.get("pipeline_current_step"),
        )

    register_toolkit_factory("sandbox_operations", _create_daytona)
    register_toolkit_factory("code_intelligence", _create_ci)
    register_toolkit_factory("coordination_planner", _create_coordination_planner)
    register_toolkit_factory("coordination_worker", _create_coordination_worker)
    register_toolkit_factory("subagent", _create_subagent)
    register_toolkit_factory("pipeline_context", _create_pipeline_context)


_register_builtins()
