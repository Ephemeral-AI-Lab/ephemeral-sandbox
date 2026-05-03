"""Wire-format helpers for runtime OCC requests and responses."""

from __future__ import annotations

import base64
from collections.abc import Sequence
from dataclasses import asdict, dataclass, is_dataclass
from typing import Any

from sandbox.occ.changeset.types import UpperChangeLike
from sandbox.occ.types import (
    EditResult,
    EditSpec,
    OperationChange,
    OperationResult,
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
        "edits": [_edit_to_dict(edit) for edit in spec.edits],
    }


def writespec_from_dict(d: dict[str, Any]) -> WriteSpec:
    return WriteSpec(
        file_path=str(d["file_path"]),
        content=str(d.get("content", "")),
        overwrite=bool(d.get("overwrite", True)),
    )


def editspec_from_dict(d: dict[str, Any]) -> EditSpec:
    return EditSpec(
        file_path=str(d["file_path"]),
        edits=tuple(_edit_from_dict(edit) for edit in d.get("edits", ())),
    )


def _edit_to_dict(edit: Any) -> dict[str, Any]:
    if is_dataclass(edit) and not isinstance(edit, type):
        data = asdict(edit)
    elif isinstance(edit, dict):
        data = dict(edit)
    else:
        data = dict(vars(edit))
    if {"start_line", "end_line"} <= set(data):
        raise ValueError("unsupported edit kind: line_range")
    kind = str(data.get("kind") or "")
    if kind and kind != "search_replace":
        raise ValueError(f"unsupported edit kind: {kind}")
    if "old_text" not in data:
        raise ValueError("edit requires old_text")
    if "new_text" not in data:
        raise ValueError("edit requires new_text")
    data["kind"] = "search_replace"
    return data


def _edit_from_dict(d: dict[str, Any]) -> Any:
    from sandbox.occ.patching.patcher import SearchReplaceEdit

    kind = str(d.get("kind") or "")
    if {"start_line", "end_line"} <= set(d):
        raise ValueError("unsupported edit kind: line_range")
    if kind and kind != "search_replace":
        raise ValueError(f"unsupported edit kind: {kind}")
    if "old_text" not in d:
        raise ValueError("edit requires old_text")
    if "new_text" not in d:
        raise ValueError("edit requires new_text")
    return SearchReplaceEdit(
        old_text=str(d["old_text"]),
        new_text=str(d["new_text"]),
    )


def operation_change_to_dict(change: OperationChange) -> dict[str, Any]:
    return {
        "file_path": change.file_path,
        "base_content": change.base_content,
        "base_hash": change.base_hash,
        "final_content": change.final_content,
        "base_existed": change.base_existed,
        "strict_base": change.strict_base,
    }


def operation_change_from_dict(d: dict[str, Any]) -> OperationChange:
    return OperationChange(
        file_path=str(d["file_path"]),
        base_content=str(d.get("base_content", "")),
        base_hash=str(d.get("base_hash", "")),
        final_content=d.get("final_content"),
        base_existed=bool(d.get("base_existed", True)),
        strict_base=bool(d.get("strict_base", False)),
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
        status=status,
        files=files,
        conflict_file=d.get("conflict_file"),
        conflict_reason=str(d.get("conflict_reason", "")),
        timings=dict(d.get("timings") or {}),
    )


@dataclass
class _UpperChange:
    rel: str
    kind: str
    base_bytes: bytes | None
    upper_bytes: bytes | None
    base_existed: bool


def _bytes_from_wire(value: Any) -> bytes | None:
    if value is None:
        return None
    return base64.b64decode(str(value).encode("ascii"))


def upper_change_from_dict(d: dict[str, Any]) -> UpperChangeLike:
    return _UpperChange(
        rel=str(d["rel"]),
        kind=str(d["kind"]),
        base_bytes=_bytes_from_wire(d.get("base_bytes")),
        upper_bytes=_bytes_from_wire(d.get("upper_bytes")),
        base_existed=bool(d.get("base_existed", True)),
    )
