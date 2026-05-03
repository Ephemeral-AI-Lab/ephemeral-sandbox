"""Overlay execution protocol."""

from __future__ import annotations

from collections.abc import Callable
from typing import Any, Protocol

from sandbox.overlay.types import OverlayRunOutcome


class OverlayEngine(Protocol):
    """Minimal overlay capture surface used by runtime pipelines."""

    async def execute(
        self,
        command: str,
        *,
        sandbox: Any = None,
        timeout: int | None = None,
        stdin: str | None = None,
        description: str = "",
        agent_id: str = "",
        run_id: str = "",
        agent_run_id: str = "",
        task_id: str = "",
        on_progress_line: Callable[[str], None] | None = None,
    ) -> OverlayRunOutcome: ...


__all__ = ["OverlayEngine"]
