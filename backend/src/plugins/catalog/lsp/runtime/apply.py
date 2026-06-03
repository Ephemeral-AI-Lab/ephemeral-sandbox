"""Apply LSP WorkspaceEdit payloads to the daemon overlay workspace."""

from __future__ import annotations

import os
import shutil
from pathlib import Path
from typing import Any
from urllib.parse import unquote, urlparse


async def apply_workspace_edit(
    edit: dict[str, Any],
    ctx: Any,
    *,
    workspace_root: str | None = None,
    expected_manifest_key: str | None = None,
) -> dict[str, Any]:
    workspace_root = str(workspace_root or ctx.overlay.workspace_root)
    publish_mounted = getattr(ctx.overlay, "publish_mounted_workspace_changes", None)
    if callable(publish_mounted):
        return await _apply_with_mounted_workspace_callback(
            edit,
            ctx,
            workspace_root=workspace_root,
            expected_manifest_key=expected_manifest_key,
            publish_mounted=publish_mounted,
        )
    raise RuntimeError("LSP WorkspaceEdit requires daemon mounted-workspace callback")


async def _apply_with_mounted_workspace_callback(
    edit: dict[str, Any],
    ctx: Any,
    *,
    workspace_root: str,
    expected_manifest_key: str | None,
    publish_mounted: Any,
) -> dict[str, Any]:
    active_manifest_key = ""
    active_manifest_key_fn = getattr(ctx.overlay, "active_manifest_key", None)
    if callable(active_manifest_key_fn):
        active_manifest_key = str(active_manifest_key_fn())
    if (
        expected_manifest_key
        and active_manifest_key
        and active_manifest_key != expected_manifest_key
    ):
        raise RuntimeError(
            "workspace changed before LSP edit could be applied; retry the tool"
        )
    changed_paths = _apply_edit_payload(edit, workspace_root=workspace_root)
    publish = await publish_mounted(changed_paths, workspace_root=workspace_root)
    timings = dict(publish.get("timings") or {})
    if "occ.apply.total_s" in timings:
        timings.setdefault("command_exec.occ_apply_s", float(timings["occ.apply.total_s"]))
    timings.setdefault("command_exec.capture_upperdir_s", 0.0)
    return {
        "success": bool(publish.get("success")),
        "changed_paths": changed_paths,
        "manifest_version": publish.get("published_manifest_version"),
        "files": publish.get("files") or [],
        "timings": timings,
    }


def _apply_edit_payload(edit: dict[str, Any], *, workspace_root: str) -> list[str]:
    root = Path(workspace_root).resolve(strict=False)
    changed: list[str] = []
    changes: dict[str, list[dict[str, Any]]] = {}
    raw_changes = edit.get("changes")
    if isinstance(raw_changes, dict):
        for uri, edits in raw_changes.items():
            if isinstance(edits, list):
                changes[str(uri)] = [e for e in edits if isinstance(e, dict)]
    for uri, edits in changes.items():
        path = _uri_to_path(uri, workspace_root=root)
        _apply_text_edits(path, edits)
        changed.append(path.resolve(strict=False).relative_to(root).as_posix())

    document_changes = edit.get("documentChanges")
    if isinstance(document_changes, list):
        for entry in document_changes:
            if not isinstance(entry, dict):
                continue
            kind = entry.get("kind")
            if kind == "create":
                changed.extend(_apply_create_file(entry, workspace_root=root))
                continue
            if kind == "delete":
                changed.extend(_apply_delete_file(entry, workspace_root=root))
                continue
            if kind == "rename":
                changed.extend(_apply_rename_file(entry, workspace_root=root))
                continue
            text_document = entry.get("textDocument")
            edits = entry.get("edits")
            if isinstance(text_document, dict) and isinstance(edits, list):
                uri = str(text_document.get("uri") or "")
                path = _uri_to_path(uri, workspace_root=root)
                _apply_text_edits(path, [e for e in edits if isinstance(e, dict)])
                changed.append(path.resolve(strict=False).relative_to(root).as_posix())
    return sorted(set(changed))


def _apply_create_file(
    entry: dict[str, Any],
    *,
    workspace_root: Path,
) -> list[str]:
    uri = str(entry.get("uri") or "")
    path = _uri_to_path(uri, workspace_root=workspace_root)
    options = entry.get("options") if isinstance(entry.get("options"), dict) else {}
    if path.exists():
        if bool(options.get("ignoreIfExists")):
            return []
        if not bool(options.get("overwrite")):
            raise FileExistsError(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("", encoding="utf-8")
    return [path.relative_to(workspace_root).as_posix()]


def _apply_delete_file(
    entry: dict[str, Any],
    *,
    workspace_root: Path,
) -> list[str]:
    uri = str(entry.get("uri") or "")
    path = _uri_to_path(uri, workspace_root=workspace_root)
    options = entry.get("options") if isinstance(entry.get("options"), dict) else {}
    rel = path.relative_to(workspace_root).as_posix()
    if not path.exists() and not path.is_symlink():
        if bool(options.get("ignoreIfNotExists")):
            return []
        raise FileNotFoundError(path)
    if path.is_dir() and not path.is_symlink():
        if not bool(options.get("recursive")):
            raise IsADirectoryError(path)
        shutil.rmtree(path)
    else:
        path.unlink()
    return [rel]


def _apply_rename_file(
    entry: dict[str, Any],
    *,
    workspace_root: Path,
) -> list[str]:
    old_path = _uri_to_path(str(entry.get("oldUri") or ""), workspace_root=workspace_root)
    new_path = _uri_to_path(str(entry.get("newUri") or ""), workspace_root=workspace_root)
    options = entry.get("options") if isinstance(entry.get("options"), dict) else {}
    if not old_path.exists() and not old_path.is_symlink():
        raise FileNotFoundError(old_path)
    if new_path.exists() or new_path.is_symlink():
        if bool(options.get("ignoreIfExists")):
            return []
        if not bool(options.get("overwrite")):
            raise FileExistsError(new_path)
        if new_path.is_dir() and not new_path.is_symlink():
            shutil.rmtree(new_path)
        else:
            new_path.unlink()
    new_path.parent.mkdir(parents=True, exist_ok=True)
    os.replace(old_path, new_path)
    return [
        old_path.relative_to(workspace_root).as_posix(),
        new_path.relative_to(workspace_root).as_posix(),
    ]


def _apply_text_edits(path: Path, edits: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    text = path.read_text(encoding="utf-8") if path.exists() else ""
    line_starts = _line_starts(text)
    replacements: list[tuple[int, int, str]] = []
    for edit in edits:
        range_obj = edit.get("range")
        new_text = str(edit.get("newText", ""))
        if not isinstance(range_obj, dict):
            replacements.append((0, len(text), new_text))
            continue
        start = _offset(line_starts, range_obj.get("start"))
        end = _offset(line_starts, range_obj.get("end"))
        replacements.append((start, end, new_text))
    for start, end, new_text in sorted(replacements, reverse=True):
        text = text[:start] + new_text + text[end:]
    path.write_text(text, encoding="utf-8")


def _line_starts(text: str) -> list[int]:
    starts = [0]
    for index, char in enumerate(text):
        if char == "\n":
            starts.append(index + 1)
    return starts


def _offset(line_starts: list[int], position: object) -> int:
    if not isinstance(position, dict):
        return 0
    line = max(0, int(position.get("line") or 0))
    character = max(0, int(position.get("character") or 0))
    if line >= len(line_starts):
        return line_starts[-1]
    return line_starts[line] + character


def _uri_to_path(uri: str, *, workspace_root: Path) -> Path:
    parsed = urlparse(uri)
    if parsed.scheme == "file":
        raw = unquote(parsed.path)
    elif parsed.scheme:
        raise ValueError(f"unsupported WorkspaceEdit URI scheme: {parsed.scheme}")
    else:
        raw = uri
    candidate = Path(raw)
    path = candidate if candidate.is_absolute() else workspace_root / candidate
    resolved = path.resolve(strict=False)
    try:
        resolved.relative_to(workspace_root)
    except ValueError:
        raise ValueError(f"WorkspaceEdit path is outside workspace: {uri}") from None
    return resolved


__all__ = ["apply_workspace_edit"]
