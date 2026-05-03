"""Canonical audit-aware :class:`SandboxApi` implementation.

Composed from a :class:`SandboxTransport` (raw I/O), a
``CodeIntelligenceService``-shaped audit engine, and a live sandbox
handle (used by the audited shell path that needs the provider object,
not just an id).

Per-context binding: Step 7 wires one ``AuditedSandboxApi`` per
``ToolExecutionContext``, so each instance carries the svc and sandbox
its tools should target. The ``sandbox_id`` parameter on every method
is structural — it satisfies the :class:`SandboxApi` Protocol so a
future registry can dispatch across multiple sandboxes — but Step 4
always uses the bound resources.

This module imports only from ``sandbox.api.*``. Engine spec types stay
contained inside :mod:`sandbox.api.audit` (the engine bridge); reading
and search go through :mod:`sandbox.api.transport`.
"""

from __future__ import annotations

from typing import Any, ClassVar

from sandbox.api import audit
from sandbox.api.models import (
    EditFileRequest,
    EditFileResult,
    ReadFileRequest,
    ReadFileResult,
    ShellRequest,
    ShellResult,
    WriteFileRequest,
    WriteFileResult,
)
from sandbox.api.transport import SandboxTransport


class AuditedSandboxApi:
    """Audit-aware :class:`SandboxApi` implementation."""

    name: ClassVar[str] = "audited"

    def __init__(
        self,
        *,
        transport: SandboxTransport,
        svc: Any,
        sandbox: Any,
    ) -> None:
        self._transport = transport
        self._svc = svc
        self._sandbox = sandbox

    # -- read / search ------------------------------------------------

    async def read_file(
        self,
        sandbox_id: str,
        request: ReadFileRequest,
    ) -> ReadFileResult:
        try:
            payload = await self._transport.read_bytes(sandbox_id, request.path)
        except FileNotFoundError:
            return ReadFileResult(content="", exists=False)
        return ReadFileResult(content=payload.decode("utf-8"), exists=True)

    # -- mutation -----------------------------------------------------

    async def write_file(
        self,
        sandbox_id: str,
        request: WriteFileRequest,
    ) -> WriteFileResult:
        del sandbox_id
        change = await audit.submit_write_request(
            self._svc, request=request, sandbox=self._sandbox,
        )
        return WriteFileResult(
            success=change.success,
            changed_paths=tuple(change.changed_paths),
            conflict_reason=change.conflict_reason,
        )

    async def edit_file(
        self,
        sandbox_id: str,
        request: EditFileRequest,
    ) -> EditFileResult:
        del sandbox_id
        change = await audit.submit_edit_request(
            self._svc, request=request, sandbox=self._sandbox,
        )
        return EditFileResult(
            success=change.success,
            changed_paths=tuple(change.changed_paths),
            applied_edits=len(request.edits) if change.success else 0,
            conflict_reason=change.conflict_reason,
        )

    # -- shell --------------------------------------------------------

    async def shell(
        self,
        sandbox_id: str,
        request: ShellRequest,
    ) -> ShellResult:
        del sandbox_id
        change = await audit.submit_shell_request(
            self._svc, sandbox=self._sandbox, request=request,
        )
        raw = change.raw
        return ShellResult(
            exit_code=int(getattr(raw, "exit_code", 1) or 0),
            stdout=str(getattr(raw, "result", "") or ""),
            stderr="",  # Overlay capture merges stderr into result
            success=change.success,
            changed_paths=tuple(change.changed_paths),
            conflict_reason=change.conflict_reason,
            warnings=tuple(getattr(raw, "warnings", []) or ()),
        )


__all__ = ["AuditedSandboxApi"]
