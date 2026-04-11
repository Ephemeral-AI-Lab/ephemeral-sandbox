"""Toolkit factory registry — context-aware toolkit instantiation.

Two registration paths are supported:

1. **Preferred**: register a toolkit *class*. ``create_toolkit`` calls
   ``cls.from_context(ctx)``, which each toolkit can override to read
   what it needs from ``ctx.metadata``. Construction logic lives with
   the toolkit, not in this module.

2. **Legacy / tests**: register a *callable* that takes a ToolkitContext
   and returns a toolkit. Kept for tests and ad-hoc registration.
"""

from __future__ import annotations

import logging
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

from tools.core.base import BaseToolkit

logger = logging.getLogger(__name__)


@dataclass
class ToolkitContext:
    """Runtime context passed to toolkit factories during agent construction.

    Only ``metadata`` is meaningful. Toolkits that need agent name, cwd, or
    similar should read them from ``metadata``.
    """

    metadata: dict[str, Any] = field(default_factory=dict)


ToolkitFactoryFn = Callable[[ToolkitContext], BaseToolkit]

_factories: dict[str, ToolkitFactoryFn] = {}
_classes: dict[str, type[BaseToolkit]] = {}


def register_toolkit_factory(name: str, factory: ToolkitFactoryFn) -> None:
    """Register a factory callable for a named toolkit (legacy/test path)."""
    _factories[name] = factory
    logger.debug("Registered toolkit factory: %s", name)


def register_toolkit_class(name: str, cls: type[BaseToolkit]) -> None:
    """Register a toolkit class. ``cls.from_context(ctx)`` is called on demand."""
    _classes[name] = cls
    logger.debug("Registered toolkit class: %s -> %s", name, cls.__name__)


def create_toolkit(name: str, ctx: ToolkitContext) -> BaseToolkit:
    """Create a toolkit instance by name.

    Class registrations win over callable registrations when both exist.
    Raises KeyError if no registration exists for *name*.
    """
    cls = _classes.get(name)
    if cls is not None:
        return cls.from_context(ctx)
    factory = _factories.get(name)
    if factory is not None:
        return factory(ctx)
    raise KeyError(
        f"Toolkit '{name}' not registered. "
        f"Classes: {list(_classes)} Factories: {list(_factories)}"
    )


def has_factory(name: str) -> bool:
    """Return True if a class or callable factory is registered for *name*."""
    return name in _classes or name in _factories


def list_factories() -> list[str]:
    """List all registered toolkit names (class + callable)."""
    return list({*_classes.keys(), *_factories.keys()})


# ---------------------------------------------------------------------------
# Self-register built-in toolkits
# ---------------------------------------------------------------------------


def _register_builtins() -> None:
    """Register built-in toolkit classes. Each toolkit owns its from_context."""
    from tools.atlas import AtlasToolkit
    from tools.daytona_toolkit import DaytonaToolkit
    from tools.ci_toolkit import CIToolkit
    from tools.posthook.toolkits import (
        SubmitPlanToolkit,
        SubmitReplanPlanToolkit,
        SubmitReplanPosthookToolkit,
        SubmitRetryPosthookToolkit,
        SubmitSummaryToolkit,
    )
    from tools.subagent import SubagentToolkit
    from tools.team_context import (
        ContextInheritanceToolkit,
        ContextSharingToolkit,
        TeamContextToolkit,
    )

    register_toolkit_class("sandbox_operations", DaytonaToolkit)
    register_toolkit_class("code_intelligence", CIToolkit)
    register_toolkit_class("subagent", SubagentToolkit)
    register_toolkit_class("context_inheritance", ContextInheritanceToolkit)
    register_toolkit_class("context_sharing", ContextSharingToolkit)
    register_toolkit_class("team_context", TeamContextToolkit)
    register_toolkit_class("atlas", AtlasToolkit)
    register_toolkit_class("submit_plan_posthook", SubmitPlanToolkit)
    register_toolkit_class("submit_summary_posthook", SubmitSummaryToolkit)
    register_toolkit_class("posthook_submit_retry", SubmitRetryPosthookToolkit)
    register_toolkit_class("posthook_submit_replan", SubmitReplanPosthookToolkit)
    register_toolkit_class("submit_replan_posthook", SubmitReplanPlanToolkit)


_register_builtins()
