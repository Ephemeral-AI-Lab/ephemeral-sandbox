"""Process-global registry of tool guards.

Guards register against a ``tool_glob`` (fnmatch-style), a ``phase``, and
an integer ``priority`` (lower runs first). Registration typically happens
at import time from a toolkit's ``guards.py``.
"""

from __future__ import annotations

import fnmatch
from dataclasses import dataclass
from typing import Literal

from tools.core.guards.types import PostToolGuard, PreToolGuard

Phase = Literal["pre", "post"]


@dataclass(frozen=True)
class GuardEntry:
    """One registered guard."""

    tool_glob: str
    phase: Phase
    priority: int
    guard: PreToolGuard | PostToolGuard
    name: str


class ToolGuardRegistry:
    """Mutable, ordered collection of tool guards."""

    def __init__(self) -> None:
        self._pre: list[GuardEntry] = []
        self._post: list[GuardEntry] = []

    def register(
        self,
        tool_glob: str,
        phase: Phase,
        priority: int,
        guard: PreToolGuard | PostToolGuard,
        *,
        name: str | None = None,
    ) -> None:
        """Register ``guard`` against tools whose name matches ``tool_glob``."""
        resolved_name = name or getattr(guard, "__name__", repr(guard))
        entry = GuardEntry(
            tool_glob=tool_glob,
            phase=phase,
            priority=priority,
            guard=guard,
            name=resolved_name,
        )
        bucket = self._pre if phase == "pre" else self._post
        bucket.append(entry)
        bucket.sort(key=lambda e: (e.priority, e.name))

    def matching(self, tool_name: str, phase: Phase) -> list[GuardEntry]:
        """Return guards whose glob matches ``tool_name`` for ``phase``."""
        bucket = self._pre if phase == "pre" else self._post
        return [e for e in bucket if fnmatch.fnmatchcase(tool_name, e.tool_glob)]

    def clear(self) -> None:
        """Remove all registrations. Primarily for tests."""
        self._pre.clear()
        self._post.clear()


_DEFAULT_REGISTRY = ToolGuardRegistry()


def default_registry() -> ToolGuardRegistry:
    """Return the process-global registry."""
    return _DEFAULT_REGISTRY
