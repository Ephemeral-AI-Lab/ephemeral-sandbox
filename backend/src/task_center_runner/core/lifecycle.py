"""LifecycleHooks Protocol + ``NoopLifecycle`` default.

``run_pipeline`` calls four hooks on a single ``LifecycleHooks`` object:

- ``before_run(ctx)``      — once at startup, after stores and bus exist
- ``on_event(event)``      — subscribed to the audit bus; fires per event
- ``after_run(ctx, report)`` — once after the recorder is disposed; may
  mutate ``report.lifecycle_extras`` (for example, SweevoLifecycle writes
  ``sweevo_result``)
- ``on_aborted(ctx, reason)`` — fired when the run times out

``NoopLifecycle`` is the default for runs that do not need lifecycle
observation (real-LLM freeform runs).
"""

from __future__ import annotations

from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from task_center_runner.audit.events import Event
    from task_center_runner.core.config import RunContext
    from task_center_runner.core.report import PipelineReport


class LifecycleHooks(Protocol):
    """Per-mode hook surface assembled by adapters (scenario, sweevo, ...)."""

    async def before_run(self, ctx: "RunContext") -> None: ...

    def on_event(self, event: "Event") -> None: ...

    async def after_run(self, ctx: "RunContext", report: "PipelineReport") -> None: ...

    async def on_aborted(self, ctx: "RunContext", reason: str) -> None: ...


class NoopLifecycle:
    """Default ``LifecycleHooks`` implementation that does nothing."""

    async def before_run(self, ctx: "RunContext") -> None:
        return None

    def on_event(self, event: "Event") -> None:
        return None

    async def after_run(self, ctx: "RunContext", report: "PipelineReport") -> None:
        return None

    async def on_aborted(self, ctx: "RunContext", reason: str) -> None:
        return None


__all__ = ["LifecycleHooks", "NoopLifecycle"]
