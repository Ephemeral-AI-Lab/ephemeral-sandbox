"""Rename Python symbols across files in Daytona."""

from __future__ import annotations

import asyncio
import json
import keyword
import logging
import os
import re
from dataclasses import dataclass
from typing import Any

from pydantic import BaseModel, Field

from code_intelligence._async_bridge import run_sync_in_executor, use_sandbox_io_loop
from code_intelligence.types import SymbolKind
from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_attribution import rebind_ci_service, resolved_agent_id
from tools.core.ci_runtime import ci_required_result, get_ci_service
from tools.core.decorator import tool
from tools.core.op_result_to_tool_result import operation_result_to_tool_result
from tools.core.sandbox_runtime import resolve_daytona_path

logger = logging.getLogger(__name__)

_IDENTIFIER_RE = r"^[A-Za-z_][A-Za-z0-9_]*$"
_CANDIDATE_LIMIT = 10
_RENAME_BATCH_WINDOW_SECONDS = (
    float(os.environ.get("CI_RENAME_BATCH_WINDOW_MS", "5")) / 1000.0
)
_RENAME_PREPLAN_CACHE_KEY = "_daytona_rename_preplan"


@dataclass
class _RenamePlanEntry:
    svc: Any
    file_path: str
    line: int
    character: int
    new_name: str
    future: asyncio.Future[Any]


@dataclass
class _RenameCommitEntry:
    svc: Any
    context: ToolExecutionContext
    plan: Any
    description: str
    future: asyncio.Future[Any]


class _RenamePlanBatcher:
    def __init__(self, svc: Any) -> None:
        self._svc = svc
        self._lock = asyncio.Lock()
        self._entries: list[_RenamePlanEntry] = []
        self._scheduled = False

    async def submit(
        self,
        *,
        file_path: str,
        line: int,
        character: int,
        new_name: str,
    ) -> Any:
        loop = asyncio.get_running_loop()
        future: asyncio.Future[Any] = loop.create_future()
        entry = _RenamePlanEntry(
            svc=self._svc,
            file_path=file_path,
            line=line,
            character=character,
            new_name=new_name,
            future=future,
        )
        async with self._lock:
            self._entries.append(entry)
            if not self._scheduled:
                self._scheduled = True
                loop.create_task(self._flush_soon())
        return await future

    async def _flush_soon(self) -> None:
        await asyncio.sleep(_RENAME_BATCH_WINDOW_SECONDS)
        async with self._lock:
            entries = self._entries
            self._entries = []
            self._scheduled = False
        if not entries:
            return
        method = getattr(self._svc, "rename_symbol_plans_many", None)
        if len(entries) == 1 or not callable(method):
            await self._flush_direct(entries)
            return
        requests = [
            {
                "file_path": entry.file_path,
                "line": entry.line,
                "character": entry.character,
                "new_name": entry.new_name,
            }
            for entry in entries
        ]
        try:
            with use_sandbox_io_loop():
                results = await run_sync_in_executor(method, requests)
        except Exception:
            await self._flush_direct(entries)
            return
        if len(results) != len(entries):
            await self._flush_direct(entries)
            return
        for entry, result in zip(entries, results, strict=True):
            entry.future.set_result(result)

    async def _flush_direct(self, entries: list[_RenamePlanEntry]) -> None:
        for entry in entries:
            try:
                with use_sandbox_io_loop():
                    result = await run_sync_in_executor(
                        self._svc.rename_symbol_plan,
                        entry.file_path,
                        entry.line,
                        entry.character,
                        entry.new_name,
                    )
            except Exception as exc:
                entry.future.set_exception(exc)
            else:
                entry.future.set_result(result)


class _RenameCommitBatcher:
    def __init__(self, svc: Any) -> None:
        self._svc = svc
        self._lock = asyncio.Lock()
        self._entries: list[_RenameCommitEntry] = []
        self._scheduled = False

    async def submit(
        self,
        context: ToolExecutionContext,
        *,
        plan: Any,
        description: str,
    ) -> Any:
        loop = asyncio.get_running_loop()
        future: asyncio.Future[Any] = loop.create_future()
        entry = _RenameCommitEntry(
            svc=self._svc,
            context=context,
            plan=plan,
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
        await asyncio.sleep(_RENAME_BATCH_WINDOW_SECONDS)
        async with self._lock:
            entries = self._entries
            self._entries = []
            self._scheduled = False
        if not entries:
            return
        method = getattr(self._svc, "commit_rename_plans_many", None)
        if len(entries) == 1 or not callable(method):
            await self._flush_direct(entries)
            return
        for entry in entries:
            rebind_ci_service(entry.context, self._svc)
        requests = [
            {
                "plan": entry.plan,
                "agent_id": resolved_agent_id(entry.context),
                "description": entry.description,
            }
            for entry in entries
        ]
        try:
            with use_sandbox_io_loop():
                results = await run_sync_in_executor(method, requests)
        except Exception:
            await self._flush_direct(entries)
            return
        if len(results) != len(entries):
            await self._flush_direct(entries)
            return
        for entry, result in zip(entries, results, strict=True):
            entry.future.set_result(result)

    async def _flush_direct(self, entries: list[_RenameCommitEntry]) -> None:
        for entry in entries:
            try:
                rebind_ci_service(entry.context, self._svc)
                with use_sandbox_io_loop():
                    result = await run_sync_in_executor(
                        self._svc.commit_rename_plan,
                        entry.plan,
                        agent_id=resolved_agent_id(entry.context),
                        description=entry.description,
                    )
            except Exception as exc:
                entry.future.set_exception(exc)
            else:
                entry.future.set_result(result)


_PLAN_BATCHERS: dict[tuple[int, int], _RenamePlanBatcher] = {}
_COMMIT_BATCHERS: dict[tuple[int, int], _RenameCommitBatcher] = {}


def _rename_plan_batcher_for(svc: Any) -> _RenamePlanBatcher:
    loop = asyncio.get_running_loop()
    key = (id(svc), id(loop))
    batcher = _PLAN_BATCHERS.get(key)
    if batcher is None:
        batcher = _RenamePlanBatcher(svc)
        _PLAN_BATCHERS[key] = batcher
    return batcher


def _rename_commit_batcher_for(svc: Any) -> _RenameCommitBatcher:
    loop = asyncio.get_running_loop()
    key = (id(svc), id(loop))
    batcher = _COMMIT_BATCHERS.get(key)
    if batcher is None:
        batcher = _RenameCommitBatcher(svc)
        _COMMIT_BATCHERS[key] = batcher
    return batcher


class FileRenameSummary(BaseModel):
    file_path: str = Field(..., description="Absolute path of the changed file.")
    status: str = Field(..., description="`renamed` or `failed`.")
    message: str | None = Field(default=None, description="Failure reason when status=failed.")


class CandidateSymbol(BaseModel):
    name: str
    kind: str
    file_path: str
    line: int
    container: str = ""
    signature: str = ""


class DaytonaRenameSymbolsOutput(BaseModel):
    status: str = Field(
        ...,
        description=(
            "`renamed`, `no_changes`, `ambiguous` (multiple matches — nothing "
            "written), `no_match`, `aborted_version`, `aborted_overlap`, "
            "`aborted_lock`, or `failed`."
        ),
    )
    new_name: str = Field(..., description="Requested new identifier.")
    files: list[FileRenameSummary] = Field(
        default_factory=list,
        description="Per-file rename outcome (one entry per touched file).",
    )
    candidates: list[CandidateSymbol] = Field(
        default_factory=list,
        description="Populated only when status=='ambiguous'; up to 10 entries.",
    )
    warnings: list[str] = Field(
        default_factory=list,
        description="Advisory warnings (e.g., files outside write_scope).",
    )
    message: str | None = Field(default=None, description="Top-level status message.")


class DaytonaRenameSymbolsInput(BaseModel):
    symbol: str = Field(
        ...,
        min_length=1,
        description=(
            "Symbol to rename. Use a dotted name like `Foo.bar` to choose a method."
        ),
    )
    new_name: str = Field(..., min_length=1, description="New identifier to rename to.")
    kind: SymbolKind | None = Field(
        default=None,
        description=(
            "Optional filter: `function`, `class`, `method`, or `variable`."
        ),
    )
    file_hint: str | None = Field(
        default=None,
        description="Optional path text used to choose one matching symbol.",
    )


# -- Helpers ----------------------------------------------------------------


def _validate_new_name(new_name: str) -> str | None:
    if not re.match(_IDENTIFIER_RE, new_name):
        return f"Invalid identifier: {new_name!r}. Must match {_IDENTIFIER_RE}."
    if keyword.iskeyword(new_name):
        return f"Cannot rename to Python keyword: {new_name!r}."
    return None


def _candidate_payload(sym: Any) -> dict[str, Any]:
    return {
        "name": str(getattr(sym, "name", "")),
        "kind": str(getattr(getattr(sym, "kind", ""), "value", getattr(sym, "kind", ""))),
        "file_path": str(getattr(sym, "file_path", "")),
        "line": int(getattr(sym, "line", 0) or 0),
        "container": str(getattr(sym, "container", "") or ""),
        "signature": str(getattr(sym, "signature", "") or ""),
    }


def _symbol_name_column(sym: Any) -> int:
    """Best-effort column for the symbol name, not the declaration keyword."""
    indexed_column = int(getattr(sym, "character", 0) or 0)
    kind = getattr(sym, "kind", None)
    signature = str(getattr(sym, "signature", "") or "")
    if kind in {SymbolKind.FUNCTION, SymbolKind.METHOD} and signature.startswith("def "):
        return indexed_column + len("def ")
    if kind is SymbolKind.CLASS and signature.startswith("class "):
        return indexed_column + len("class ")
    return indexed_column


def _resolve_symbol(
    svc: Any,
    *,
    symbol: str,
    kind: SymbolKind | None,
    file_hint: str | None,
) -> list[Any]:
    """Find matching symbols in the workspace index."""
    parent: str | None = None
    leaf = symbol
    if "." in symbol:
        parent, _, leaf = symbol.rpartition(".")
    symbol_index = getattr(svc, "symbol_index", None)
    if symbol_index is None:
        return []
    try:
        symbol_index.ensure_built(wait=True)
    except Exception:  # pragma: no cover - defensive
        logger.debug("symbol_index.ensure_built failed", exc_info=True)
    try:
        raw = symbol_index.find(leaf, kind=kind)
    except Exception:  # pragma: no cover - defensive
        logger.debug("symbol_index.find failed", exc_info=True)
        return []

    matches = [m for m in raw if getattr(m, "name", "") == leaf]
    if parent is not None:
        matches = [m for m in matches if str(getattr(m, "container", "")) == parent]
    if file_hint:
        matches = [m for m in matches if file_hint in str(getattr(m, "file_path", ""))]
    return matches


async def _perform_rename(
    *,
    svc: Any,
    context: ToolExecutionContext,
    resolved_path: str,
    line: int,
    character: int,
    new_name: str,
    extra_warnings: list[str] | None = None,
    plan: Any | None = None,
) -> ToolResult:
    """Run one planned symbol rename."""
    if plan is None:
        try:
            plan = await _rename_plan_batcher_for(svc).submit(
                file_path=resolved_path,
                line=int(line),
                character=int(character),
                new_name=new_name,
            )
        except Exception as exc:  # pragma: no cover - defensive
            logger.debug("rename_symbol_plan raised for %s", resolved_path, exc_info=True)
            return ToolResult(output=f"LSP rename failed: {exc}", is_error=True)

    changes = getattr(plan, "changes", ()) or ()
    warnings = list(extra_warnings or [])
    if not changes:
        return ToolResult(
            output=json.dumps(
                {
                    "status": "no_changes",
                    "new_name": new_name,
                    "files": [],
                    "warnings": warnings,
                    "message": (
                        f"No rename changes produced for {resolved_path}:{line}. "
                        "Confirm the position points to a valid symbol and that "
                        "`new_name` is not already in use."
                    ),
                }
            ),
        )

    rebind_ci_service(context, svc)
    result = await _rename_commit_batcher_for(svc).submit(
        context,
        plan=plan,
        description=f"rename to {new_name}",
    )

    primary_paths = [change.file_path for change in changes]
    return operation_result_to_tool_result(
        result,
        tool_name="daytona_rename_symbol",
        success_status="renamed",
        primary_paths=primary_paths,
        warnings=warnings,
        success_extra={
            "new_name": new_name,
            "files": [
                {"file_path": path, "status": "renamed"}
                for path in primary_paths
            ],
        },
    )


def _preplanned_rename(
    context: ToolExecutionContext,
    *,
    svc: Any,
    symbol: str,
    new_name: str,
    kind: SymbolKind | None,
    file_hint: str | None,
    resolved_path: str,
    line: int,
    character: int,
) -> Any | None:
    cached = context.metadata.get(_RENAME_PREPLAN_CACHE_KEY)
    if _RENAME_PREPLAN_CACHE_KEY in context.metadata:
        del context.metadata.extras[_RENAME_PREPLAN_CACHE_KEY]
    if not isinstance(cached, dict):
        return None
    expected = (
        id(svc),
        symbol,
        new_name,
        str(kind or ""),
        str(file_hint or ""),
    )
    if cached.get("key") != expected:
        return None
    if cached.get("resolved_path") != resolved_path:
        return None
    if cached.get("line") != int(line) or cached.get("character") != int(character):
        return None
    return cached.get("plan")


@tool(
    name="daytona_rename_symbol",
    description=(
        "Rename a Python symbol across files. If the name matches more than one "
        "symbol, the tool returns candidates. Call again with `kind` or `file_hint` "
        "to choose one. Python only."
    ),
    short_description="Rename a symbol by name across every referencing file.",
    input_model=DaytonaRenameSymbolsInput,
    output_model=DaytonaRenameSymbolsOutput,
)
async def daytona_rename_symbol(
    symbol: str,
    new_name: str,
    kind: SymbolKind | None = None,
    file_hint: str | None = None,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Find one symbol and rename it."""
    svc = get_ci_service(context)
    if svc is None:
        return ci_required_result(
            "daytona_rename_symbol",
            "LSP rename is disabled without CI service.",
        )
    if not hasattr(svc, "rename_symbol_plan"):
        return ToolResult(output="LSP rename not available", is_error=True)
    invalid = _validate_new_name(new_name)
    if invalid is not None:
        return ToolResult(output=invalid, is_error=True)

    matches = _resolve_symbol(svc, symbol=symbol, kind=kind, file_hint=file_hint)

    if not matches:
        msg = (
            f"No symbol named {symbol!r} found in the workspace index. "
            "Try `ci_query_symbol` for discovery, or broaden the name."
        )
        return ToolResult(
            output=json.dumps(
                {
                    "status": "no_match",
                    "new_name": new_name,
                    "files": [],
                    "candidates": [],
                    "warnings": [],
                    "message": msg,
                }
            ),
            is_error=True,
        )

    if len(matches) > 1:
        truncated = matches[:_CANDIDATE_LIMIT]
        extra = len(matches) - len(truncated)
        more = f" (+{extra} more — refine with `kind` and/or `file_hint`)" if extra else ""
        msg = (
            f"{len(matches)} symbols match {symbol!r}. "
            f"Re-invoke with `kind=` or `file_hint=` to disambiguate.{more}"
        )
        return ToolResult(
            output=json.dumps(
                {
                    "status": "ambiguous",
                    "new_name": new_name,
                    "files": [],
                    "candidates": [_candidate_payload(m) for m in truncated],
                    "warnings": [],
                    "message": msg,
                }
            ),
            is_error=True,
            metadata={"candidate_count": len(matches)},
        )

    sym = matches[0]
    resolved_path = resolve_daytona_path(str(getattr(sym, "file_path", "")), context)
    pivot_line = int(getattr(sym, "line", 0) or 0)
    pivot_char = _symbol_name_column(sym)
    plan = _preplanned_rename(
        context,
        svc=svc,
        symbol=symbol,
        new_name=new_name,
        kind=kind,
        file_hint=file_hint,
        resolved_path=resolved_path,
        line=pivot_line,
        character=pivot_char,
    )
    return await _perform_rename(
        svc=svc,
        context=context,
        resolved_path=resolved_path,
        line=pivot_line,
        character=pivot_char,
        new_name=new_name,
        plan=plan,
    )
