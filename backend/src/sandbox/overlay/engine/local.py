"""Overlay execution engine.

The engine owns overlay capture only: lease lifecycle, runtime setup, command
execution, readback, cleanup, and timing. OCC policy is intentionally outside
this module.
"""

from __future__ import annotations

import asyncio
import logging
import posixpath
import time
import uuid
from collections.abc import Awaitable, Callable
from typing import Any

from sandbox.overlay.config import overlay_max_concurrent, overlay_upper_size_mb
from sandbox.overlay.engine.constants import RUN_DIR_PREFIX, WorkspaceFingerprint
from sandbox.overlay.engine.helpers import encode_command
from sandbox.overlay.engine.readback import OverlayReadbackMixin
from sandbox.overlay.engine.runner import OverlayRunnerMixin
from sandbox.overlay.types import (
    ConflictInfo,
    OverlayCapture,
    OverlayLease,
    OverlayPolicyReject,
    OverlayRunOutcome,
)

logger = logging.getLogger(__name__)


class LocalOverlayEngine(OverlayRunnerMixin, OverlayReadbackMixin):
    """Run one command under a fresh overlay namespace and capture upperdir."""

    def __init__(
        self,
        *,
        sandbox_id: str,
        workspace_root: str,
        exec_process: Callable[..., Awaitable[Any]] | None = None,
        max_concurrent: int | None = None,
        upper_size_mb: int | None = None,
        direct_runtime: bool = True,
    ) -> None:
        self._sandbox_id = sandbox_id
        self._workspace_root = workspace_root.rstrip("/")
        self._exec_process = exec_process or self._local_exec_process
        self._direct_runtime = direct_runtime
        self._semaphore = asyncio.Semaphore(
            max_concurrent if max_concurrent is not None else overlay_max_concurrent()
        )
        self._upper_size_mb = (
            upper_size_mb if upper_size_mb is not None else overlay_upper_size_mb()
        )
        self._script_upload_lock = asyncio.Lock()
        self._script_uploaded = False
        self._fingerprint_lock = asyncio.Lock()
        self._active_fingerprint_guards = 0
        self._last_workspace_fingerprint: WorkspaceFingerprint | None = None

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
    ) -> OverlayRunOutcome:
        """Run *command* under overlay and hand back an OCC-free outcome."""
        del run_id, agent_run_id, task_id, agent_id, description
        if self._direct_runtime and sandbox is None and on_progress_line is None:
            return await self._execute_direct_runtime(
                command,
                timeout=timeout,
                stdin=stdin,
            )

        async with self._semaphore:
            lease = self._new_lease()
            stage_timings: dict[str, float] = {}
            total_started = time.perf_counter()
            outcome: OverlayRunOutcome | None = None
            error: BaseException | None = None
            try:
                await self._timed_stage(
                    "upload_runtime",
                    stage_timings=stage_timings,
                    lease=lease,
                    command=command,
                    awaitable=self._ensure_runtime_available(sandbox),
                )
                outcome = await self._run_and_assemble_outcome(
                    sandbox=sandbox,
                    command=command,
                    lease=lease,
                    stage_timings=stage_timings,
                    timeout=timeout,
                    stdin=stdin,
                    on_progress_line=on_progress_line,
                )
                return outcome
            except BaseException as exc:
                error = exc
                raise
            finally:
                try:
                    await self._timed_stage(
                        "cleanup",
                        stage_timings=stage_timings,
                        lease=lease,
                        command=command,
                        awaitable=self._cleanup_run_dir(sandbox, lease),
                    )
                except Exception:
                    logger.debug(
                        "overlay run-dir cleanup failed for %s",
                        lease.run_dir,
                        exc_info=True,
                    )
                stage_timings["total"] = round(time.perf_counter() - total_started, 6)
                if outcome is not None:
                    outcome.overlay_stage_timings = dict(stage_timings)
                self._log_execution_summary(
                    command=command,
                    lease=lease,
                    stage_timings=stage_timings,
                    outcome=outcome,
                    error=error,
                )

    async def _execute_direct_runtime(
        self,
        command: str,
        *,
        timeout: int | None,
        stdin: str | None,
    ) -> OverlayRunOutcome:
        async with self._semaphore:
            lease = self._new_lease()
            stage_timings: dict[str, float] = {}
            total_started = time.perf_counter()
            outcome: OverlayRunOutcome | None = None
            error: BaseException | None = None
            fingerprint_guard_started = False
            try:
                await self._begin_workspace_fingerprint_guard()
                fingerprint_guard_started = True
                await self._timed_stage(
                    "upload_runtime",
                    stage_timings=stage_timings,
                    lease=lease,
                    command=command,
                    awaitable=self._ensure_runtime_available(None),
                )
                user_cmd_b64, stdin_b64 = encode_command(command, stdin)
                overlay_stdout, script_exit = await self._timed_stage(
                    "unshare",
                    stage_timings=stage_timings,
                    lease=lease,
                    command=command,
                    awaitable=self._run_overlay_direct_runtime(
                        lease=lease,
                        user_cmd_b64=user_cmd_b64,
                        stdin_b64=stdin_b64,
                        timeout=timeout,
                    ),
                )
                await self._timed_stage(
                    "read_envelope",
                    stage_timings=stage_timings,
                    lease=lease,
                    command=command,
                    awaitable=self._read_result_envelope(
                        lease,
                        overlay_stdout=overlay_stdout,
                        overlay_exit_code=script_exit,
                    ),
                )
                outcome = await self._finish_outcome(
                    sandbox=None,
                    command=command,
                    lease=lease,
                    stage_timings=stage_timings,
                    overlay_stdout=overlay_stdout,
                    script_exit=script_exit,
                )
                return outcome
            except BaseException as exc:
                error = exc
                raise
            finally:
                try:
                    await self._timed_stage(
                        "cleanup",
                        stage_timings=stage_timings,
                        lease=lease,
                        command=command,
                        awaitable=self._cleanup_run_dir(None, lease),
                    )
                except OSError:
                    logger.warning(
                        "overlay direct-runtime run-dir cleanup failed for %s",
                        lease.run_dir,
                        exc_info=True,
                    )
                except Exception:
                    logger.debug(
                        "overlay direct-runtime run-dir cleanup failed for %s",
                        lease.run_dir,
                        exc_info=True,
                    )
                stage_timings["total"] = round(time.perf_counter() - total_started, 6)
                if outcome is not None:
                    outcome.overlay_stage_timings = dict(stage_timings)
                if fingerprint_guard_started:
                    await self._end_workspace_fingerprint_guard()
                self._log_execution_summary(
                    command=command,
                    lease=lease,
                    stage_timings=stage_timings,
                    outcome=outcome,
                    error=error,
                )

    async def _run_and_assemble_outcome(
        self,
        *,
        sandbox: Any,
        command: str,
        lease: OverlayLease,
        stage_timings: dict[str, float],
        timeout: int | None,
        stdin: str | None,
        on_progress_line: Callable[[str], None] | None,
    ) -> OverlayRunOutcome:
        user_cmd_b64, stdin_b64 = encode_command(command, stdin)
        if on_progress_line is None:
            stdout_text, script_exit = await self._timed_stage(
                "run_overlay",
                stage_timings=stage_timings,
                lease=lease,
                command=command,
                awaitable=self._run_overlay(
                    sandbox,
                    lease=lease,
                    user_cmd_b64=user_cmd_b64,
                    stdin_b64=stdin_b64,
                    timeout=timeout,
                ),
            )
        else:
            stdout_text, script_exit = await self._timed_stage(
                "run_overlay",
                stage_timings=stage_timings,
                lease=lease,
                command=command,
                awaitable=self._run_overlay_with_progress(
                    sandbox,
                    lease=lease,
                    user_cmd_b64=user_cmd_b64,
                    stdin_b64=stdin_b64,
                    timeout=timeout,
                    on_progress_line=on_progress_line,
                ),
            )
        return await self._finish_outcome(
            sandbox=sandbox,
            command=command,
            lease=lease,
            stage_timings=stage_timings,
            overlay_stdout=stdout_text,
            script_exit=script_exit,
        )

    async def _finish_outcome(
        self,
        *,
        sandbox: Any,
        command: str,
        lease: OverlayLease,
        stage_timings: dict[str, float],
        overlay_stdout: str,
        script_exit: int,
    ) -> OverlayRunOutcome:
        stdout_text = await self._timed_stage(
            "read_stdout",
            stage_timings=stage_timings,
            lease=lease,
            command=command,
            awaitable=self._read_stdout(sandbox, lease, fallback=overlay_stdout),
        )
        diff_or_reject = await self._timed_stage(
            "read_diff",
            stage_timings=stage_timings,
            lease=lease,
            command=command,
            awaitable=self._read_diff(
                sandbox,
                lease,
                overlay_stdout=stdout_text,
                overlay_exit_code=script_exit,
            ),
        )
        if isinstance(diff_or_reject, OverlayPolicyReject):
            return self._reject_outcome(
                stdout=stdout_text,
                exit_code=script_exit,
                reject=diff_or_reject,
            )
        return self._assemble_outcome(stdout=stdout_text, diff=diff_or_reject)

    def _assemble_outcome(
        self,
        *,
        stdout: str,
        diff: OverlayCapture,
    ) -> OverlayRunOutcome:
        return OverlayRunOutcome(
            exit_code=diff.exit_code,
            stdout=stdout,
            upper_changes=diff.upper_changes,
            overlay_rejected=False,
            conflict=None,
            warnings=tuple(diff.warnings),
            overlay_run_timings=dict(diff.run_timings),
            policy_reject=None,
        )

    def _reject_outcome(
        self,
        *,
        stdout: str,
        exit_code: int,
        reject: OverlayPolicyReject,
    ) -> OverlayRunOutcome:
        detail = (
            f"{reject.reason}: {','.join(reject.paths)}"
            if reject.paths
            else reject.reason
        )
        conflict = ConflictInfo(
            reason=reject.reason,
            conflict_file=reject.paths[0] if reject.paths else None,
            message=detail,
        )
        return OverlayRunOutcome(
            exit_code=exit_code,
            stdout=stdout,
            upper_changes=(),
            overlay_rejected=True,
            conflict=conflict,
            warnings=(detail,),
            overlay_run_timings=dict(reject.run_timings),
            policy_reject=reject,
        )

    def _new_lease(self) -> OverlayLease:
        run_dir = posixpath.join(
            RUN_DIR_PREFIX, self._sandbox_id, f"run-{uuid.uuid4().hex}"
        )
        return OverlayLease(run_dir=run_dir)


__all__ = ["LocalOverlayEngine"]
