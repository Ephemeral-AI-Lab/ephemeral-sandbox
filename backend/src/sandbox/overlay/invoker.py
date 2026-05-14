"""Runtime invocation for one leased snapshot overlay shell call."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import replace
from pathlib import Path
from typing import Any, Protocol, runtime_checkable
from uuid import uuid4

from sandbox.layer_stack.manifest import Manifest
from sandbox.overlay.request import OverlayShellRequest
from sandbox.overlay.result import OverlayCapture
from sandbox.overlay.worker import execute_request
from sandbox.async_bridge import run_sync_in_executor
from sandbox.timing import monotonic_now


@runtime_checkable
class OverlayInvoker(Protocol):
    async def invoke(
        self,
        *,
        request: OverlayShellRequest,
        manifest: Manifest,
    ) -> OverlayCapture: ...

    def invoke_sync(
        self,
        *,
        request: OverlayShellRequest,
        manifest: Manifest,
    ) -> OverlayCapture: ...


class OverlayRuntimeInvoker:
    """Invoke the runtime-local overlay shell command and return its capture."""

    def __init__(
        self,
        *,
        storage_root: str | Path,
        runtime_root: str | Path | None = None,
    ) -> None:
        self.storage_root = Path(storage_root)
        self.runtime_root = Path(runtime_root) if runtime_root is not None else (
            self.storage_root / "runtime" / "overlay_shell"
        )

    async def invoke(
        self,
        *,
        request: OverlayShellRequest,
        manifest: Manifest,
    ) -> OverlayCapture:
        run_dir = self._run_dir(request)
        invoke_start = monotonic_now()
        capture, worker_start, worker_elapsed = await run_sync_in_executor(
            _execute_request_with_timings,
            request_payload=request.to_dict(),
            manifest_payload=manifest.to_dict(),
            storage_root=self.storage_root,
            run_dir=run_dir,
        )
        invoke_elapsed = monotonic_now() - invoke_start
        return _with_invoker_timings(
            capture,
            invoke_elapsed=invoke_elapsed,
            invoke_start=invoke_start,
            worker_start=worker_start,
            worker_elapsed=worker_elapsed,
        )

    def invoke_sync(
        self,
        *,
        request: OverlayShellRequest,
        manifest: Manifest,
    ) -> OverlayCapture:
        run_dir = self._run_dir(request)
        invoke_start = monotonic_now()
        capture, worker_start, worker_elapsed = _execute_request_with_timings(
            request_payload=request.to_dict(),
            manifest_payload=manifest.to_dict(),
            storage_root=self.storage_root,
            run_dir=run_dir,
        )
        invoke_elapsed = monotonic_now() - invoke_start
        return _with_invoker_timings(
            capture,
            invoke_elapsed=invoke_elapsed,
            invoke_start=invoke_start,
            worker_start=worker_start,
            worker_elapsed=worker_elapsed,
        )

    def _run_dir(self, request: OverlayShellRequest) -> Path:
        # ``OverlayShellRequest`` already rejects empty ids. This second pass
        # keeps runtime paths safe even if request-id rules are loosened later.
        safe_id = "".join(
            char if char.isalnum() or char in ("-", "_") else "-"
            for char in request.request_id
        ).strip("-")
        suffix = uuid4().hex[:8]
        return self.runtime_root / f"{safe_id or 'request'}-{suffix}"


def _execute_request_with_timings(
    *,
    request_payload: Mapping[str, Any],
    manifest_payload: Mapping[str, Any],
    storage_root: Path,
    run_dir: Path,
) -> tuple[OverlayCapture, float, float]:
    worker_start = monotonic_now()
    capture = execute_request(
        request_payload=request_payload,
        manifest_payload=manifest_payload,
        storage_root=storage_root,
        run_dir=run_dir,
    )
    return capture, worker_start, monotonic_now() - worker_start


def _with_invoker_timings(
    capture: OverlayCapture,
    *,
    invoke_elapsed: float,
    invoke_start: float,
    worker_start: float,
    worker_elapsed: float,
) -> OverlayCapture:
    return replace(
        capture,
        timings={
            **dict(capture.timings),
            "overlay.invoker.queue_wait_s": _queue_wait_s(
                worker_start,
                invoke_start,
            ),
            "overlay.invoker.worker_total_s": worker_elapsed,
            "overlay.invoker.resume_wait_s": _resume_wait_s(
                invoke_elapsed,
                worker_start=worker_start,
                invoke_start=invoke_start,
                worker_elapsed=worker_elapsed,
            ),
            "overlay.invoker.total_s": invoke_elapsed,
        },
    )


def _queue_wait_s(worker_start: float, invoke_start: float) -> float:
    return max(0.0, worker_start - invoke_start)


def _resume_wait_s(
    invoke_elapsed: float,
    *,
    worker_start: float,
    invoke_start: float,
    worker_elapsed: float,
) -> float:
    queue_wait = _queue_wait_s(worker_start, invoke_start)
    non_worker_elapsed = max(0.0, invoke_elapsed - worker_elapsed)
    return max(0.0, non_worker_elapsed - queue_wait)


__all__ = [
    "OverlayInvoker",
    "OverlayRuntimeInvoker",
]
