"""Shared commit helpers for sandbox API write operations.

Decoupled from tool execution contexts: callers resolve attribution and the CI
service themselves, then pass resolved values in.
"""

from __future__ import annotations

import asyncio
import os
from collections.abc import Callable, Sequence
from dataclasses import dataclass
from types import SimpleNamespace
from typing import TYPE_CHECKING, Any, Generic, Literal, TypeVar

from sandbox.client.async_bridge import run_sync_in_executor, use_sandbox_io_loop

if TYPE_CHECKING:
    from sandbox.occ.types import OperationResult


T = TypeVar("T")

CommitOp = Literal["write", "edit"]
_BATCH_WINDOW_SECONDS = float(os.environ.get("CI_COMMIT_BATCH_WINDOW_MS", "5")) / 1000.0
_BATCHERS: dict[tuple[int, int], "_CommitBatcher"] = {}


@dataclass(frozen=True, kw_only=True)
class FileChangeResult(Generic[T]):
    """Normalized result for file-changing tools."""

    success: bool
    changed_paths: tuple[str, ...]
    raw: T
    status: str | None = None
    conflict_reason: str | None = None


@dataclass
class _CommitBatchEntry:
    op: CommitOp
    specs: Sequence[Any]
    fallback_paths: Sequence[str]
    description: str
    future: asyncio.Future[Any]
    agent_id: str
    sandbox: Any


def _maybe_rebind(svc: Any, sandbox: Any) -> None:
    if sandbox is None:
        return
    rebind = getattr(svc, "rebind_sandbox", None)
    if callable(rebind):
        rebind(sandbox)


class _CommitBatcher:
    def __init__(self, svc: Any) -> None:
        self._svc = svc
        self._lock = asyncio.Lock()
        self._entries: list[_CommitBatchEntry] = []
        self._scheduled = False

    async def submit(
        self,
        *,
        op: CommitOp,
        specs: Sequence[Any],
        fallback_paths: Sequence[str],
        description: str,
        agent_id: str,
        sandbox: Any,
    ) -> Any:
        loop = asyncio.get_running_loop()
        future: asyncio.Future[Any] = loop.create_future()
        entry = _CommitBatchEntry(
            op=op,
            specs=tuple(specs),
            fallback_paths=tuple(fallback_paths),
            description=description,
            future=future,
            agent_id=agent_id,
            sandbox=sandbox,
        )
        async with self._lock:
            self._entries.append(entry)
            if not self._scheduled:
                self._scheduled = True
                loop.create_task(self._flush_soon())
        return await future

    async def _flush_soon(self) -> None:
        await asyncio.sleep(_BATCH_WINDOW_SECONDS)
        async with self._lock:
            entries = self._entries
            self._entries = []
            self._scheduled = False
        if not entries:
            return
        if len(entries) == 1:
            entry = entries[0]
            try:
                result = await _run_direct_commit(self._svc, entry)
            except Exception as exc:
                entry.future.set_exception(exc)
            else:
                entry.future.set_result(result)
            return

        commit_many = getattr(self._svc, "commit_specs_many", None)
        if not callable(commit_many):
            await self._flush_direct(entries)
            return

        for entry in entries:
            _maybe_rebind(self._svc, entry.sandbox)
        requests = [
            {
                "op": entry.op,
                "specs": tuple(entry.specs),
                "agent_id": entry.agent_id,
                "description": entry.description,
            }
            for entry in entries
        ]
        try:
            with use_sandbox_io_loop():
                results = await run_sync_in_executor(commit_many, requests)
        except Exception:
            await self._flush_direct(entries)
            return
        if len(results) != len(entries):
            await self._flush_direct(entries)
            return
        for entry, result in zip(entries, results, strict=True):
            entry.future.set_result(result)

    async def _flush_direct(self, entries: Sequence[_CommitBatchEntry]) -> None:
        for entry in entries:
            try:
                result = await _run_direct_commit(self._svc, entry)
            except Exception as exc:
                entry.future.set_exception(exc)
            else:
                entry.future.set_result(result)


def _dedup_sorted(raw: Any) -> tuple[str, ...]:
    """Normalize a path list from ``svc.cmd``: str, strip empties, sort, dedup."""
    if not isinstance(raw, (list, tuple, set)):
        return ()
    return tuple(sorted({str(p) for p in raw if str(p or "").strip()}))


def _operation_paths(result: Any, fallback: Sequence[str]) -> tuple[str, ...]:
    files = getattr(result, "files", None)
    if isinstance(files, (list, tuple)):
        if not files and bool(getattr(result, "success", False)):
            return ()
        paths = tuple(
            str(getattr(item, "file_path", "") or "")
            for item in files
            if str(getattr(item, "file_path", "") or "").strip()
        )
        if paths:
            return paths
    return tuple(fallback)


async def _run_direct_commit(svc: Any, entry: _CommitBatchEntry) -> Any:
    method = getattr(svc, f"{entry.op}_file")
    _maybe_rebind(svc, entry.sandbox)
    with use_sandbox_io_loop():
        return await run_sync_in_executor(
            method,
            list(entry.specs),
            agent_id=entry.agent_id,
            description=entry.description,
        )


def _batcher_for(svc: Any) -> _CommitBatcher:
    loop = asyncio.get_running_loop()
    key = (id(svc), id(loop))
    batcher = _BATCHERS.get(key)
    if batcher is None:
        batcher = _CommitBatcher(svc)
        _BATCHERS[key] = batcher
    return batcher


async def submit_commit(
    svc: Any,
    *,
    op: CommitOp,
    specs: Sequence[Any],
    fallback_paths: Sequence[str],
    description: str,
    agent_id: str,
    sandbox: Any | None = None,
) -> FileChangeResult["OperationResult"]:
    """Submit one write/edit/delete/move commit through *svc*.

    *svc* must be an active CodeIntelligenceService. *agent_id* is the ledger
    attribution label. *sandbox*, when not ``None``, is rebound onto the service
    before the sync OCC dispatch so reads see the current handle.
    """
    if svc is None:
        raise RuntimeError(
            "submit_commit requires an active ci_service; "
            "caller must require SandboxApi before submitting commits",
        )

    result = await _batcher_for(svc).submit(
        op=op,
        specs=specs,
        fallback_paths=fallback_paths,
        description=description,
        agent_id=agent_id,
        sandbox=sandbox,
    )

    paths = _operation_paths(result, fallback_paths)
    conflict = str(getattr(result, "conflict_reason", "") or "")
    status = str(getattr(result, "status", "") or "")
    return FileChangeResult(
        success=bool(getattr(result, "success", False)),
        changed_paths=paths,
        status=status or None,
        conflict_reason=conflict or None,
        raw=result,
    )


async def submit_shell_cmd(
    svc: Any,
    sandbox: Any,
    *,
    command: str,
    description: str,
    timeout: int | None = None,
    attribute_changes: bool = True,
    on_progress_line: Callable[[str], None] | None = None,
    agent_id: str,
    run_id: str = "",
    agent_run_id: str = "",
    task_id: str = "",
) -> FileChangeResult[SimpleNamespace]:
    """Run a shell command through *svc* against *sandbox*."""
    if svc is None:
        raise RuntimeError(
            "submit_shell_cmd requires an active ci_service; "
            "caller must short-circuit before entering the façade",
        )
    if sandbox is None:
        raise RuntimeError(
            "submit_shell_cmd requires a sandbox handle "
            "(ci_sandbox or daytona_sandbox in context, or an explicit override)",
        )

    cmd_kwargs: dict[str, Any] = {
        "timeout": timeout,
        "description": description,
        "agent_id": agent_id,
        "run_id": run_id,
        "agent_run_id": agent_run_id,
        "task_id": task_id,
        "attribute_changes": attribute_changes,
    }
    if on_progress_line is not None:
        cmd_kwargs["on_progress_line"] = on_progress_line
    response = await svc.cmd(sandbox, command, **cmd_kwargs)

    changed = _dedup_sorted(getattr(response, "changed_paths", None))
    exit_code = int(getattr(response, "exit_code", 1) or 0)
    conflict_reason = getattr(response, "conflict_reason", None)
    if not conflict_reason:
        conflict = getattr(response, "conflict", None)
        if conflict is not None:
            conflict_reason = (
                getattr(conflict, "message", None)
                or getattr(conflict, "reason", None)
            )

    success = exit_code == 0 and not conflict_reason
    return FileChangeResult(
        success=success,
        changed_paths=changed,
        status="ok" if success else "error",
        conflict_reason=(str(conflict_reason) if conflict_reason else None),
        raw=response,
    )


def commit_metadata(change: Any, paths: list[str] | None = None) -> dict[str, Any]:
    """Return common metadata for file commit results."""
    changed_paths = list(change.changed_paths if paths is None else paths)
    return {
        "changed_paths": changed_paths,
        "conflict_reason": change.conflict_reason,
    }


__all__ = [
    "CommitOp",
    "FileChangeResult",
    "commit_metadata",
    "submit_commit",
    "submit_shell_cmd",
]
