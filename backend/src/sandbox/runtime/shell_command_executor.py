"""OCC-gated sandbox command execution for runtime services."""

from __future__ import annotations

import asyncio
import inspect
import logging
import subprocess
from collections.abc import Callable
from types import SimpleNamespace
from typing import Any

from sandbox.api.transport import SandboxTransport
from sandbox.client.async_bridge import run_sync_in_executor, use_sandbox_io_loop
from sandbox.overlay.engine import LocalOverlayEngine, OverlayEngine
from sandbox.overlay.types import ShellResult
from sandbox.runtime.pipelines import shell_pipeline

logger = logging.getLogger(__name__)


class AuditedCommandExecutor:
    """Runs sandbox commands through the runtime shell pipeline."""

    def __init__(
        self,
        *,
        sandbox_id: str,
        workspace_root: str,
        write_coordinator: Any,
        rebind_sandbox: Callable[[Any], None],
        transport: SandboxTransport | None = None,
        daemon_local: bool = False,
    ) -> None:
        self.sandbox_id = sandbox_id
        self.workspace_root = workspace_root
        self._write_coordinator = write_coordinator
        self._rebind_sandbox = rebind_sandbox
        self._transport = transport
        self._daemon_local = daemon_local
        self._overlay_engine: OverlayEngine | None = None
        self._init_lock = asyncio.Lock()

    async def cmd(
        self,
        sandbox: Any,
        command: str,
        *,
        timeout: int | None = None,
        description: str = "",
        agent_id: str = "",
        run_id: str = "",
        agent_run_id: str = "",
        task_id: str = "",
        stdin: str | None = None,
        attribute_changes: bool = True,
        on_progress_line: Callable[[str], None] | None = None,
    ) -> SimpleNamespace:
        """Run one command through the fail-closed OCC audit path."""
        del attribute_changes, run_id, agent_run_id, task_id
        self._rebind_sandbox(sandbox)
        overlay = await self._ensure_capture_runner()
        result = await shell_pipeline(
            command=command,
            workspace_root=self.workspace_root,
            sandbox_id=self.sandbox_id,
            timeout=timeout,
            stdin=stdin,
            description=description or "shell overlay",
            agent_id=agent_id,
            overlay_engine=overlay,
            overlay_sandbox=sandbox,
            occ_apply_changeset=_WriteCoordinatorChangeset(self._write_coordinator).apply_changeset,
            on_progress_line=on_progress_line,
        )
        return _simple_namespace_from_shell_result(result)

    async def _ensure_capture_runner(self) -> OverlayEngine:
        """Return the lazily initialized overlay engine.

        The method name stays for compatibility with existing tests and callers
        that patch this private boundary.
        """
        cached = self._overlay_engine
        if cached is not None:
            return cached
        async with self._init_lock:
            cached = self._overlay_engine
            if cached is not None:
                return cached
            self._overlay_engine = LocalOverlayEngine(
                sandbox_id=self.sandbox_id,
                workspace_root=self.workspace_root,
                exec_process=self._exec_sandbox_process,
                transport=self._transport,
                daemon_local=self._daemon_local,
            )
            return self._overlay_engine

    async def _exec_sandbox_process(
        self,
        sandbox: Any,
        command: str,
        *,
        timeout: int | None,
    ) -> Any:
        if sandbox is None:
            completed = await asyncio.to_thread(
                subprocess.run,
                command,
                shell=True,
                text=True,
                capture_output=True,
                timeout=timeout,
                check=False,
            )
            return SimpleNamespace(
                result=completed.stdout + completed.stderr,
                exit_code=completed.returncode,
            )
        process = getattr(sandbox, "process", None)
        exec_fn = getattr(process, "exec", None) if process is not None else None
        if exec_fn is None and callable(getattr(sandbox, "exec", None)):
            exec_fn = getattr(sandbox, "exec")
        if not callable(exec_fn):
            raise RuntimeError("Sandbox process.exec is unavailable")
        if not inspect.iscoroutinefunction(exec_fn):
            raise RuntimeError("Sandbox process.exec must be async")
        if timeout is not None:
            return await exec_fn(command, timeout=timeout)
        return await exec_fn(command)


class _WriteCoordinatorChangeset:
    def __init__(self, write_coordinator: Any) -> None:
        self._write_coordinator = write_coordinator

    async def apply_changeset(self, *args: Any, **kwargs: Any) -> Any:
        with use_sandbox_io_loop():
            return await run_sync_in_executor(
                self._write_coordinator.apply_changeset,
                *args,
                **kwargs,
            )


def _simple_namespace_from_shell_result(result: ShellResult) -> SimpleNamespace:
    conflict = result.conflict
    conflict_reason = None
    conflict_file = None
    if conflict is not None:
        conflict_reason = conflict.message or conflict.reason
        conflict_file = conflict.conflict_file
    ns = SimpleNamespace(
        result=result.result,
        exit_code=result.exit_code,
        changed_paths=list(result.changed_paths),
        files_written=len(result.changed_paths),
        conflict_file=conflict_file,
        conflict_reason=conflict_reason,
        warnings=list(result.warnings),
        overlay_run_timings=dict(result.overlay_run_timings),
        overlay_stage_timings=dict(result.overlay_stage_timings),
    )
    return ns


__all__ = ["AuditedCommandExecutor"]
