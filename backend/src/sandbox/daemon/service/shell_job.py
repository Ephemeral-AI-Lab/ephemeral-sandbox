"""Daemon-native background shell job control.

A :class:`ShellJob` owns one ``shell(background=True)`` invocation end-to-end on
the daemon side: the layer-stack lease, the child process group, the per-job
upperdir, and the post-run OCC publish. The engine wraps each job in a thin
asyncio polling task; the daemon owns the heavy lifting so that lease release
and process termination survive RPC disconnect or engine process kill.

Plan: ``docs/plans/2026-05-22-shell-background-mode.md`` (Option B).
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import logging
import os
import shutil
import signal
import threading
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable
from uuid import uuid4

from sandbox.audit import events as audit_events
from sandbox.execution.contract import (
    AnyOverlayLayout,
    CommandExecRequest,
    LayerPathsLayout,
    MountMode,
    OverlayLayout,
)
from sandbox.execution.overlay.capability import new_mount_api_supported
from sandbox.execution.path_change import OverlayPathChange
from sandbox.execution.runner import run_workspace_replaced_command
from sandbox.execution.scratch import command_exec_scratch_root
from sandbox.daemon.service.sandbox_overlay import (
    OperationOverlayHandle,
    SandboxOverlay,
)
from sandbox._shared.clock import monotonic_now

logger = logging.getLogger(__name__)


# Default idle-since-last-poll threshold before the reaper assumes the host has
# abandoned a job. 5 minutes matches a typical agent's inter-tool latency
# upper bound; configurable via the ShellJobRegistry constructor.
DEFAULT_TTL_SECONDS = 300.0
DEFAULT_REAPER_INTERVAL_S = 30.0
# Grace window between SIGTERM and SIGKILL escalation on cancel.
SIGTERM_GRACE_S = 2.0
# Default ShellExecutor sizing. Sized small (4x typical sandbox concurrency)
# so cancel fan-out doesn't exhaust the executor (Pre-mortem #4).
DEFAULT_EXECUTOR_WORKERS = 64
# Tail size for in-flight progress on ``shell.poll``.
_PROGRESS_TAIL_BYTES = 4096


class ShellJobNotFound(KeyError):
    """``shell.*`` RPC referenced a job_id that has been reaped or never existed."""


@dataclass
class ShellJob:
    """One background shell invocation: lease + child PG + upperdir + result."""

    job_id: str
    request: CommandExecRequest
    overlay: SandboxOverlay
    handle: OperationOverlayHandle
    storage_root: Path
    started_at: float
    last_poll_at: float
    cancel_event: threading.Event = field(default_factory=threading.Event)
    process_done: threading.Event = field(default_factory=threading.Event)
    cancelled: bool = False
    cancel_reason: str = ""
    released: bool = False
    pgrp: int = 0
    exit_code: int | None = None
    stdout_ref: str = ""
    stderr_ref: str = ""
    mount_mode: MountMode | None = None
    timings: dict[str, float] = field(default_factory=dict)
    error: str | None = None
    thread_future: concurrent.futures.Future | None = None

    @property
    def status(self) -> str:
        """Single source of truth for terminal status precedence.

        Order: ``running`` < ``cancelling`` < ``cancelled`` < (``finished`` |
        ``failed``). The plan requires completed > failed > cancelled when the
        race goes to a natural exit; we encode that by checking
        ``process_done`` first when ``cancelled`` is set.
        """
        if not self.process_done.is_set():
            return "cancelling" if self.cancelled else "running"
        if self.cancelled and self.exit_code is None:
            # Process never exited successfully — cancel signal terminated it.
            return "cancelled"
        if self.cancelled and self.exit_code is not None and self.exit_code != 0:
            return "cancelled"
        if self.error is not None:
            return "failed"
        if self.exit_code is None or self.exit_code != 0:
            return "failed"
        return "finished"


class ShellJobRegistry:
    """Daemon-side registry tracking background shell jobs.

    Lifecycle: ``launch`` -> ``poll`` * -> (``cancel``) ? -> ``reap``.
    A TTL reaper releases leases for jobs that nobody has polled in
    ``ttl_seconds`` (default 5 min) so a host crash cannot leak quota.
    """

    def __init__(
        self,
        *,
        executor: concurrent.futures.ThreadPoolExecutor | None = None,
        ttl_seconds: float = DEFAULT_TTL_SECONDS,
        reaper_interval_s: float = DEFAULT_REAPER_INTERVAL_S,
        audit_callback: Callable[[str, dict[str, object]], None] | None = None,
    ) -> None:
        self._jobs: dict[str, ShellJob] = {}
        self._lock = threading.RLock()
        self._executor = executor or concurrent.futures.ThreadPoolExecutor(
            max_workers=DEFAULT_EXECUTOR_WORKERS,
            thread_name_prefix="shell-job",
        )
        self._owns_executor = executor is None
        self._ttl_seconds = float(ttl_seconds)
        self._reaper_interval_s = float(reaper_interval_s)
        self._audit_callback = audit_callback
        self._reaper_task: asyncio.Task[None] | None = None
        # Lifetime counter for TTL-reaped jobs. Surfaced via :meth:`metrics`
        # and the ``api.shell.metrics`` RPC so AC-13 (engine-kill TTL reap)
        # is observable after the engine that drove the launch is dead.
        self._ttl_reaped_total: int = 0

    # ---- public RPC surface ------------------------------------------------

    def launch(
        self,
        *,
        request: CommandExecRequest,
        overlay: SandboxOverlay,
        storage_root: Path,
    ) -> dict[str, object]:
        """Acquire a lease, fork the strategy in the executor, return a job id."""
        job_id = f"shell-{uuid4().hex[:12]}"
        handle = overlay.acquire_operation_overlay(
            request_id=request.request_id,
            materialize=not new_mount_api_supported(),
        )
        now = monotonic_now()
        job = ShellJob(
            job_id=job_id,
            request=request,
            overlay=overlay,
            handle=handle,
            storage_root=storage_root,
            started_at=now,
            last_poll_at=now,
        )
        with self._lock:
            self._jobs[job_id] = job
        try:
            future = self._executor.submit(self._run_strategy, job)
        except RuntimeError:
            handle.release()
            with self._lock:
                self._jobs.pop(job_id, None)
            raise
        job.thread_future = future
        self._ensure_reaper_started()
        self._emit_audit(
            audit_events.SHELL_LAUNCHED,
            {
                "job_id": job_id,
                "lease_id": handle.lease_id,
                "request_id": request.request_id,
            },
        )
        return {
            "job_id": job_id,
            "lease_id": handle.lease_id,
            "started_at": now,
        }

    def poll(self, job_id: str) -> dict[str, object]:
        job = self._get(job_id)
        job.last_poll_at = monotonic_now()
        snapshot = {
            "job_id": job_id,
            "status": job.status,
            "exit_code": job.exit_code,
            "stdout_tail": _read_tail(job.stdout_ref, _PROGRESS_TAIL_BYTES),
            "stderr_tail": _read_tail(job.stderr_ref, _PROGRESS_TAIL_BYTES),
            "pid_alive": _pgrp_alive(job.pgrp),
            "cancelled": job.cancelled,
        }
        self._emit_audit(
            audit_events.SHELL_POLLED,
            {
                "job_id": job_id,
                "status": job.status,
                "exit_code": job.exit_code,
            },
        )
        return snapshot

    def cancel(self, job_id: str, *, reason: str = "") -> dict[str, object]:
        """Signal-cancel an in-flight job; idempotent against late-cancel races.

        The check-and-set is scoped to ``self._lock`` so the late-cancel race
        (process exits between ``is_set()`` and the cancel mutation) resolves
        deterministically: if the process beat us, ``cancelled`` stays False
        and no ``SHELL_CANCELLED`` audit event fires (AC-5 invariant).
        """
        job = self._get(job_id)
        with self._lock:
            if job.process_done.is_set():
                return {
                    "job_id": job_id,
                    "cancelled": False,
                    "already_done": True,
                    "status": job.status,
                }
            if job.cancelled:
                return {
                    "job_id": job_id,
                    "cancelled": True,
                    "already_cancelled": True,
                }
            job.cancelled = True
            job.cancel_reason = str(reason or "")
            job.cancel_event.set()
        # Signal + audit happen outside the lock: SIGTERM may block the
        # thread briefly and audit callbacks should never serialize callers.
        if job.pgrp:
            _signal_pgrp(job.pgrp, signal.SIGTERM)
        timer = threading.Timer(SIGTERM_GRACE_S, _escalate_kill, args=(job,))
        timer.daemon = True
        timer.start()
        self._emit_audit(
            audit_events.SHELL_CANCELLED,
            {"job_id": job_id, "reason": job.cancel_reason},
        )
        return {"job_id": job_id, "cancelled": True, "already_cancelled": False}

    async def reap(
        self,
        job_id: str,
        *,
        timeout_seconds: float = 300.0,
    ) -> dict[str, object]:
        """Wait for the strategy thread, conditionally publish, release lease."""
        job = self._get(job_id)
        await self._await_process_done(job, timeout_seconds)

        path_changes: tuple[OverlayPathChange, ...] = ()
        if not job.cancelled and job.error is None and job.exit_code is not None:
            publish = await job.overlay.publish_cycle(
                request=job.request,
                upperdir=job.handle.upperdir,
                snapshot=job.handle.manifest,
                run_maintenance=False,
            )
            path_changes = tuple(publish.path_changes)
            job.timings.update(publish.timings)
            maintenance_timings = await job.overlay.run_maintenance_after_publish(
                publish.changeset,
                workspace_ref=job.request.workspace_ref,
            )
            job.timings.update(maintenance_timings)

        # Idempotent release via OperationOverlayHandle._released.
        job.handle.release()
        job.released = True
        shutil.rmtree(job.handle.run_dir, ignore_errors=True)
        with self._lock:
            self._jobs.pop(job_id, None)

        payload = {
            "job_id": job_id,
            "status": job.status,
            "exit_code": job.exit_code if job.exit_code is not None else -1,
            "stdout": _read_full(job.stdout_ref),
            "stderr": _read_full(job.stderr_ref),
            "changed_paths": [_change_path(c) for c in path_changes],
            "timings": dict(job.timings),
            "error": job.error,
        }
        self._emit_audit(
            audit_events.SHELL_REAPED,
            {
                "job_id": job_id,
                "status": job.status,
                "changed_paths_count": len(path_changes),
            },
        )
        return payload

    def get(self, job_id: str) -> ShellJob | None:
        with self._lock:
            return self._jobs.get(job_id)

    def active_count(self) -> int:
        with self._lock:
            return len(self._jobs)

    def metrics(self) -> dict[str, int]:
        """Snapshot of registry-wide counters for the ``api.shell.metrics`` RPC."""
        with self._lock:
            return {
                "active_jobs": len(self._jobs),
                "ttl_reaped_total": int(self._ttl_reaped_total),
            }

    def shutdown(self) -> None:
        if self._reaper_task is not None and not self._reaper_task.done():
            self._reaper_task.cancel()
        if self._owns_executor:
            self._executor.shutdown(wait=False, cancel_futures=True)

    # ---- internal --------------------------------------------------------

    def _get(self, job_id: str) -> ShellJob:
        with self._lock:
            job = self._jobs.get(job_id)
        if job is None:
            raise ShellJobNotFound(job_id)
        return job

    def _record_pgrp(self, job: ShellJob, pgrp: int) -> None:
        job.pgrp = int(pgrp)
        if job.cancel_event.is_set() and job.pgrp:
            # Cancel landed before the child was spawned; signal it now.
            _signal_pgrp(job.pgrp, signal.SIGTERM)

    def _run_strategy(self, job: ShellJob) -> None:
        try:
            spec = self._build_layout(job)
            process_result = run_workspace_replaced_command(
                spec=spec,
                request=job.request,
                run_dir=Path(job.handle.run_dir),
                timings=job.timings,
                cancel_event=job.cancel_event,
                pid_recorder=lambda pgid: self._record_pgrp(job, pgid),
            )
            job.exit_code = int(process_result.exit_code)
            job.stdout_ref = str(process_result.stdout_ref)
            job.stderr_ref = str(process_result.stderr_ref)
            job.mount_mode = process_result.mount_mode
        except Exception as exc:
            job.error = f"{type(exc).__name__}: {exc}"
            job.exit_code = -1
            logger.exception("shell job %s strategy failed", job.job_id)
        finally:
            job.process_done.set()

    def _build_layout(self, job: ShellJob) -> AnyOverlayLayout:
        scratch_root = command_exec_scratch_root(Path(job.storage_root))
        handle = job.handle
        if handle.layer_paths is not None:
            layer_storage_root = ""
            stack = getattr(job.overlay, "_layer_stack", None)
            if stack is not None and hasattr(stack, "storage_root"):
                layer_storage_root = str(stack.storage_root)
            return LayerPathsLayout(
                workspace_root=job.request.workspace_root,
                layer_paths=tuple(Path(p) for p in handle.layer_paths),
                layer_storage_root=layer_storage_root,
                writes=handle.upperdir,
                kernel_scratch=handle.workdir,
                scratch_root=str(scratch_root),
            )
        return OverlayLayout(
            workspace_root=job.request.workspace_root,
            base_repo=handle.lowerdir or "",
            writes=handle.upperdir,
            kernel_scratch=handle.workdir,
            scratch_root=str(scratch_root),
        )

    async def _await_process_done(
        self,
        job: ShellJob,
        timeout: float,
    ) -> None:
        loop = asyncio.get_event_loop()
        await loop.run_in_executor(
            None,
            lambda: job.process_done.wait(timeout=timeout),
        )
        if not job.process_done.is_set():
            if job.pgrp:
                _signal_pgrp(job.pgrp, signal.SIGKILL)
            await loop.run_in_executor(
                None,
                lambda: job.process_done.wait(timeout=5.0),
            )

    def _ensure_reaper_started(self) -> None:
        if self._reaper_task is not None and not self._reaper_task.done():
            return
        try:
            loop = asyncio.get_event_loop()
        except RuntimeError:
            return
        self._reaper_task = loop.create_task(self._reaper_loop_body())

    async def _reaper_loop_body(self) -> None:
        while True:
            try:
                await asyncio.sleep(self._reaper_interval_s)
            except asyncio.CancelledError:
                return
            try:
                self._reap_stale_jobs()
            except Exception:
                logger.exception("shell job reaper iteration failed")

    def _reap_stale_jobs(self) -> None:
        now = monotonic_now()
        with self._lock:
            stale = [
                job for job in self._jobs.values()
                if now - job.last_poll_at >= self._ttl_seconds
            ]
        for job in stale:
            logger.warning(
                "shell job %s reaper: forcing release after %.0fs of inactivity",
                job.job_id,
                now - job.last_poll_at,
            )
            if not job.cancelled:
                try:
                    self.cancel(job.job_id, reason="ttl_reaper")
                except ShellJobNotFound:
                    continue
            if job.pgrp and not job.process_done.is_set():
                _signal_pgrp(job.pgrp, signal.SIGKILL)
            # Skip OCC publish on TTL reap: by definition the host abandoned us
            # mid-flight, so the upperdir contents are not authoritative.
            job.handle.release()
            shutil.rmtree(job.handle.run_dir, ignore_errors=True)
            with self._lock:
                self._jobs.pop(job.job_id, None)
                self._ttl_reaped_total += 1
            self._emit_audit(
                audit_events.SHELL_REAPED,
                {
                    "job_id": job.job_id,
                    "status": "ttl_expired",
                    "changed_paths_count": 0,
                },
            )

    def _emit_audit(
        self,
        event_name: str,
        payload: dict[str, object],
    ) -> None:
        if self._audit_callback is None:
            return
        try:
            self._audit_callback(event_name, payload)
        except Exception:
            logger.exception("shell job audit emit failed event=%s", event_name)


def _signal_pgrp(pgrp: int, sig: int) -> None:
    try:
        os.killpg(pgrp, sig)
    except (ProcessLookupError, PermissionError):
        pass


def _escalate_kill(job: ShellJob) -> None:
    if job.process_done.is_set():
        return
    if job.pgrp:
        _signal_pgrp(job.pgrp, signal.SIGKILL)


def _pgrp_alive(pgrp: int) -> bool:
    if not pgrp:
        return False
    try:
        os.killpg(pgrp, 0)
        return True
    except (ProcessLookupError, PermissionError):
        return False


def _read_tail(ref: str, max_bytes: int) -> str:
    if not ref:
        return ""
    path = Path(ref)
    if not path.exists():
        return ""
    try:
        with path.open("rb") as f:
            try:
                f.seek(-max_bytes, os.SEEK_END)
            except OSError:
                pass  # file smaller than max_bytes
            return f.read().decode("utf-8", "replace")
    except OSError:
        return ""


def _read_full(ref: str) -> str:
    if not ref:
        return ""
    path = Path(ref)
    if not path.exists():
        return ""
    try:
        return path.read_bytes().decode("utf-8", "replace")
    except OSError:
        return ""


def _change_path(change: OverlayPathChange) -> str:
    return str(getattr(change, "path", change))


_REGISTRY: ShellJobRegistry | None = None
_REGISTRY_LOCK = threading.Lock()

# Env-var overrides for the singleton TTL + reaper-interval. Required so the
# T4 engine-kill integration test can drive the TTL reaper in a CI-bounded
# window (default 300 s is unusable). See Phase 2 plan §Step 4.
_ENV_TTL_S = "EOS_SHELL_JOB_TTL_S"
_ENV_REAPER_INTERVAL_S = "EOS_SHELL_JOB_REAPER_INTERVAL_S"


def _env_float_or_default(name: str, default: float) -> float:
    """Read a non-negative float from ``os.environ``; fall back on any parse failure.

    Empty string, missing key, unparseable value, and negative numbers all
    fall back to ``default``. Negative TTL/interval would either disable the
    reaper or fire it constantly — both are footguns we hide behind the
    default rather than surface as an exception.
    """
    raw = os.environ.get(name, "")
    if not raw:
        return default
    try:
        parsed = float(raw)
    except (TypeError, ValueError):
        return default
    if parsed < 0:
        return default
    return parsed


def get_shell_job_registry() -> ShellJobRegistry:
    """Return the process-local singleton registry (lazily constructed).

    Reads optional ``EOS_SHELL_JOB_TTL_S`` and
    ``EOS_SHELL_JOB_REAPER_INTERVAL_S`` env vars on first construction. Once
    built, the singleton persists for the daemon process lifetime; tests
    that want fresh values must call :func:`reset_shell_job_registry`.
    """
    global _REGISTRY
    with _REGISTRY_LOCK:
        if _REGISTRY is None:
            _REGISTRY = ShellJobRegistry(
                ttl_seconds=_env_float_or_default(_ENV_TTL_S, DEFAULT_TTL_SECONDS),
                reaper_interval_s=_env_float_or_default(
                    _ENV_REAPER_INTERVAL_S, DEFAULT_REAPER_INTERVAL_S
                ),
            )
        return _REGISTRY


def reset_shell_job_registry() -> None:
    """Test helper: drop the singleton so the next call constructs a fresh one."""
    global _REGISTRY
    with _REGISTRY_LOCK:
        if _REGISTRY is not None:
            _REGISTRY.shutdown()
        _REGISTRY = None


__all__ = [
    "DEFAULT_EXECUTOR_WORKERS",
    "DEFAULT_TTL_SECONDS",
    "ShellJob",
    "ShellJobNotFound",
    "ShellJobRegistry",
    "get_shell_job_registry",
    "reset_shell_job_registry",
]
