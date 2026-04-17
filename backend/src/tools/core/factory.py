"""Toolkit registry for context-aware toolkit instantiation."""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from typing import Any

from tools.core.base import BaseToolkit

logger = logging.getLogger(__name__)


@dataclass
class ToolkitContext:
    """Runtime context passed to toolkits during agent construction.

    Only ``metadata`` is meaningful. Toolkits that need agent name, cwd, or
    similar should read them from ``metadata``.
    """

    metadata: dict[str, Any] = field(default_factory=dict)


_classes: dict[str, type[BaseToolkit]] = {}


def register_toolkit_class(name: str, cls: type[BaseToolkit]) -> None:
    """Register a toolkit class. ``cls.from_context(ctx)`` is called on demand."""
    _classes[name] = cls
    logger.debug("Registered toolkit class: %s -> %s", name, cls.__name__)


def create_toolkit(name: str, ctx: ToolkitContext) -> BaseToolkit:
    """Create a toolkit instance by name.

    Raises KeyError if no registration exists for *name*.
    """
    cls = _classes.get(name)
    if cls is not None:
        toolkit = cls.from_context(ctx)
        if toolkit.name != name:
            toolkit.name = name
        return toolkit
    raise KeyError(f"Toolkit '{name}' not registered. Toolkits: {list(_classes)}")


def has_toolkit(name: str) -> bool:
    """Return True if a toolkit class is registered for *name*."""
    return name in _classes


def list_toolkits() -> list[str]:
    """List all registered toolkit names."""
    return list(_classes.keys())


# ---------------------------------------------------------------------------
# Self-register built-in toolkits
# ---------------------------------------------------------------------------


def _register_builtins() -> None:
    """Register built-in toolkit classes. Each toolkit owns its from_context."""
    from tools.daytona_toolkit import DaytonaToolkit
    from tools.ci_toolkit import CIToolkit
    from tools.subagent import SubagentToolkit

    # Core toolkits (unchanged)
    register_toolkit_class("sandbox_operations", DaytonaToolkit)
    register_toolkit_class("code_intelligence", CIToolkit)
    register_toolkit_class("subagent", SubagentToolkit)

    # Plan A toolkits — Task Center (notes + scope awareness)
    from tools.task_center import TaskCenterToolkit

    register_toolkit_class("task_center", TaskCenterToolkit)

    # Submission toolkit — in-loop terminal tools
    from tools.submission.toolkit import SubmissionToolkit

    register_toolkit_class("submission", SubmissionToolkit)


_register_builtins()
