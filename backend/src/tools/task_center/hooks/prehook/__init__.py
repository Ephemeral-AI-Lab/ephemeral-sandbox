"""Task Center pre-hook registration."""

from __future__ import annotations

from tools.core.hooks import ToolHookRegistry
from tools.task_center.hooks.prehook import scout_file_note_coverage_policy

_MODULES = (scout_file_note_coverage_policy,)


def register_all(registry: ToolHookRegistry | None = None) -> None:
    for module in _MODULES:
        module.register(registry)
