"""Shared commit helpers for Daytona write tools."""

from __future__ import annotations

import asyncio
import os
from collections.abc import Callable
from dataclasses import dataclass
from types import SimpleNamespace
from typing import TYPE_CHECKING, Any, Generic, Literal, Sequence, TypeVar

from code_intelligence._async_bridge import run_sync_in_executor, use_sandbox_io_loop
from tools.core.ci_attribution import (
    agent_attribution_from_context,
    rebind_ci_service,
    resolved_agent_id,
)

if TYPE_CHECKING:
    from code_intelligence.types import OperationResult
    from tools.core.base import ToolExecutionContext


T = TypeVar("T")

CommitOp = Literal["write", "edit", "delete", "move"]
_BATCH_WINDOW_SECONDS = float(os.environ.get("CI_COMMIT_BATCH_WINDOW_MS", "5")) / 1000.0
_BATCHERS: dict[tuple[int, int], "_CommitBatcher"] = {}


@dataclass(frozen=True, kw_only=True)
class FileChangeResult(Generic[T]):
    """Normalized result for Daytona file-changing tools."""

    success: bool
    changed_paths: tuple[str, ...]
    raw: T
    ambient_changed_paths: tuple[str, ...] = ()
    conflict_reason: str | None = None


@dataclass
class _CommitBatchEntry:
    context: "ToolExecutionContext"
    op: CommitOp
    specs: Sequence[Any]
    fallback_paths: Sequence[str]
    description: str
    future: asyncio.Future[Any]


class _CommitBatcher:
    def __init__(self, svc: Any) -> None:
        self._svc = svc
        self._lock = asyncio.Lock()
        self._entries: list[_CommitBatchEntry] = []
        self._scheduled = False

    async def submit(
        self,
        context: "ToolExecutionContext",
        *,
        op: CommitOp,
        specs: Sequence[Any],
        fallback_paths: Sequence[str],
        description: str,
    ) -> Any:
        loop = asyncio.get_running_loop()
        future: asyncio.Future[Any] = loop.create_future()
        entry = _CommitBatchEntry(
            context=context,
            op=op,
            specs=tuple(specs),
            fallback_paths=tuple(fallback_paths),
            description=description,
            future=future,
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
                result = await _run_direct_commit(
                    entry.context,
                    self._svc,
                    op=entry.op,
                    specs=entry.specs,
                    description=entry.description,
                )
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
            rebind_ci_service(entry.context, self._svc)
        requests = [
            {
                "op": entry.op,
                "specs": tuple(entry.specs),
                "agent_id": resolved_agent_id(entry.context),
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
                result = await _run_direct_commit(
                    entry.context,
                    self._svc,
                    op=entry.op,
                    specs=entry.specs,
                    description=entry.description,
                )
            except Exception as exc:
                entry.future.set_exception(exc)
            else:
                entry.future.set_result(result)


def _dedup_sorted(raw: Any) -> tuple[str, ...]:
    """Normalize a path list from ``svc.cmd``: str, strip empties, sort, dedup."""
    if not isinstance(raw, list):
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


async def _run_direct_commit(
    context: "ToolExecutionContext",
    svc: Any,
    *,
    op: CommitOp,
    specs: Sequence[Any],
    description: str,
) -> Any:
    method = getattr(svc, f"{op}_file")
    rebind_ci_service(context, svc)
    with use_sandbox_io_loop():
        return await run_sync_in_executor(
            method,
            list(specs),
            agent_id=resolved_agent_id(context),
            description=description,
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
    context: "ToolExecutionContext",
    *,
    op: CommitOp,
    specs: Sequence[Any],
    fallback_paths: Sequence[str],
    description: str,
) -> FileChangeResult["OperationResult"]:
    """Submit one write/edit/delete/move commit through the CI service."""
    from tools.core.ci_runtime import get_ci_service

    svc = get_ci_service(context)
    if svc is None:
        raise RuntimeError(
            "submit_commit requires an active ci_service; "
            "caller must short-circuit with ci_write_required_result first",
        )

    result = await _batcher_for(svc).submit(
        context,
        op=op,
        specs=specs,
        fallback_paths=fallback_paths,
        description=description,
    )

    paths = _operation_paths(result, fallback_paths)
    conflict = str(getattr(result, "conflict_reason", "") or "")
    return FileChangeResult(
        success=bool(getattr(result, "success", False)),
        changed_paths=paths,
        conflict_reason=conflict or None,
        raw=result,
    )


async def submit_shell_cmd(
    context: "ToolExecutionContext",
    *,
    command: str,
    description: str,
    timeout: int | None = None,
    sandbox: Any | None = None,
    attribute_changes: bool = True,
    on_progress_line: Callable[[str], None] | None = None,
) -> FileChangeResult[SimpleNamespace]:
    """Run a daytona_shell command through the CI service."""
    from tools.core.ci_runtime import get_ci_service

    svc = get_ci_service(context)
    if svc is None:
        raise RuntimeError(
            "submit_shell_cmd requires an active ci_service; "
            "caller must short-circuit before entering the façade",
        )
    resolved_sandbox = sandbox
    if resolved_sandbox is None:
        resolved_sandbox = context.metadata.get("ci_sandbox") or context.metadata.get(
            "daytona_sandbox",
        )
    if resolved_sandbox is None:
        raise RuntimeError(
            "submit_shell_cmd requires a sandbox in context.metadata "
            "(ci_sandbox or daytona_sandbox) or as an explicit argument",
        )

    attribution = agent_attribution_from_context(context)
    cmd_kwargs: dict[str, Any] = {
        "timeout": timeout,
        "description": description,
        "agent_id": attribution.agent_id,
        "team_run_id": attribution.team_run_id,
        "agent_run_id": attribution.agent_run_id,
        "task_id": attribution.task_id,
        "attribute_changes": attribute_changes,
    }
    if on_progress_line is not None:
        cmd_kwargs["on_progress_line"] = on_progress_line
    response = await svc.cmd(resolved_sandbox, command, **cmd_kwargs)

    changed = _dedup_sorted(getattr(response, "changed_paths", None))
    ambient = _dedup_sorted(getattr(response, "ambient_changed_paths", None))
    exit_code = int(getattr(response, "exit_code", 1) or 0)
    commit_status = getattr(response, "git_commit_status", None)
    conflict_reason = getattr(response, "git_conflict_reason", None)

    success = exit_code == 0 and (commit_status in (None, "committed", "noop"))
    return FileChangeResult(
        success=success,
        changed_paths=changed,
        ambient_changed_paths=ambient,
        conflict_reason=(str(conflict_reason) if conflict_reason else None),
        raw=response,
    )


__all__ = [
    "CommitOp",
    "FileChangeResult",
    "submit_shell_cmd",
    "submit_commit",
]
