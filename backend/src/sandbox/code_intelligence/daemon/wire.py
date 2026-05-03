"""Wire-format helpers for the transport-backed CI daemon backend."""

from __future__ import annotations

import dataclasses
from collections.abc import Sequence
from typing import Any

from sandbox.code_intelligence.core.types import (
    CITelemetry,
    DeleteSpec,
    Diagnostic,
    EditRequest,
    EditResult,
    EditSpec,
    HoverResult,
    MoveSpec,
    OperationChange,
    OperationResult,
    ReferenceInfo,
    SymbolInfo,
    WriteSpec,
)


def normalize_write_specs(
    specs: Sequence[WriteSpec] | WriteSpec,
) -> list[WriteSpec]:
    return [specs] if isinstance(specs, WriteSpec) else list(specs)


def normalize_edit_specs(
    specs: Sequence[EditSpec] | EditSpec,
) -> list[EditSpec]:
    return [specs] if isinstance(specs, EditSpec) else list(specs)


def writespec_to_dict(spec: WriteSpec) -> dict[str, Any]:
    return {
        "file_path": spec.file_path,
        "content": spec.content,
        "overwrite": spec.overwrite,
    }


def editspec_to_dict(spec: EditSpec) -> dict[str, Any]:
    return {
        "file_path": spec.file_path,
        "edits": list(spec.edits),
    }


def movespec_to_dict(spec: MoveSpec) -> dict[str, Any]:
    return {
        "src_path": spec.src_path,
        "dst_path": spec.dst_path,
        "overwrite": spec.overwrite,
        "is_folder": spec.is_folder,
    }


def deletespec_to_dict(spec: DeleteSpec) -> dict[str, Any]:
    return {"path": spec.path, "is_folder": spec.is_folder}


def operation_change_to_dict(change: OperationChange) -> dict[str, Any]:
    return {
        "file_path": change.file_path,
        "base_content": change.base_content,
        "base_hash": change.base_hash,
        "final_content": change.final_content,
        "base_existed": change.base_existed,
        "strict_base": change.strict_base,
    }


def edit_request_to_dict(request: EditRequest) -> dict[str, Any]:
    return {
        "file_path": request.file_path,
        "old_text": request.old_text,
        "new_text": request.new_text,
        "agent_id": request.agent_id,
        "description": request.description,
    }


def symbol_info_from_dict(d: dict[str, Any]) -> SymbolInfo:
    from sandbox.code_intelligence.core.types import SymbolKind

    kind_raw = d.get("kind")
    if isinstance(kind_raw, SymbolKind):
        kind = kind_raw
    elif isinstance(kind_raw, str):
        try:
            kind = SymbolKind(kind_raw)
        except ValueError:
            kind = SymbolKind.OTHER if hasattr(SymbolKind, "OTHER") else SymbolKind.CLASS
    else:
        kind = SymbolKind.CLASS
    return SymbolInfo(
        name=str(d.get("name", "")),
        kind=kind,
        file_path=str(d.get("file_path", "")),
        line=int(d.get("line", 0)),
        end_line=d.get("end_line"),
        character=int(d.get("character", 0)),
        signature=str(d.get("signature", "")),
        docstring=str(d.get("docstring", "")),
        container=str(d.get("container", "")),
    )


def reference_info_from_dict(d: dict[str, Any]) -> ReferenceInfo:
    return ReferenceInfo(
        file_path=str(d.get("file_path", "")),
        line=int(d.get("line", 0)),
        character=int(d.get("character", 0)),
        text=str(d.get("text", "")),
    )


def hover_result_from_dict(d: dict[str, Any]) -> HoverResult:
    sym_dict = d.get("symbol")
    symbol = symbol_info_from_dict(sym_dict) if sym_dict else None
    return HoverResult(
        content=str(d.get("content", "")),
        language=str(d.get("language", "")),
        symbol=symbol,
    )


def diagnostic_from_dict(d: dict[str, Any]) -> Diagnostic:
    from sandbox.code_intelligence.core.types import DiagnosticSeverity

    severity_raw = d.get("severity")
    if isinstance(severity_raw, DiagnosticSeverity):
        severity = severity_raw
    elif isinstance(severity_raw, str):
        try:
            severity = DiagnosticSeverity(severity_raw)
        except ValueError:
            severity = DiagnosticSeverity.ERROR
    else:
        severity = DiagnosticSeverity.ERROR
    return Diagnostic(
        file_path=str(d.get("file_path", "")),
        line=int(d.get("line", 0)),
        character=int(d.get("character", 0)),
        end_line=d.get("end_line"),
        end_character=d.get("end_character"),
        severity=severity,
        message=str(d.get("message", "")),
        source=str(d.get("source", "")),
        code=str(d.get("code", "")),
    )


def edit_result_from_dict(d: dict[str, Any]) -> EditResult:
    return EditResult(
        success=bool(d.get("success", False)),
        file_path=str(d.get("file_path", "")),
        message=str(d.get("message", "")),
        conflict=bool(d.get("conflict", False)),
        conflict_reason=str(d.get("conflict_reason", "")),
        snapshot_id=str(d.get("snapshot_id", "")),
        timings=dict(d.get("timings") or {}),
    )


def operation_result_from_dict(d: dict[str, Any]) -> OperationResult:
    files = tuple(edit_result_from_dict(f) for f in (d.get("files") or ()))
    status = d.get("status", "failed")
    return OperationResult(
        success=bool(d.get("success", False)),
        status=status,  # type: ignore[arg-type]
        files=files,
        conflict_file=d.get("conflict_file"),
        conflict_reason=str(d.get("conflict_reason", "")),
        timings=dict(d.get("timings") or {}),
    )


def telemetry_from_dict(d: dict[str, Any]) -> CITelemetry:
    """Reconstruct a :class:`CITelemetry` from its asdict() shape."""
    if isinstance(d, CITelemetry):
        return d
    init = {
        f.name: d.get(f.name)
        for f in dataclasses.fields(CITelemetry)
        if f.name in d
    }
    try:
        return CITelemetry(**init)  # type: ignore[arg-type]
    except TypeError:
        return CITelemetry()
