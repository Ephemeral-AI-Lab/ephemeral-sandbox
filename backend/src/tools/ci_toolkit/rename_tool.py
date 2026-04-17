"""Cross-file symbol rename tools backed by the code intelligence LSP.

Exposes two tools:

* ``ci_rename_symbol(file_path, line, character, new_name)`` — the position-
  driven primitive. Use it when you already know a definition/reference's
  coordinates.
* ``ci_rename(symbol, new_name, kind=?, file_hint=?)`` — the ergonomic
  facade. Resolves the symbol name via :class:`SymbolIndex`, returns
  ``status="ambiguous"`` with candidates when a name matches multiple
  places, and otherwise delegates to the same batch OCC commit.

Both route through ``commit_many_against_base``: the whole rename lands or
none of it does — never leaves a half-renamed tree.
"""

from __future__ import annotations

import difflib
import json
import logging
import re
from typing import Any

from code_intelligence.types import SymbolKind
from pydantic import BaseModel, Field

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_runtime import get_ci_service
from tools.core.decorator import tool
from tools.core.sandbox_runtime import resolve_daytona_path
from tools.daytona_toolkit._daytona_utils import (
    _team_repo_write_error,
    _team_repo_write_warning,
    record_coordination_warning,
)

logger = logging.getLogger(__name__)

_DIFF_MAX_CHARS = 8000
_IDENTIFIER_RE = r"^[A-Za-z_][A-Za-z0-9_]*$"
_CANDIDATE_LIMIT = 10


# -- Shared schemas ---------------------------------------------------------


class FileRenameSummary(BaseModel):
    file_path: str = Field(..., description="Absolute path of the changed file.")
    status: str = Field(..., description="`renamed`, `dry_run`, or `failed`.")
    diff: str | None = Field(default=None, description="Unified diff for dry-run.")
    message: str | None = Field(default=None, description="Failure reason when status=failed.")


class CandidateSymbol(BaseModel):
    name: str
    kind: str
    file_path: str
    line: int
    container: str = ""
    signature: str = ""


class CiRenameSymbolOutput(BaseModel):
    status: str = Field(
        ...,
        description=(
            "`renamed`, `dry_run`, `no_changes`, `ambiguous` (multiple "
            "matches — nothing written), `no_match`, `aborted` (OCC/merge "
            "conflict — nothing written), or `failed`."
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


# -- Position-based tool input ---------------------------------------------


class CiRenameSymbolInput(BaseModel):
    file_path: str = Field(
        ...,
        description="File containing the symbol's definition or a reference.",
    )
    line: int = Field(..., ge=1, description="One-based line number of the symbol.")
    character: int = Field(
        default=0,
        ge=0,
        description=(
            "Zero-based column of the symbol. Pass 0 to auto-resolve to the first non-"
            "whitespace column (handles `def`/`class` lines correctly)."
        ),
    )
    new_name: str = Field(..., min_length=1, description="New identifier to rename to.")
    dry_run: bool = Field(
        default=False,
        description="Preview the per-file diffs without writing anything.",
    )


# -- Name-based facade input -----------------------------------------------


class CiRenameInput(BaseModel):
    symbol: str = Field(
        ...,
        min_length=1,
        description=(
            "Target symbol name; may be dotted (e.g. `Foo.bar`, "
            "`module.func`) to disambiguate a method from a module-level "
            "function with the same leaf name."
        ),
    )
    new_name: str = Field(..., min_length=1, description="New identifier to rename to.")
    kind: SymbolKind | None = Field(
        default=None,
        description=(
            "Optional disambiguator: `function`, `class`, `method`, "
            "`variable`. Narrows candidates before the ambiguity check."
        ),
    )
    file_hint: str | None = Field(
        default=None,
        description=(
            "Optional substring match against the absolute file path "
            "(e.g. `backend/src/foo/`) to narrow candidates."
        ),
    )
    dry_run: bool = Field(
        default=False,
        description="Preview the per-file diffs without writing anything.",
    )


# -- Helpers ----------------------------------------------------------------


def _validate_new_name(new_name: str) -> str | None:
    if not re.match(_IDENTIFIER_RE, new_name):
        return f"Invalid identifier: {new_name!r}. Must match {_IDENTIFIER_RE}."
    if new_name in {"None", "True", "False"}:
        return f"Cannot rename to Python keyword: {new_name!r}."
    return None


def _unified_diff(old: str, new: str, path: str) -> str:
    diff = "".join(
        difflib.unified_diff(
            old.splitlines(keepends=True),
            new.splitlines(keepends=True),
            fromfile=f"a/{path}",
            tofile=f"b/{path}",
        )
    )
    if len(diff) > _DIFF_MAX_CHARS:
        diff = diff[:_DIFF_MAX_CHARS] + "\n... (truncated)"
    return diff


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
    """Resolve *symbol* via the workspace symbol index.

    Supports dotted names (``Foo.bar`` — leaf ``bar`` filtered by
    container ``Foo``) and optional ``kind``/``file_hint`` narrowing.
    """
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


def _perform_rename(
    *,
    svc: Any,
    context: ToolExecutionContext,
    resolved_path: str,
    line: int,
    character: int,
    new_name: str,
    dry_run: bool,
    extra_warnings: list[str] | None = None,
) -> ToolResult:
    """Shared body: build a SemanticRenamePlan and dispatch the batch commit."""
    try:
        plan = svc.rename_symbol_plan(resolved_path, int(line), int(character), new_name)
    except Exception as exc:  # pragma: no cover - defensive
        logger.debug("rename_symbol_plan raised for %s", resolved_path, exc_info=True)
        return ToolResult(output=f"LSP rename failed: {exc}", is_error=True)

    changes = getattr(plan, "changes", ()) or ()
    if not changes:
        return ToolResult(
            output=json.dumps(
                {
                    "status": "no_changes",
                    "new_name": new_name,
                    "files": [],
                    "warnings": list(extra_warnings or []),
                    "message": (
                        f"No rename changes produced for {resolved_path}:{line}. "
                        "Confirm the position points to a valid symbol and that "
                        "`new_name` is not already in use."
                    ),
                }
            ),
        )

    hard_errors: list[str] = []
    soft_warnings: list[str] = list(extra_warnings or [])
    for change in changes:
        path = change.file_path
        err = _team_repo_write_error(context, path, tool_name="ci_rename_symbol")
        if err is not None:
            hard_errors.append(err)
            continue
        warn = _team_repo_write_warning(context, path, tool_name="ci_rename_symbol")
        if warn is not None:
            soft_warnings.append(warn)
            record_coordination_warning(
                context, category="write_scope", message=warn,
            )
    if hard_errors:
        return ToolResult(
            output=(
                "Rename blocked by write-scope policy:\n  - "
                + "\n  - ".join(hard_errors)
            ),
            is_error=True,
        )

    if dry_run:
        file_summaries = [
            {
                "file_path": change.file_path,
                "status": "dry_run",
                "diff": _unified_diff(
                    change.base_content, change.final_content, change.file_path,
                ),
            }
            for change in changes
        ]
        return ToolResult(
            output=json.dumps(
                {
                    "status": "dry_run",
                    "new_name": new_name,
                    "files": file_summaries,
                    "warnings": soft_warnings,
                }
            ),
            metadata={"dry_run": True, "file_count": len(file_summaries)},
        )

    agent_id = str(
        context.metadata.get("agent_run_id")
        or context.metadata.get("agent_id")
        or "",
    )
    result = svc.commit_many_against_base(
        changes,
        agent_id=agent_id,
        edit_type="rename",
        description=f"rename to {new_name}",
        expected_arbiter_generation=plan.arbiter_generation,
    )

    if result.success:
        files = [
            {"file_path": f.file_path, "status": "renamed"}
            for f in result.files
        ]
        return ToolResult(
            output=json.dumps(
                {
                    "status": "renamed",
                    "new_name": new_name,
                    "files": files,
                    "warnings": soft_warnings,
                    "message": None,
                }
            ),
            metadata={
                "file_count": len(files),
                "success_count": len(files),
            },
        )

    aborted = result.status.startswith("aborted")
    top_status = "aborted" if aborted else "failed"
    message = (
        f"Rename aborted ({result.status}): {result.conflict_reason}. "
        "Re-read the affected file(s) and retry."
        if aborted
        else f"Rename failed during commit: {result.conflict_reason}."
    )
    files_out = [
        {
            "file_path": f.file_path,
            "status": "failed",
            "message": f.message or result.conflict_reason,
        }
        for f in result.files
    ]
    return ToolResult(
        output=json.dumps(
            {
                "status": top_status,
                "new_name": new_name,
                "files": files_out,
                "warnings": soft_warnings,
                "message": message,
            }
        ),
        is_error=True,
        metadata={
            "file_count": len(files_out),
            "success_count": 0,
            "conflict_file": result.conflict_file,
            "conflict_reason": result.conflict_reason,
            "batch_status": result.status,
        },
    )


# -- Tool: ci_rename_symbol (position-based) -------------------------------


@tool(
    name="ci_rename_symbol",
    description=(
        "Rename a Python symbol at a specific ``(file, line, character)`` across "
        "every file where it is referenced, using LSP semantics. Atomic: the "
        "whole rename commits or none of it does. Prefer `ci_rename(symbol, "
        "new_name)` when you only know the name — it resolves coordinates for "
        "you. Python-only for now."
    ),
    short_description="Rename a symbol at a position across every referencing file (atomic).",
    input_model=CiRenameSymbolInput,
    output_model=CiRenameSymbolOutput,
)
async def ci_rename_symbol(
    file_path: str,
    line: int,
    new_name: str,
    character: int = 0,
    dry_run: bool = False,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Apply an LSP-driven symbol rename across all affected files atomically."""
    svc = get_ci_service(context)
    if svc is None or not hasattr(svc, "rename_symbol_plan"):
        return ToolResult(output="LSP rename not available", is_error=True)
    invalid = _validate_new_name(new_name)
    if invalid is not None:
        return ToolResult(output=invalid, is_error=True)
    resolved = resolve_daytona_path(file_path, context)
    return _perform_rename(
        svc=svc,
        context=context,
        resolved_path=resolved,
        line=int(line),
        character=int(character),
        new_name=new_name,
        dry_run=dry_run,
    )


# -- Tool: ci_rename (name-based facade) -----------------------------------


@tool(
    name="ci_rename",
    description=(
        "Rename a Python symbol by name — no coordinates needed. Resolves "
        "`symbol` via the workspace symbol index, supports dotted names "
        "(`Foo.bar` narrows to the `bar` method on class `Foo`), and when "
        "the name is ambiguous returns `status=\"ambiguous\"` with up to "
        "10 candidates so the caller can re-invoke with `kind` and/or "
        "`file_hint`. Otherwise commits the rename atomically across every "
        "referencing file. Python-only."
    ),
    short_description="Rename a symbol by name (resolves coordinates, atomic).",
    input_model=CiRenameInput,
    output_model=CiRenameSymbolOutput,
)
async def ci_rename(
    symbol: str,
    new_name: str,
    kind: SymbolKind | None = None,
    file_hint: str | None = None,
    dry_run: bool = False,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Resolve *symbol* then delegate to the atomic batch rename."""
    svc = get_ci_service(context)
    if svc is None or not hasattr(svc, "rename_symbol_plan"):
        return ToolResult(output="LSP rename not available", is_error=True)
    invalid = _validate_new_name(new_name)
    if invalid is not None:
        return ToolResult(output=invalid, is_error=True)

    matches = _resolve_symbol(
        svc, symbol=symbol, kind=kind, file_hint=file_hint,
    )

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
    return _perform_rename(
        svc=svc,
        context=context,
        resolved_path=resolved_path,
        line=pivot_line,
        character=pivot_char,
        new_name=new_name,
        dry_run=dry_run,
    )
