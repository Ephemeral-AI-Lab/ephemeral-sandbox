"""Storage-level layer change objects.

These values describe already-accepted filesystem mutations. They deliberately
do not encode OCC policy, ignore-file policy, or overlay runtime details.
"""

from __future__ import annotations

import os
from abc import ABC, abstractmethod
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Literal, Protocol

from sandbox.layer_stack._paths import join_layer_path, remove_path
from sandbox.layer_stack.layer.index import OPAQUE_MARKER, WHITEOUT_PREFIX


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


class LayerChange(ABC):
    """Base contract for explicit layer-change variants."""

    path: str
    kind: LayerChangeKind
    content_hash: str | None = None
    source_path: str | None = None

    def prepare(
        self,
        *,
        source_root: Path | None = None,
    ) -> PreparedLayerChange:
        del source_root
        return PreparedLayerChange(change=self)

    def update_digest(self, digest: DigestSink, prepared: PreparedLayerChange) -> None:
        if prepared.change is not self:
            raise ValueError("prepared layer change does not match change")
        digest.update(self.kind.encode("utf-8"))
        digest.update(b"\0")
        digest.update(self.path.encode("utf-8"))
        digest.update(b"\0")
        self._update_digest_payload(digest, prepared)
        digest.update(b"\0")

    @abstractmethod
    def _update_digest_payload(
        self,
        digest: DigestSink,
        prepared: PreparedLayerChange,
    ) -> None:
        """Add variant-specific payload to the publish digest."""

    @abstractmethod
    def write_to(self, layer_dir: Path, prepared: PreparedLayerChange) -> None:
        """Write this prepared change into an immutable layer directory."""


@dataclass(frozen=True)
class PreparedLayerChange:
    change: LayerChange
    write_content: bytes | None = None


@dataclass(frozen=True)
class WriteLayerChange(LayerChange):
    path: str
    source_path: str
    content_hash: str | None = None
    kind: Literal["write"] = "write"

    def __post_init__(self) -> None:
        if self.kind != "write":
            raise ValueError(f"unsupported write layer change kind: {self.kind}")
        if not self.source_path:
            raise ValueError("write changes require source_path")
        object.__setattr__(self, "path", normalize_layer_path(self.path))

    def prepare(
        self,
        *,
        source_root: Path | None = None,
    ) -> PreparedLayerChange:
        source_path = Path(self.source_path)
        if source_root is not None:
            resolved_source = source_path.resolve(strict=True)
            if not resolved_source.is_relative_to(source_root):
                raise ValueError(
                    "write source path is outside trusted source root: "
                    f"{self.path}"
                )
            source_path = resolved_source
        content = source_path.read_bytes()
        content_hash = _sha256_hex(content)
        if self.content_hash and content_hash != self.content_hash:
            raise ValueError(f"content hash mismatch for {self.path}")
        return PreparedLayerChange(change=self, write_content=content)

    def _update_digest_payload(
        self,
        digest: DigestSink,
        prepared: PreparedLayerChange,
    ) -> None:
        if prepared.write_content is None:
            raise ValueError(f"prepared write content missing for {self.path}")
        digest.update(prepared.write_content)

    def write_to(self, layer_dir: Path, prepared: PreparedLayerChange) -> None:
        if prepared.write_content is None:
            raise ValueError(f"prepared write content missing for {self.path}")
        target = join_layer_path(layer_dir, self.path)
        target.parent.mkdir(parents=True, exist_ok=True)
        remove_path(target)
        target.write_bytes(prepared.write_content)


@dataclass(frozen=True)
class DeleteLayerChange(LayerChange):
    path: str
    kind: Literal["delete"] = "delete"
    source_path: None = None
    content_hash: None = None

    def __post_init__(self) -> None:
        if self.kind != "delete":
            raise ValueError(f"unsupported delete layer change kind: {self.kind}")
        if self.source_path is not None:
            raise ValueError("delete changes must not carry source_path")
        if self.content_hash is not None:
            raise ValueError("delete changes must not carry content_hash")
        object.__setattr__(self, "path", normalize_layer_path(self.path))

    def _update_digest_payload(
        self,
        digest: DigestSink,
        prepared: PreparedLayerChange,
    ) -> None:
        del digest, prepared

    def write_to(self, layer_dir: Path, prepared: PreparedLayerChange) -> None:
        del prepared
        _whiteout_path(layer_dir, self.path).write_text("", encoding="utf-8")


@dataclass(frozen=True)
class SymlinkLayerChange(LayerChange):
    path: str
    source_path: str
    kind: Literal["symlink"] = "symlink"
    content_hash: None = None

    def __post_init__(self) -> None:
        if self.kind != "symlink":
            raise ValueError(f"unsupported symlink layer change kind: {self.kind}")
        if not self.source_path:
            raise ValueError("symlink changes require source_path")
        if self.content_hash is not None:
            raise ValueError("symlink changes must not carry content_hash")
        object.__setattr__(self, "path", normalize_layer_path(self.path))

    def _update_digest_payload(
        self,
        digest: DigestSink,
        prepared: PreparedLayerChange,
    ) -> None:
        del prepared
        digest.update(self.source_path.encode("utf-8"))

    def write_to(self, layer_dir: Path, prepared: PreparedLayerChange) -> None:
        del prepared
        target = join_layer_path(layer_dir, self.path)
        target.parent.mkdir(parents=True, exist_ok=True)
        remove_path(target)
        os.symlink(self.source_path, target)


@dataclass(frozen=True)
class OpaqueDirLayerChange(LayerChange):
    path: str
    kind: Literal["opaque_dir"] = "opaque_dir"
    source_path: None = None
    content_hash: None = None

    def __post_init__(self) -> None:
        if self.kind != "opaque_dir":
            raise ValueError(f"unsupported opaque-dir layer change kind: {self.kind}")
        if self.source_path is not None:
            raise ValueError("opaque_dir changes must not carry source_path")
        if self.content_hash is not None:
            raise ValueError("opaque_dir changes must not carry content_hash")
        object.__setattr__(self, "path", normalize_layer_path(self.path))

    def _update_digest_payload(
        self,
        digest: DigestSink,
        prepared: PreparedLayerChange,
    ) -> None:
        del digest, prepared

    def write_to(self, layer_dir: Path, prepared: PreparedLayerChange) -> None:
        del prepared
        marker = join_layer_path(layer_dir, self.path) / OPAQUE_MARKER
        marker.parent.mkdir(parents=True, exist_ok=True)
        marker.write_text("", encoding="utf-8")


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
    return LayerDelta(changes=tuple(final_by_path[path] for path in sorted(final_by_path)))


def make_layer_change(
    *,
    path: str,
    kind: LayerChangeKind,
    content_hash: str | None = None,
    source_path: str | None = None,
) -> LayerChange:
    """Compatibility factory for callers that still parse kind strings."""
    if kind == "write":
        if source_path is None:
            raise ValueError("write changes require source_path")
        return WriteLayerChange(
            path=path,
            source_path=source_path,
            content_hash=content_hash,
        )
    if kind == "delete":
        return DeleteLayerChange(
            path=path,
            content_hash=content_hash,
            source_path=source_path,
        )
    if kind == "symlink":
        if source_path is None:
            raise ValueError("symlink changes require source_path")
        return SymlinkLayerChange(
            path=path,
            source_path=source_path,
            content_hash=content_hash,
        )
    if kind == "opaque_dir":
        return OpaqueDirLayerChange(
            path=path,
            content_hash=content_hash,
            source_path=source_path,
        )
    raise ValueError(f"unsupported layer change kind: {kind}")


def _sha256_hex(content: bytes) -> str:
    import hashlib

    return hashlib.sha256(content).hexdigest()


def _whiteout_path(layer_dir: Path, rel: str) -> Path:
    target = PurePosixPath(rel)
    parent_parts = tuple(part for part in target.parent.parts if part != ".")
    whiteout = layer_dir.joinpath(*parent_parts, f"{WHITEOUT_PREFIX}{target.name}")
    whiteout.parent.mkdir(parents=True, exist_ok=True)
    return whiteout
