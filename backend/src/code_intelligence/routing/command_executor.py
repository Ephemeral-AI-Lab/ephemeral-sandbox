"""Audited sandbox command execution for code intelligence services.

Commands run through the overlay auditor. Each command executes inside a fresh
``unshare -Urm`` namespace with a tmpfs upperdir over the live workspace, then
routes tracked writes through OCC and direct-merges gitignored runtime files.
"""

from __future__ import annotations

import asyncio
import inspect
from typing import Any, Callable

from code_intelligence.routing.overlay_auditor import OverlayAuditor


class AuditedCommandExecutor:
    """Runs sandbox commands through the OCC-gated audit path.

    The overlay auditor is initialized lazily on first use.
    """

    def __init__(
        self,
        *,
        sandbox_id: str,
        workspace_root: str,
        write_coordinator: Any,
        rebind_sandbox: Callable[[Any], None],
    ) -> None:
        self.sandbox_id = sandbox_id
        self.workspace_root = workspace_root
        self._write_coordinator = write_coordinator
        self._rebind_sandbox = rebind_sandbox
        self._overlay_auditor: OverlayAuditor | None = None
        self._init_lock = asyncio.Lock()

    async def cmd(
        self,
        sandbox: Any,
        command: str,
        *,
        timeout: int | None = None,
        description: str = "",
        agent_id: str = "",
        team_run_id: str = "",
        agent_run_id: str = "",
        task_id: str = "",
        stdin: str | None = None,
        attribute_changes: bool = True,
        on_progress_line: Callable[[str], None] | None = None,
    ) -> Any:
        """Run one command through the fail-closed OCC audit path."""
        self._rebind_sandbox(sandbox)
        overlay = await self._ensure_overlay_auditor()
        return await overlay.execute(
            sandbox,
            command,
            timeout=timeout,
            description=description,
            agent_id=agent_id,
            team_run_id=team_run_id,
            agent_run_id=agent_run_id,
            task_id=task_id,
            stdin=stdin,
            attribute_changes=attribute_changes,
            on_progress_line=on_progress_line,
        )

    async def warmup(self, sandbox: Any) -> None:
        """Pre-upload the overlay runner script so the first ``cmd()`` burst

        does not serialize 50-way concurrency behind a cold Daytona exec.
        Safe to call repeatedly; subsequent calls are ~free.
        """
        self._rebind_sandbox(sandbox)
        overlay = await self._ensure_overlay_auditor()
        await overlay._ensure_script_uploaded(sandbox)

    async def _ensure_overlay_auditor(self) -> OverlayAuditor:
        cached = self._overlay_auditor
        if cached is not None:
            return cached
        async with self._init_lock:
            cached = self._overlay_auditor
            if cached is not None:
                return cached
            self._overlay_auditor = OverlayAuditor(
                sandbox_id=self.sandbox_id,
                workspace_root=self.workspace_root,
                exec_process=self._exec_sandbox_process,
                write_coordinator=self._write_coordinator,
            )
            return self._overlay_auditor

    async def _exec_sandbox_process(
        self,
        sandbox: Any,
        command: str,
        *,
        timeout: int | None,
    ) -> Any:
        process = getattr(sandbox, "process", None)
        exec_fn = getattr(process, "exec", None) if process is not None else None
        if not callable(exec_fn):
            raise RuntimeError("Sandbox process.exec is unavailable")
        if not inspect.iscoroutinefunction(exec_fn):
            raise RuntimeError("Sandbox process.exec must be async")
        return await exec_fn(command, timeout=timeout) if timeout is not None else await exec_fn(command)
