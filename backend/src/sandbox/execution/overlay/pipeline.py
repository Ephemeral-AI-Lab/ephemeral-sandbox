"""Overlay-shell pipeline: factory + invoker + user-command stage.

Three stages of one runtime call, collapsed into a single module per the
sandbox-reframe RFC §4 Wave 2:

- ``create_overlay_invoker`` (factory) wires a default
  :class:`OverlayRuntimeInvoker` from a :class:`LayerStackManager`.
- :class:`OverlayInvoker` / :class:`OverlayRuntimeInvoker` (invoker) drives
  the worker process for one leased snapshot, stamping invoker-side
  timings on the returned capture.
- :func:`run_user_command` / :class:`OverlayCommandResult` (command) is
  the in-worker subprocess wrapper that ultimately runs the user shell
  command inside the mounted workspace.

Behavior is preserved verbatim from the prior
``overlay/{factory,invoker,command}.py`` trio; only physical layout
changed.
"""

from __future__ import annotations

import os
import subprocess
from collections.abc import Mapping
from dataclasses import dataclass, replace
from pathlib import Path
from typing import Any, Protocol, runtime_checkable
from uuid import uuid4

from sandbox.execution.overlay.request import OverlayShellRequest
from sandbox.execution.overlay.result import OverlayCapture
from sandbox.layer_stack.manager import LayerStackManager
from sandbox.layer_stack.manifest import Manifest
from sandbox.runtime.async_bridge import run_sync_in_executor
from sandbox.timing import monotonic_now

# Deferred import to break the
# pipeline -> worker -> pipeline (run_user_command) cycle introduced by
# merging factory + invoker + command into this module.


# Host env vars that the user command needs to function (PATH for argv0
# resolution, HOME/TERM for shells, locale vars for tooling that branches on
# encoding). Host secrets are intentionally absent from this allow-list.
_HOST_ENV_ALLOWLIST: tuple[str, ...] = (
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "TERM",
    "TZ",
)


@dataclass(frozen=True)
class OverlayCommandResult:
    exit_code: int
    stdout_ref: str
    stderr_ref: str


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
        safe_id = "".join(
            char if char.isalnum() or char in ("-", "_") else "-"
            for char in request.request_id
        ).strip("-")
        suffix = uuid4().hex[:8]
        return self.runtime_root / f"{safe_id or 'request'}-{suffix}"


def create_overlay_invoker(layer_stack: LayerStackManager) -> OverlayInvoker:
    return OverlayRuntimeInvoker(storage_root=layer_stack.storage_root)


def run_user_command(
    *,
    command: tuple[str, ...],
    workspace_root: str | Path,
    cwd: str,
    env: dict[str, str],
    timeout_seconds: float | None,
    stdout_ref: str | Path,
    stderr_ref: str | Path,
) -> OverlayCommandResult:
    root = Path(workspace_root)
    resolved_cwd = _validate_cwd(root, cwd)
    _ensure_cwd(resolved_cwd)
    stdout_path = Path(stdout_ref)
    stderr_path = Path(stderr_ref)
    stdout_path.parent.mkdir(parents=True, exist_ok=True)
    stderr_path.parent.mkdir(parents=True, exist_ok=True)

    base_env = {
        key: os.environ[key]
        for key in _HOST_ENV_ALLOWLIST
        if key in os.environ
    }
    child_env = {**base_env, **env, "GIT_OPTIONAL_LOCKS": "0"}

    with stdout_path.open("wb") as stdout_file, stderr_path.open("wb") as stderr_file:
        try:
            completed = subprocess.run(
                list(command),
                cwd=resolved_cwd,
                env=child_env,
                stdout=stdout_file,
                stderr=stderr_file,
                timeout=timeout_seconds,
                check=False,
            )
            exit_code = int(completed.returncode)
        except subprocess.TimeoutExpired:
            # 124 follows the GNU `timeout(1)` convention so callers can
            # distinguish a user-command timeout from infrastructure failure.
            exit_code = 124
    return OverlayCommandResult(
        exit_code=exit_code,
        stdout_ref=str(stdout_path),
        stderr_ref=str(stderr_path),
    )


def _execute_request_with_timings(
    *,
    request_payload: Mapping[str, Any],
    manifest_payload: Mapping[str, Any],
    storage_root: Path,
    run_dir: Path,
) -> tuple[OverlayCapture, float, float]:
    from sandbox.execution.overlay.worker import execute_request

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


def _validate_cwd(workspace_root: Path, cwd: str) -> Path:
    root = workspace_root.resolve()
    candidate = Path(cwd)
    if not candidate.is_absolute():
        candidate = root / candidate
    resolved = candidate.resolve()
    if os.path.commonpath([str(root), str(resolved)]) != str(root):
        raise ValueError(f"cwd escapes mounted workspace: {cwd!r}")
    return resolved


def _ensure_cwd(resolved_cwd: Path) -> None:
    resolved_cwd.mkdir(parents=True, exist_ok=True)


__all__ = [
    "OverlayCommandResult",
    "OverlayInvoker",
    "OverlayRuntimeInvoker",
    "create_overlay_invoker",
    "run_user_command",
]
