"""Toolkit factory registry — context-aware toolkit instantiation.

Each toolkit can self-register a factory function that accepts a ToolkitContext
and returns a BaseToolkit instance. The builder calls ``create_toolkit(name, ctx)``
instead of hardcoded if/elif chains.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Any, Callable

from ephemeralos.tools.base import BaseToolkit

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
    raise KeyError(
        f"Toolkit factory '{name}' not registered. "
        f"Available: {list(_factories.keys())}"
    )


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
        from ephemeralos.tools.daytona_toolkit import DaytonaToolkit

        sandbox_id = ctx.metadata.get("sandbox_id", "")
        return DaytonaToolkit(sandbox_id=sandbox_id or None)

    def _create_ci(ctx: ToolkitContext) -> BaseToolkit:
        from ephemeralos.tools.ci_toolkit import CIToolkit

        return CIToolkit()

    register_toolkit_factory("daytona", _create_daytona)
    register_toolkit_factory("ci", _create_ci)
    register_toolkit_factory("explorer", _create_ci)  # backward-compat alias


_register_builtins()
