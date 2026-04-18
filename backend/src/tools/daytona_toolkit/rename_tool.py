"""Daytona-backed cross-file symbol rename tools.

Exposes one tool:

* ``daytona_rename_symbol(symbol, new_name, kind=?, file_hint=?)`` — resolves the
  symbol name via :class:`SymbolIndex`, returns ``status="ambiguous"`` with
  candidates when a name matches multiple places, and otherwise delegates to
  a single audited process command.
"""

from __future__ import annotations

import base64
import json
import keyword
import logging
import re
import shlex
from typing import Any

from code_intelligence.types import SymbolKind
from pydantic import BaseModel, Field

from tools.core.base import ToolExecutionContext, ToolResult
from tools.core.ci_runtime import (
    ci_required_result,
    exec_ci_process_operation,
    get_ci_service,
)
from tools.core.decorator import tool
from tools.core.sandbox_runtime import resolve_daytona_path
from tools.daytona_toolkit._daytona_utils import (
    _extract_exit_code,
    _require_sandbox,
    _team_repo_write_error,
    _team_repo_write_warning,
    _wrap_bash_command,
)

logger = logging.getLogger(__name__)

_IDENTIFIER_RE = r"^[A-Za-z_][A-Za-z0-9_]*$"
_CANDIDATE_LIMIT = 10
_PROCESS_RENAME_TIMEOUT = 180
_PROCESS_RENAME_SCRIPT = r"""
import base64
import json
import os
import pathlib
import sys
import tempfile


payload = json.loads(base64.b64decode(sys.argv[1]).decode("utf-8"))
changes = payload.get("files", [])
temps = []
try:
    for change in changes:
        file_path = str(change["file_path"])
        path = pathlib.Path(file_path)
        final_content = change.get("final_content")
        if final_content is None:
            if path.exists():
                path.unlink()
        else:
            parent = path.parent
            parent.mkdir(parents=True, exist_ok=True)
            fd, tmp = tempfile.mkstemp(prefix=f".{path.name}.", suffix=".tmp", dir=str(parent))
            temps.append(tmp)
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                handle.write(str(final_content))
            os.replace(tmp, path)
            temps.remove(tmp)
    results = [{"file_path": str(change["file_path"]), "status": "renamed"} for change in changes]
except Exception as exc:
    print(json.dumps({"ok": False, "status": "failed", "error": str(exc)}))
    raise SystemExit(1)
finally:
    for tmp in list(temps):
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass

print(json.dumps({"ok": True, "status": "renamed", "files": results}))
"""


# -- Shared schemas ---------------------------------------------------------


class FileRenameSummary(BaseModel):
    file_path: str = Field(..., description="Absolute path of the changed file.")
    status: str = Field(..., description="`renamed`, `dry_run`, or `failed`.")
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
            "`renamed`, `dry_run`, `no_changes`, `ambiguous` (multiple "
            "matches — nothing written), `no_match`, or `failed`."
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
        description="Resolve and validate the rename plan without writing anything.",
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


def _rename_process_command(changes: tuple[Any, ...]) -> str:
    payload = {
        "files": [
            {
                "file_path": change.file_path,
                "final_content": change.final_content,
            }
            for change in changes
        ],
    }
    encoded = base64.b64encode(json.dumps(payload).encode("utf-8")).decode("ascii")
    return _wrap_bash_command(
        f"python3 -c {shlex.quote(_PROCESS_RENAME_SCRIPT)} {shlex.quote(encoded)}"
    )


async def _perform_rename(
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
    """Shared body: build a SemanticRenamePlan and run one audited process command."""
    try:
        planner = svc.rename_symbol_plan
        preview_planner = getattr(svc, "preview_rename_symbol_plan", None)
        if dry_run and callable(preview_planner):
            planner = preview_planner
        plan = planner(resolved_path, int(line), int(character), new_name)
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
        err = _team_repo_write_error(context, path, tool_name="daytona_rename_symbol")
        if err is not None:
            hard_errors.append(err)
            continue
        warn = _team_repo_write_warning(context, path, tool_name="daytona_rename_symbol")
        if warn is not None:
            soft_warnings.append(warn)
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

    operation_changes = tuple(changes)
    description = f"rename to {new_name}"
    try:
        sandbox = await _require_sandbox(context)
        response = await exec_ci_process_operation(
            context,
            sandbox,
            _rename_process_command(operation_changes),
            timeout=_PROCESS_RENAME_TIMEOUT,
            description=description,
        )
    except Exception as exc:
        return ToolResult(
            output=json.dumps(
                {
                    "status": "failed",
                    "new_name": new_name,
                    "files": [
                        {"file_path": change.file_path, "status": "failed", "message": str(exc)}
                        for change in operation_changes
                    ],
                    "warnings": soft_warnings,
                    "message": f"Rename failed during process execution: {exc}",
                }
            ),
            is_error=True,
        )

    raw = str(getattr(response, "result", "") or "")
    cleaned, exit_code = _extract_exit_code(
        raw,
        fallback_exit_code=getattr(response, "exit_code", None),
    )
    try:
        payload = json.loads(cleaned or "{}")
    except json.JSONDecodeError:
        payload = {"ok": False, "status": "failed", "error": cleaned or "rename process failed"}

    if exit_code not in (0, None) or not bool(payload.get("ok", False)):
        message = str(payload.get("error") or cleaned or "rename process failed")
        return ToolResult(
            output=json.dumps(
                {
                    "status": "failed",
                    "new_name": new_name,
                    "files": [
                        {"file_path": change.file_path, "status": "failed", "message": message}
                        for change in operation_changes
                    ],
                    "warnings": soft_warnings,
                    "message": message,
                }
            ),
            is_error=True,
            metadata={"file_count": len(operation_changes), "success_count": 0},
        )

    files = [
        {"file_path": str(item.get("file_path") or ""), "status": "renamed"}
        for item in (payload.get("files") or [])
        if isinstance(item, dict) and item.get("file_path")
    ]
    if not files:
        files = [{"file_path": change.file_path, "status": "renamed"} for change in operation_changes]
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


# -- Tool: daytona_rename_symbol -------------------------------------------


@tool(
    name="daytona_rename_symbol",
    description=(
        "Rename a Python symbol by name across every file where it is referenced "
        "inside the Daytona sandbox, using LSP semantics. Resolves `symbol` via "
        "the workspace symbol index, "
        "supports dotted names (`Foo.bar` narrows to the `bar` method on class "
        "`Foo`), and returns `status=\"ambiguous\"` with candidates when the "
        "name is not unique. Executes the resulting rewrite as one audited "
        "process operation. Python-only for now."
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
    dry_run: bool = False,
    *,
    context: ToolExecutionContext,
) -> ToolResult:
    """Resolve *symbol* then run one audited process operation for the rename."""
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
    return await _perform_rename(
        svc=svc,
        context=context,
        resolved_path=resolved_path,
        line=pivot_line,
        character=pivot_char,
        new_name=new_name,
        dry_run=dry_run,
    )
