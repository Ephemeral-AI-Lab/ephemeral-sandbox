"""In-process CodeIntelligenceService backend."""

from __future__ import annotations

import logging
import threading
from collections.abc import Sequence
from typing import Any

from sandbox.occ.engine import LocalOCCEngine
from sandbox.occ.state.edit_history_ledger import EditHistoryLedger
from sandbox.occ.types import (
    EditSpec,
    OperationChange,
    OperationResult,
    WriteSpec,
)
from sandbox.runtime.shell_command_executor import AuditedCommandExecutor

__all__ = ["InProcessBackend"]

logger = logging.getLogger(__name__)


class InProcessBackend:
    """In-process backend for local and sandboxless flows."""

    def __init__(
        self,
        sandbox_id: str,
        workspace_root: str = "/workspace",
        sandbox: Any = None,
        *,
        edit_history: Any | None = None,
        direct_runtime: bool = False,
    ) -> None:
        self.sandbox_id = sandbox_id
        self.workspace_root = workspace_root
        self._sandbox = sandbox
        self._initialized = False
        self._init_lock = threading.Lock()

        history = edit_history if edit_history is not None else EditHistoryLedger()
        self._occ = LocalOCCEngine(
            workspace_root=workspace_root,
            sandbox=sandbox,
            edit_history=history,
        )
        self.arbiter = self._occ.arbiter
        self._write_coordinator = self._occ.write_coordinator
        self._command_executor = AuditedCommandExecutor(
            sandbox_id=sandbox_id,
            workspace_root=workspace_root,
            write_coordinator=self._write_coordinator,
            rebind_sandbox=self.rebind_sandbox,
            direct_runtime=direct_runtime,
        )

    def ensure_initialized(self, wait: bool = True) -> bool:
        del wait
        with self._init_lock:
            if self._initialized:
                return True

        with self._init_lock:
            self._initialized = True
        return self.is_initialized

    @property
    def is_initialized(self) -> bool:
        with self._init_lock:
            if self._initialized:
                return True
        return False

    def warmup(self) -> None:
        if self.is_initialized:
            return
        try:
            self.ensure_initialized(wait=True)
        except Exception:
            logger.debug("warmup full init failed", exc_info=True)

    def rebind_sandbox(self, sandbox: Any) -> None:
        if sandbox is None:
            return
        self._sandbox = sandbox
        self._occ.bind_sandbox(sandbox)

    async def cmd(self, sandbox: Any, command: str, **kwargs: Any) -> Any:
        return await self._command_executor.cmd(sandbox, command, **kwargs)

    def commit_operation_against_base(
        self,
        changes: Sequence[OperationChange],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> OperationResult:
        return self._occ.commit_operation_against_base(
            changes,
            agent_id=agent_id,
            edit_type=edit_type,
            description=description,
        )

    def commit_specs_many(
        self,
        requests: Sequence[dict[str, Any]],
    ) -> list[OperationResult]:
        return self._occ.commit_specs_many(requests)

    def write_file(
        self,
        specs: Sequence[WriteSpec] | WriteSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        return self._occ.write_file(
            specs,
            agent_id=agent_id,
            description=description,
        )

    def edit_file(
        self,
        specs: Sequence[EditSpec] | EditSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        return self._occ.edit_file(
            specs,
            agent_id=agent_id,
            description=description,
        )

    def dispose(self) -> None:
        self._occ.dispose()
        logger.info("CodeIntelligenceService disposed for sandbox %s", self.sandbox_id)
