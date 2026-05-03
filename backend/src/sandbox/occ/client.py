"""Host-side client for OCC runtime server operations."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any

from sandbox.occ.changeset.types import Change, ChangesetResult
from sandbox.occ.types import (
    EditSpec,
    OperationResult,
    WriteSpec,
)
from sandbox.occ.wire import (
    change_to_dict,
    changeset_result_from_dict,
    editspec_to_dict,
    normalize_edit_specs,
    normalize_write_specs,
    operation_result_from_dict,
    writespec_to_dict,
)
from sandbox.providers.registry import get_adapter
from sandbox.runtime._server_dispatch import RuntimeDispatchError, call_runtime_server


class OCCClientError(RuntimeError):
    """Raised when the OCC runtime server returns a transport/error envelope."""

    def __init__(
        self,
        kind: str,
        message: str,
        details: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(f"{kind}: {message}")
        self.kind = kind
        self.message = message
        self.details = details or {}


class OCCClient:
    """Typed host route for OCC requests dispatched through runtime/server.py."""

    def __init__(
        self,
        sandbox_id: str,
        *,
        workspace_root: str = "/workspace",
        timeout: int = 300,
    ) -> None:
        self.sandbox_id = sandbox_id
        self.workspace_root = workspace_root
        self.timeout = timeout

    async def write(
        self,
        specs: Sequence[WriteSpec] | WriteSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        result = await self._call(
            "occ.write",
            {
                "specs": [writespec_to_dict(s) for s in normalize_write_specs(specs)],
                "agent_id": agent_id,
                "description": description,
            },
        )
        return operation_result_from_dict(result)

    async def edit(
        self,
        specs: Sequence[EditSpec] | EditSpec,
        *,
        agent_id: str = "",
        description: str = "",
    ) -> OperationResult:
        result = await self._call(
            "occ.edit",
            {
                "specs": [editspec_to_dict(s) for s in normalize_edit_specs(specs)],
                "agent_id": agent_id,
                "description": description,
            },
        )
        return operation_result_from_dict(result)

    async def apply_changeset(
        self,
        changes: Sequence[Change],
        *,
        agent_id: str = "",
        description: str = "",
    ) -> ChangesetResult:
        """Apply a typed :class:`Change` batch through the new OCC gate."""
        result = await self._call(
            "occ.apply_changeset",
            {
                "changes": [change_to_dict(c) for c in changes],
                "agent_id": agent_id,
                "description": description,
            },
        )
        return changeset_result_from_dict(result)

    async def _call(self, op: str, args: dict[str, Any]) -> dict[str, Any]:
        try:
            return await call_runtime_server(
                exec_fn=get_adapter(self.sandbox_id).exec,
                sandbox_id=self.sandbox_id,
                op=op,
                args={"workspace_root": self.workspace_root, **args},
                timeout=self.timeout,
            )
        except RuntimeDispatchError as exc:
            raise OCCClientError(exc.kind, exc.message, exc.details) from exc


__all__ = ["OCCClient", "OCCClientError"]
