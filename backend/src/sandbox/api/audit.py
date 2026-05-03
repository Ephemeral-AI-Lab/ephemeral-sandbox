"""Attribution-aware façade over the lifecycle commit engine.

The OCC machinery (``WriteCoordinator``, ``OCCOperationService``,
overlay capture, etc.) lives in :mod:`sandbox.runtime` and
is reached today through a ``CodeIntelligenceService`` handle. This
module provides the thin attribution-aware façade that the
``SandboxApi`` impl will compose: ``submit_commit`` and
``submit_shell_cmd`` accept a single :class:`RequestActor` instead of
the four loose strings the engine entry points consume.

Phase 1 boundary: this module forwards to
:mod:`sandbox.lifecycle.commit`. The engine consumes ``SandboxTransport``
directly; this façade talks to ``svc`` rather than to a transport.

Provider neutrality: this module must not import from
``sandbox.daytona``, ``sandbox.runtime``, or ``tools.*``.
"""

from __future__ import annotations

from collections.abc import Callable, Sequence
from types import SimpleNamespace
from typing import Any

from sandbox.api.models import (
    EditFileRequest,
    RequestActor,
    ShellRequest,
    WriteFileRequest,
)
from sandbox.lifecycle.commit import (
    CommitOp,
    FileChangeResult,
    commit_metadata,
    submit_commit as _engine_submit_commit,
    submit_shell_cmd as _engine_submit_shell_cmd,
)

__all__ = [
    "CommitOp",
    "FileChangeResult",
    "commit_metadata",
    "submit_commit",
    "submit_edit_request",
    "submit_shell_cmd",
    "submit_shell_request",
    "submit_write_request",
]


async def submit_commit(
    svc: Any,
    *,
    op: CommitOp,
    specs: Sequence[Any],
    fallback_paths: Sequence[str],
    description: str,
    actor: RequestActor,
    sandbox: Any | None = None,
) -> FileChangeResult:
    """Submit a write/edit/delete/move commit through the OCC pipeline.

    Provider-neutral: ``svc`` is a ``CodeIntelligenceService``-shaped object
    that owns the OCC machinery; ``sandbox`` is the live provider handle
    rebound onto the service for read-side fidelity. Attribution is carried
    as a single ``RequestActor`` rather than four loose strings.
    """
    return await _engine_submit_commit(
        svc,
        op=op,
        specs=specs,
        fallback_paths=fallback_paths,
        description=description,
        agent_id=actor.agent_id,
        sandbox=sandbox,
    )


async def submit_shell_cmd(
    svc: Any,
    sandbox: Any,
    *,
    command: str,
    description: str,
    actor: RequestActor,
    timeout: int | None = None,
    attribute_changes: bool = True,
    on_progress_line: Callable[[str], None] | None = None,
) -> FileChangeResult[SimpleNamespace]:
    """Run a shell command through the audited execution path."""
    return await _engine_submit_shell_cmd(
        svc,
        sandbox,
        command=command,
        description=description,
        timeout=timeout,
        attribute_changes=attribute_changes,
        on_progress_line=on_progress_line,
        agent_id=actor.agent_id,
        run_id=actor.run_id,
        agent_run_id=actor.agent_run_id,
        task_id=actor.task_id,
    )


# -- Request-shaped helpers (the AuditedSandboxApi entry surface) -----------
#
# Each helper translates one ``sandbox.api.models`` request into the engine
# spec types and forwards through ``submit_commit`` / ``submit_shell_cmd``.
# Audit is the engine bridge, so importing the engine spec types here is
# in-scope; AuditedSandboxApi itself stays free of engine imports.


async def submit_write_request(
    svc: Any,
    *,
    request: WriteFileRequest,
    sandbox: Any | None = None,
) -> FileChangeResult:
    """Forward a :class:`WriteFileRequest` through the OCC pipeline."""
    from sandbox.occ.types import WriteSpec

    spec = WriteSpec(
        file_path=request.path,
        content=request.content,
        overwrite=request.overwrite,
    )
    return await submit_commit(
        svc,
        op="write",
        specs=[spec],
        fallback_paths=[request.path],
        description=request.description or f"write {request.path}",
        actor=request.actor,
        sandbox=sandbox,
    )


async def submit_edit_request(
    svc: Any,
    *,
    request: EditFileRequest,
    sandbox: Any | None = None,
) -> FileChangeResult:
    """Forward an :class:`EditFileRequest` through the OCC pipeline."""
    from sandbox.occ.types import EditSpec
    from sandbox.occ.patching.patcher import (
        SearchReplaceEdit as _EngineSREdit,
    )

    engine_edits = [
        _EngineSREdit(old_text=edit.old_text, new_text=edit.new_text)
        for edit in request.edits
    ]
    spec = EditSpec(file_path=request.path, edits=engine_edits)
    return await submit_commit(
        svc,
        op="edit",
        specs=[spec],
        fallback_paths=[request.path],
        description=request.description or f"edit {request.path}",
        actor=request.actor,
        sandbox=sandbox,
    )


async def submit_shell_request(
    svc: Any,
    *,
    sandbox: Any,
    request: ShellRequest,
    on_progress_line: Callable[[str], None] | None = None,
) -> FileChangeResult[SimpleNamespace]:
    """Forward a :class:`ShellRequest` through the audited execution path."""
    return await submit_shell_cmd(
        svc,
        sandbox,
        command=request.command,
        description=request.description or "shell",
        actor=request.actor,
        timeout=request.timeout,
        attribute_changes=request.attribute_changes,
        on_progress_line=on_progress_line,
    )
