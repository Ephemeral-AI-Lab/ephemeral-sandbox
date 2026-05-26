"""``PipelineReport`` — the unified return shape of ``run_pipeline``.

Intentionally narrower than the legacy ``RunReport`` (which carries mock-only
fields like ``launches``/``tool_calls``/``prompt_inspections``/``sandbox_checks``).
Mock side-channels travel through ``MOCK_*`` audit events instead — see
``task_center_runner.audit.events`` for the four enum values; ``run_scenario``
uses ``ScenarioLifecycle``'s accumulated records when assembling its
``RunReport`` view.

The ``performance_report_task`` field is populated when the engine spawns
performance-report writing as a background ``asyncio.Task``.
"""

from __future__ import annotations

import asyncio
from collections.abc import Mapping
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Literal


@dataclass(slots=True)
class PipelineReport:
    """Result returned by ``run_pipeline``; lifecycle hooks may mutate extras."""

    status: Literal["completed", "aborted"]
    task_center_run_id: str
    request_id: str
    sandbox_id: str
    instance_id: str
    run_dir: Path
    task_center_status: str | None
    duration_s: float
    task_count: int
    tasks_completed: int
    tasks_failed: int
    metrics: Mapping[str, Any]
    aborted_by_timeout: bool
    lifecycle_extras: dict[str, Any] = field(default_factory=dict)
    performance_report_task: asyncio.Task[Path] | None = None


__all__ = ["PipelineReport"]
