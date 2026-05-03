"""Per-sandbox runtime service facade.

The facade delegates every public op to a backend selected at construction
time. Sandboxes with a registered provider adapter use :class:`DaemonBackend`;
sandboxless/local flows keep using :class:`InProcessBackend`.
"""

from __future__ import annotations

import logging
from collections.abc import Sequence
from typing import Any

from sandbox.providers.registry import get_adapter
from sandbox.runtime.backends import (
    CodeIntelligenceBackend,
    InProcessBackend,
    DaemonBackend,
)
from sandbox.occ.types import (
    EditSpec,
    OperationChange,
    OperationResult,
    WriteSpec,
)

__all__ = ["CodeIntelligenceService"]

logger = logging.getLogger(__name__)


def _select_backend(
    sandbox_id: str,
    workspace_root: str,
    sandbox: Any,
    *,
    edit_history: Any | None = None,
    direct_runtime: bool = False,
) -> CodeIntelligenceBackend:
    """Pick a backend based on provider-adapter availability.

    Provider-backed remote sandboxes use the daemon backend. Local
    sandboxless flows (no adapter / empty sandbox_id) keep using
    :class:`InProcessBackend`.

    ``edit_history`` is only meaningful for the in-process backend. The daemon
    owns the canonical SQLite ledger when the daemon backend is in use.
    """
    if _has_provider_adapter(sandbox_id):
        return DaemonBackend(
            sandbox_id=sandbox_id,
            workspace_root=workspace_root,
        )
    return InProcessBackend(
        sandbox_id=sandbox_id,
        workspace_root=workspace_root,
        sandbox=sandbox,
        edit_history=edit_history,
        direct_runtime=direct_runtime,
    )


def _has_provider_adapter(sandbox_id: str) -> bool:
    if not sandbox_id:
        return False
    try:
        get_adapter(sandbox_id)
    except KeyError:
        return False
    return True


class CodeIntelligenceService:
    """Thin facade that forwards every public op to the selected backend."""

    def __init__(
        self,
        sandbox_id: str,
        workspace_root: str = "/workspace",
        sandbox: Any = None,
        *,
        edit_history: Any | None = None,
        direct_runtime: bool = False,
    ) -> None:
        self._impl: CodeIntelligenceBackend = _select_backend(
            sandbox_id,
            workspace_root,
            sandbox,
            edit_history=edit_history,
            direct_runtime=direct_runtime,
        )

    # -- Identity / state forwarding -----------------------------------------

    @property
    def sandbox_id(self) -> str:
        return self._impl.sandbox_id

    @property
    def workspace_root(self) -> str:
        return self._impl.workspace_root

    @property
    def is_initialized(self) -> bool:
        return self._impl.is_initialized

    # -- Internal-component pass-through (load-bearing for mutation callers) -

    @property
    def arbiter(self) -> Any:
        return self._impl.arbiter  # type: ignore[attr-defined]

    @property
    def _write_coordinator(self) -> Any:
        return self._impl._write_coordinator  # type: ignore[attr-defined]

    @property
    def _command_executor(self) -> Any:
        return self._impl._command_executor  # type: ignore[attr-defined]

    # -- Public API forwarding -----------------------------------------------

    def ensure_initialized(self, wait: bool = True) -> bool:
        return self._impl.ensure_initialized(wait=wait)

    def warmup(self) -> None:
        self._impl.warmup()

    def rebind_sandbox(self, sandbox: Any) -> None:
        self._impl.rebind_sandbox(sandbox)

    async def cmd(self, sandbox: Any, command: str, **kwargs: Any) -> Any:
        return await self._impl.cmd(sandbox, command, **kwargs)

    def commit_operation_against_base(
        self,
        changes: Sequence[OperationChange],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> OperationResult:
        return self._impl.commit_operation_against_base(
            changes,
            agent_id=agent_id,
            edit_type=edit_type,
            description=description,
        )

    def commit_specs_many(
        self,
        requests: Sequence[dict[str, Any]],
    ) -> list[OperationResult]:
        return self._impl.commit_specs_many(requests)

    def write_file(
        self,
        specs: Sequence[WriteSpec] | WriteSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        return self._impl.write_file(specs, agent_id=agent_id, description=description)

    def edit_file(
        self,
        specs: Sequence[EditSpec] | EditSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        return self._impl.edit_file(specs, agent_id=agent_id, description=description)

    def dispose(self) -> None:
        self._impl.dispose()
