"""Audit helpers for workspace lifecycle operations."""

from __future__ import annotations

from contextlib import asynccontextmanager
from typing import AsyncIterator

from audit.jsonl import append_jsonl_event
from sandbox._shared.clock import monotonic_now
from sandbox.audit import events


@asynccontextmanager
async def lifecycle_operation(
    *,
    kind: str,
    agent_id: str,
    audit_path: str | None = None,
) -> AsyncIterator[dict[str, float]]:
    timings: dict[str, float] = {}
    started = monotonic_now()
    _emit(
        audit_path,
        events.WORKSPACE_LIFECYCLE_STARTED,
        {"kind": kind, "agent_id": agent_id},
    )
    try:
        yield timings
    except Exception as exc:
        timings["workspace_lifecycle.total_s"] = monotonic_now() - started
        _emit(
            audit_path,
            events.WORKSPACE_LIFECYCLE_FAILED,
            {
                "kind": kind,
                "agent_id": agent_id,
                "error": type(exc).__name__,
                "message": str(exc),
                "timings": dict(timings),
            },
        )
        raise
    else:
        timings["workspace_lifecycle.total_s"] = monotonic_now() - started
        _emit(
            audit_path,
            events.WORKSPACE_LIFECYCLE_COMPLETED,
            {"kind": kind, "agent_id": agent_id, "timings": dict(timings)},
        )


def emit_lifecycle_batch_rejected(
    *,
    lifecycle_tool: str,
    sibling_tools: tuple[str, ...],
    agent_id: str,
    audit_path: str | None = None,
) -> None:
    """Record an engine-side batch rejection in the lifecycle audit stream.

    Phase 4 §AC6: the engine refuses to dispatch ``Intent.LIFECYCLE`` calls
    co-batched with siblings (or other lifecycle calls). The rejection is
    recorded next to enter/exit events so trace bundles capture the cause
    of the missing dispatch.
    """
    _emit(
        audit_path,
        events.WORKSPACE_LIFECYCLE_BATCH_REJECTED,
        {
            "lifecycle_tool": lifecycle_tool,
            "sibling_tools": list(sibling_tools),
            "sibling_count": len(sibling_tools),
            "agent_id": agent_id,
        },
    )


def _emit(path: str | None, event_type: str, payload: dict[str, object]) -> None:
    append_jsonl_event(path, {"type": event_type, "payload": payload})


__all__ = ["emit_lifecycle_batch_rejected", "lifecycle_operation"]
