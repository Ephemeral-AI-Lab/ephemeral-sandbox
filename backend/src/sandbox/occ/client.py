"""Host-side client for OCC changeset operations."""

from __future__ import annotations

from collections.abc import Sequence
from typing import Any, Protocol

from sandbox.occ.changeset.intent import CommitIntent, PreparedChangeset
from sandbox.occ.changeset.types import Change, ChangesetResult
from sandbox.occ.service import OccService


class OccApplyService(Protocol):
    async def apply_changeset(
        self,
        changes: Sequence[Change],
        *,
        snapshot: Any = None,
        options: CommitIntent | None = None,
    ) -> ChangesetResult | PreparedChangeset: ...


_SERVICES: dict[str, OccApplyService] = {}


def register_occ_service(sandbox_id: str, service: OccApplyService) -> None:
    """Bind a sandbox id to the typed OCC service path."""
    key = str(sandbox_id).strip()
    if not key:
        raise ValueError("sandbox_id must not be empty")
    _SERVICES[key] = service


def dispose_occ_service(sandbox_id: str) -> None:
    """Remove a typed OCC service binding."""
    _SERVICES.pop(str(sandbox_id), None)


def get_occ_service(sandbox_id: str) -> OccApplyService:
    """Return the typed OCC service bound to *sandbox_id*."""
    try:
        return _SERVICES[str(sandbox_id)]
    except KeyError as exc:
        raise OCCClientError(
            "MissingOccService",
            f"no typed OCC service is registered for sandbox {sandbox_id!r}",
        ) from exc


class OCCClientError(RuntimeError):
    """Raised when the typed OCC client is not bound to a service."""

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
    """Public OCC changeset client.

    Callers either bind a service directly or resolve one by sandbox id through
    the local service registry. The old runtime OCC wire dispatch path has
    been removed from this client.
    """

    def __init__(
        self,
        sandbox_id: str | None = None,
        *,
        service: OccApplyService | OccService | None = None,
    ) -> None:
        if service is None and sandbox_id is not None:
            service = get_occ_service(sandbox_id)
        if service is None:
            raise OCCClientError(
                "MissingOccService",
                "OCCClient requires a typed OccService or registered sandbox binding",
            )
        self._service = service

    async def apply_changeset(
        self,
        changes: Sequence[Change],
        *,
        agent_id: str = "",
        description: str = "",
        snapshot=None,
        options: CommitIntent | None = None,
    ) -> ChangesetResult | PreparedChangeset:
        """Apply or prepare a typed :class:`Change` batch through OCC."""
        intent = options or CommitIntent(
            caller_id=agent_id,
            description=description,
        )
        return await self._service.apply_changeset(
            changes,
            snapshot=snapshot,
            options=intent,
        )


__all__ = [
    "OCCClient",
    "OCCClientError",
    "OccApplyService",
    "dispose_occ_service",
    "get_occ_service",
    "register_occ_service",
]
