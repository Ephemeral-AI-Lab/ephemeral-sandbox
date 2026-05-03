"""Backend protocol for the CodeIntelligenceService facade."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any, Protocol

from sandbox.occ.types import (
    EditRequest,
    EditResult,
    EditSpec,
    OperationChange,
    OperationResult,
    WriteSpec,
)


class CodeIntelligenceBackend(Protocol):
    """Shape that every code-intelligence backend implements."""

    sandbox_id: str
    workspace_root: str
    is_initialized: bool

    def ensure_initialized(self, wait: bool = True) -> bool: ...
    def warmup(self) -> None: ...
    def rebind_sandbox(self, sandbox: Any) -> None: ...
    async def cmd(self, sandbox: Any, command: str, **kwargs: Any) -> Any: ...
    def apply(self, request: EditRequest) -> EditResult: ...
    def commit_operation_against_base(
        self,
        changes: Sequence[OperationChange],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> OperationResult: ...
    def commit_specs_many(
        self,
        requests: Sequence[dict[str, Any]],
    ) -> list[OperationResult]: ...
    def write_file(
        self,
        specs: Sequence[WriteSpec] | WriteSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult: ...
    def edit_file(
        self,
        specs: Sequence[EditSpec] | EditSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult: ...
    def dispose(self) -> None: ...


__all__ = ["CodeIntelligenceBackend"]
