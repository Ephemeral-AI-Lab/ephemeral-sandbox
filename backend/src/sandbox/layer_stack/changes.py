"""Storage-level layer change objects.

These values describe already-accepted filesystem mutations. They deliberately
do not encode OCC policy, ignore-file policy, or overlay runtime details.
"""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import PurePosixPath
from typing import Literal


LayerChangeKind = Literal["write", "delete", "symlink", "opaque_dir"]


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
    path: str
    kind: LayerChangeKind
    content_hash: str | None = None
    source_path: str | None = None

    def __post_init__(self) -> None:
        object.__setattr__(self, "path", normalize_layer_path(self.path))
        if self.kind in ("write", "symlink"):
            if not self.source_path:
                raise ValueError(f"{self.kind} changes require source_path")
            return
        if self.source_path is not None:
            raise ValueError(f"{self.kind} changes must not carry source_path")
        if self.content_hash is not None:
            raise ValueError(f"{self.kind} changes must not carry content_hash")


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
