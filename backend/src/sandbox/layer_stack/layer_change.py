"""Storage-level layer change objects.

These values describe already-accepted filesystem mutations. They deliberately
do not encode OCC policy, ignore-file policy, or overlay runtime details.
"""

from __future__ import annotations

import hashlib
import os
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Literal, Protocol

from sandbox.layer_stack._paths import join_layer_path, remove_path
from sandbox.layer_stack.layer_index import OPAQUE_MARKER, WHITEOUT_PREFIX


LayerChangeKind = Literal["write", "delete", "symlink", "opaque_dir"]


class DigestSink(Protocol):
    def update(self, data: bytes, /) -> object: ...


def normalize_layer_path(path: str, *, allow_root: bool = False) -> str:
    raw = str(path).replace("\\", "/").strip()
    candidate = PurePosixPath(raw)
    if candidate.is_absolute():
        raise ValueError(f"path must be relative: {path}")

    parts = tuple(part for part in candidate.parts if part not in ("", "."))
    if not parts:
        if allow_root:
            return ""
        raise ValueError("path must not be empty")
    if any(part == ".." for part in parts):
        raise ValueError(f"path must stay inside the layer stack: {path}")
    return "/".join(parts)


@dataclass(frozen=True)
class LayerChange:
    """Tagged-union storage-level layer change."""

    kind: LayerChangeKind
    path: str
    source_path: str | None = None
    content_hash: str | None = None

    def __post_init__(self) -> None:
        object.__setattr__(self, "path", normalize_layer_path(self.path))
        if self.kind == "write":
            if not self.source_path:
                raise ValueError("write changes require source_path")
        elif self.kind == "symlink":
            if not self.source_path:
                raise ValueError("symlink changes require source_path")
            if self.content_hash is not None:
                raise ValueError("symlink changes must not carry content_hash")
        elif self.kind in ("delete", "opaque_dir"):
            if self.source_path is not None:
                raise ValueError(f"{self.kind} changes must not carry source_path")
            if self.content_hash is not None:
                raise ValueError(f"{self.kind} changes must not carry content_hash")
        else:
            raise ValueError(f"unsupported layer change kind: {self.kind}")


@dataclass(frozen=True)
class PreparedLayerChange:
    change: LayerChange
    write_content: bytes | None = None


def WriteLayerChange(
    *,
    path: str,
    source_path: str | None = None,
    content_hash: str | None = None,
) -> LayerChange:
    return LayerChange(
        kind="write", path=path, source_path=source_path, content_hash=content_hash
    )


def DeleteLayerChange(
    *,
    path: str,
    source_path: str | None = None,
    content_hash: str | None = None,
) -> LayerChange:
    return LayerChange(
        kind="delete", path=path, source_path=source_path, content_hash=content_hash
    )


def SymlinkLayerChange(
    *,
    path: str,
    source_path: str | None = None,
    content_hash: str | None = None,
) -> LayerChange:
    return LayerChange(
        kind="symlink", path=path, source_path=source_path, content_hash=content_hash
    )


def OpaqueDirLayerChange(
    *,
    path: str,
    source_path: str | None = None,
    content_hash: str | None = None,
) -> LayerChange:
    return LayerChange(
        kind="opaque_dir", path=path, source_path=source_path, content_hash=content_hash
    )


def prepare_layer_change(
    change: LayerChange,
    *,
    source_root: Path | None = None,
) -> PreparedLayerChange:
    if change.kind != "write":
        return PreparedLayerChange(change=change)
    assert change.source_path is not None
    source_path = Path(change.source_path)
    if source_root is not None:
        resolved = source_path.resolve(strict=True)
        if not resolved.is_relative_to(source_root):
            raise ValueError(
                f"write source path is outside trusted source root: {change.path}"
            )
        source_path = resolved
    content = source_path.read_bytes()
    digest = hashlib.sha256(content).hexdigest()
    if change.content_hash and digest != change.content_hash:
        raise ValueError(f"content hash mismatch for {change.path}")
    return PreparedLayerChange(change=change, write_content=content)


def write_layer_change(prepared: PreparedLayerChange, layer_dir: Path) -> None:
    c = prepared.change
    if c.kind == "write":
        assert prepared.write_content is not None
        target = join_layer_path(layer_dir, c.path)
        target.parent.mkdir(parents=True, exist_ok=True)
        remove_path(target)
        target.write_bytes(prepared.write_content)
    elif c.kind == "delete":
        _whiteout_path(layer_dir, c.path).write_text("", encoding="utf-8")
    elif c.kind == "symlink":
        assert c.source_path is not None
        target = join_layer_path(layer_dir, c.path)
        target.parent.mkdir(parents=True, exist_ok=True)
        remove_path(target)
        os.symlink(c.source_path, target)
    elif c.kind == "opaque_dir":
        marker = join_layer_path(layer_dir, c.path) / OPAQUE_MARKER
        marker.parent.mkdir(parents=True, exist_ok=True)
        marker.write_text("", encoding="utf-8")


def update_digest(digest: DigestSink, prepared: PreparedLayerChange) -> None:
    c = prepared.change
    digest.update(c.kind.encode("utf-8"))
    digest.update(b"\0")
    digest.update(c.path.encode("utf-8"))
    digest.update(b"\0")
    if c.kind == "write":
        assert prepared.write_content is not None
        digest.update(prepared.write_content)
    elif c.kind == "symlink":
        assert c.source_path is not None
        digest.update(c.source_path.encode("utf-8"))
    digest.update(b"\0")


@dataclass(frozen=True)
class LayerDelta:
    changes: tuple[LayerChange, ...]

    def __post_init__(self) -> None:
        object.__setattr__(self, "changes", tuple(self.changes))


def aggregate_layer_changes(changes: Iterable[LayerChange]) -> LayerDelta:
    """Collapse accepted same-path changes into a deterministic layer delta."""
    final_by_path: dict[str, LayerChange] = {}
    for change in changes:
        final_by_path[change.path] = change
    return LayerDelta(
        changes=tuple(final_by_path[path] for path in sorted(final_by_path))
    )


def _whiteout_path(layer_dir: Path, rel: str) -> Path:
    target = PurePosixPath(rel)
    parent_parts = tuple(part for part in target.parent.parts if part != ".")
    whiteout = layer_dir.joinpath(*parent_parts, f"{WHITEOUT_PREFIX}{target.name}")
    whiteout.parent.mkdir(parents=True, exist_ok=True)
    return whiteout


__all__ = [
    "DeleteLayerChange",
    "DigestSink",
    "LayerChange",
    "LayerChangeKind",
    "LayerDelta",
    "OpaqueDirLayerChange",
    "PreparedLayerChange",
    "SymlinkLayerChange",
    "WriteLayerChange",
    "aggregate_layer_changes",
    "normalize_layer_path",
    "prepare_layer_change",
    "update_digest",
    "write_layer_change",
]
