"""OCC engine boundary for runtime handlers and pipelines."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from sandbox.occ.changeset.types import ChangesetResult, UpperChangeLike
from sandbox.occ.commit import WriteCoordinator
from sandbox.occ.content.manager import ContentManager
from sandbox.occ.operations.service import OCCOperationService
from sandbox.occ.patching.patcher import Patcher
from sandbox.occ.state.arbiter import Arbiter
from sandbox.occ.state.ledger_store import LedgerStore, state_dir
from sandbox.occ.types import (
    EditSpec,
    OperationChange,
    OperationResult,
    WriteSpec,
)


class LocalOCCEngine:
    """Concrete in-sandbox OCC engine assembled from OCC-owned components."""

    def __init__(
        self,
        *,
        workspace_root: str,
        sandbox: Any = None,
        edit_history: Any | None = None,
    ) -> None:
        self.workspace_root = workspace_root
        self._ledger = None if edit_history is not None else LedgerStore(
            state_dir(workspace_root)
        )
        self._content = ContentManager(workspace_root, sandbox=sandbox)
        self._arbiter = Arbiter(
            workspace_root=workspace_root,
            edit_history=edit_history if edit_history is not None else self._ledger,
        )
        self._patcher = Patcher()
        self._write_coordinator = WriteCoordinator(
            arbiter=self._arbiter,
            content=self._content,
        )
        self._operations = OCCOperationService(
            content=self._content,
            write_coordinator=self._write_coordinator,
            patcher=self._patcher,
        )

    @property
    def arbiter(self) -> Arbiter:
        return self._arbiter

    @property
    def write_coordinator(self) -> WriteCoordinator:
        return self._write_coordinator

    def bind_sandbox(self, sandbox: Any) -> None:
        self._content.bind_sandbox(sandbox)

    def commit_operation_against_base(
        self,
        changes: Sequence[OperationChange],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> OperationResult:
        return self._write_coordinator.commit_operation_against_base(
            changes,
            agent_id=agent_id,
            edit_type=edit_type,
            description=description,
        )

    def commit_specs_many(
        self,
        requests: Sequence[dict[str, Any]],
    ) -> list[OperationResult]:
        return self._operations.commit_specs_many(requests)

    def write_file(
        self,
        specs: Sequence[WriteSpec] | WriteSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        return self._operations.write_file(
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
        return self._operations.edit_file(
            specs,
            agent_id=agent_id,
            description=description,
        )

    def apply_changeset(
        self,
        upper_changes: Sequence[UpperChangeLike],
        *,
        agent_id: str = "",
        edit_type: str,
        description: str = "",
    ) -> ChangesetResult:
        return self._write_coordinator.apply_changeset(
            upper_changes,
            agent_id=agent_id,
            edit_type=edit_type,
            description=description,
        )

    def dispose(self) -> None:
        self._arbiter.cleanup_locks()
        if self._ledger is not None:
            self._ledger.close()


__all__ = ["LocalOCCEngine"]
