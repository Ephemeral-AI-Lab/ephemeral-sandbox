"""Runtime invocation for one leased snapshot overlay shell call."""

from __future__ import annotations

import time
from collections.abc import Mapping
from dataclasses import replace
from pathlib import Path
from typing import Any
from uuid import uuid4

from sandbox.layer_stack.manifest import Manifest
from sandbox.overlay.capture.types import OverlayCapture
from sandbox.overlay.runner.snapshot_overlay_runner import (
    OverlayShellRequest,
    overlay_shell_request_to_dict,
)
from sandbox.overlay.cli import execute_request
from sandbox.runtime.async_bridge import run_sync_in_executor


class RuntimeInvoker:
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
        invoke_start = time.perf_counter()
        capture, worker_start, worker_elapsed = await run_sync_in_executor(
            _execute_request_with_timings,
            request_payload=overlay_shell_request_to_dict(request),
            manifest_payload=manifest.to_dict(),
            storage_root=self.storage_root,
            run_dir=run_dir,
        )
        invoke_elapsed = time.perf_counter() - invoke_start
        return replace(
            capture,
            timings={
                **capture.timings,
                "overlay.invoker.queue_wait_s": worker_start - invoke_start,
                "overlay.invoker.worker_total_s": worker_elapsed,
                "overlay.invoker.resume_wait_s": max(
                    0.0,
                    invoke_elapsed - (worker_start - invoke_start) - worker_elapsed,
                ),
                "overlay.invoker.total_s": invoke_elapsed,
            },
        )

    def invoke_sync(
        self,
        *,
        request: OverlayShellRequest,
        manifest: Manifest,
    ) -> OverlayCapture:
        run_dir = self._run_dir(request)
        invoke_start = time.perf_counter()
        capture, worker_start, worker_elapsed = _execute_request_with_timings(
            request_payload=overlay_shell_request_to_dict(request),
            manifest_payload=manifest.to_dict(),
            storage_root=self.storage_root,
            run_dir=run_dir,
        )
        invoke_elapsed = time.perf_counter() - invoke_start
        return replace(
            capture,
            timings={
                **capture.timings,
                "overlay.invoker.queue_wait_s": worker_start - invoke_start,
                "overlay.invoker.worker_total_s": worker_elapsed,
                "overlay.invoker.resume_wait_s": max(
                    0.0,
                    invoke_elapsed - (worker_start - invoke_start) - worker_elapsed,
                ),
                "overlay.invoker.total_s": invoke_elapsed,
            },
        )

    def _run_dir(self, request: OverlayShellRequest) -> Path:
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
    worker_start = time.perf_counter()
    capture = execute_request(
        request_payload=dict(request_payload),
        manifest_payload=dict(manifest_payload),
        storage_root=storage_root,
        run_dir=run_dir,
    )
    return capture, worker_start, time.perf_counter() - worker_start


__all__ = ["RuntimeInvoker"]
