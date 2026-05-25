"""Newest-first merged reads for layer-stack manifests."""

from __future__ import annotations

import errno
import os
import shutil
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

from sandbox.layer_stack.paths import join_layer_path, remove_path
from sandbox.layer_stack.changes import normalize_layer_path
from sandbox.layer_stack.layer_index import (
    OPAQUE_MARKER,
    WHITEOUT_PREFIX,
    LayerIndex,
    build_layer_index,
    has_ancestor_in,
)
from sandbox.layer_stack.manifest import LayerRef, Manifest


SymlinkLookup = Literal["symlink", "file", "absent"]


class LayerStackStorageError(RuntimeError):
    """Raised when a manifest references missing or invalid layer storage."""

    def __init__(self, message: str, *, layer_id: str | None = None) -> None:
        super().__init__(message)
        self.layer_id = layer_id


@dataclass(frozen=True)
class _VisibleLayerEntry:
    layer: LayerRef
    path: Path


__all__ = ["LayerStackStorageError", "MergedView", "SymlinkLookup"]


class MergedView:
    """Reads paths through a frozen manifest without mutating layer state."""

    def __init__(self, storage_root: str | Path) -> None:
        self._storage_root = Path(storage_root)
        self._layer_index_cache: dict[str, LayerIndex] = {}

    def _layer_index(self, layer: LayerRef) -> LayerIndex:
        cached = self._layer_index_cache.get(layer.layer_id)
        if cached is not None:
            return cached
        index = build_layer_index(self._layer_dir(layer))
        return self._layer_index_cache.setdefault(layer.layer_id, index)

    def evict_layer_index(self, layer_id: str) -> None:
        """Drop the cached presence index for ``layer_id``.

        Called by ``LayerStack`` after a layer dir is removed; without
        this the cache grows unboundedly on long-running daemons.
        """
        self._layer_index_cache.pop(layer_id, None)

    def read_bytes(self, path: str, manifest: Manifest) -> tuple[bytes | None, bool]:
        rel = normalize_layer_path(path)
        entry = self._visible_entry(rel, manifest)
        if entry is None:
            return None, False
        try:
            if entry.path.is_symlink():
                return os.readlink(entry.path).encode("utf-8"), True
            if entry.path.is_file():
                return entry.path.read_bytes(), True
        except OSError as exc:
            raise _stale_layer_error(entry.layer, rel) from exc
        raise _stale_layer_error(entry.layer, rel)

    def read_text(self, path: str, manifest: Manifest) -> tuple[str, bool]:
        content, exists = self.read_bytes(path, manifest)
        if not exists:
            return "", False
        assert content is not None
        return content.decode("utf-8"), True

    def read_symlink(self, path: str, manifest: Manifest) -> tuple[str, SymlinkLookup]:
        rel = normalize_layer_path(path)
        entry = self._visible_entry(rel, manifest)
        if entry is None:
            return "", "absent"
        try:
            if entry.path.is_symlink():
                return os.readlink(entry.path), "symlink"
            if entry.path.exists():
                return "", "file"
        except OSError as exc:
            raise _stale_layer_error(entry.layer, rel) from exc
        raise _stale_layer_error(entry.layer, rel)

    def _visible_entry(
        self,
        rel: str,
        manifest: Manifest,
    ) -> _VisibleLayerEntry | None:
        for layer in manifest.layers:
            index = self._layer_index(layer)
            if rel in index.whiteouts:
                return None
            if rel in index.files:
                return _VisibleLayerEntry(
                    layer=layer,
                    path=join_layer_path(self._layer_dir(layer), rel),
                )
            if _lookup_blocked_by_layer(rel, index):
                return None
        return None

    def list_dir(self, path: str, manifest: Manifest) -> tuple[str, ...]:
        rel = normalize_layer_path(path, allow_root=True)
        names: set[str] = set()
        hidden: set[str] = set()
        prefix = f"{rel}/" if rel else ""

        for layer in manifest.layers:
            index = self._layer_index(layer)

            # A file at rel, a file ancestor, or an opaque ancestor stops
            # directory lookup at this layer.
            if rel and (rel in index.files or _lookup_blocked_by_layer(rel, index)):
                return tuple(sorted(names))

            # Direct-child whiteouts at this level mask same-name
            # children in older layers. Whiteouts deeper than rel/<name>
            # are not relevant here — they affect list_dir(rel/<name>).
            for whiteout in index.whiteouts:
                child = _direct_child_segment(whiteout, prefix)
                if child is not None:
                    hidden.add(child)

            # Direct-child files contribute their first segment.
            for file_path in index.files:
                child = _direct_child_segment(file_path, prefix)
                if child is not None and child not in hidden:
                    names.add(child)

            # Direct-child opaque-dir markers ALSO imply a directory
            # child at this level.
            for opaque in index.opaque_dirs:
                child = _direct_child_segment(opaque, prefix)
                if child is not None and child not in hidden:
                    names.add(child)

            # rel itself is opaque in this layer → stop after collecting
            # this layer's children; older layers can't contribute. A
            # plain whiteout on rel (without an opaque marker) cannot
            # appear with same-layer children produced by this module's
            # publisher; the case isn't represented here.
            if rel in index.opaque_dirs:
                return tuple(sorted(names))

        return tuple(sorted(names))

    def iter_paths(self, manifest: Manifest) -> Iterator[str]:
        """Yield every visible workspace-relative file path in the manifest.

        Walks layers newest-first (matching ``read_bytes`` semantics).
        For each path, newer layers shadow older entries; whiteouts and
        opaque-dir markers in newer layers mask matching files in older
        layers. Symlinks are listed as paths (not followed).

        Output is sorted alphabetically for deterministic test order.
        """
        visible: set[str] = set()
        whiteouts_seen: set[str] = set()
        opaque_dirs_seen: set[str] = set()

        for layer in manifest.layers:
            index = self._layer_index(layer)
            for path in index.files:
                if path in visible:
                    continue
                if path in whiteouts_seen:
                    continue
                if has_ancestor_in(path, whiteouts_seen):
                    continue
                if has_ancestor_in(path, opaque_dirs_seen):
                    continue
                visible.add(path)
            whiteouts_seen.update(index.whiteouts)
            opaque_dirs_seen.update(index.opaque_dirs)

        yield from sorted(visible)

    def materialize(
        self,
        destination: str | Path,
        manifest: Manifest,
        *,
        share_inodes: bool = False,
    ) -> None:
        """Materialise *manifest* into *destination*.

        ``share_inodes=True`` hardlinks regular files from source layers. Only safe
        when the caller treats *destination* as read-only (e.g. a layer-stack
        projection);
        a writer would corrupt the source layer through the shared inode.
        """
        dest = Path(destination)
        if dest.exists():
            shutil.rmtree(dest)
        dest.mkdir(parents=True)

        for layer in reversed(manifest.layers):
            self._apply_layer(
                self._layer_dir(layer),
                dest,
                share_inodes=share_inodes,
            )

    def _layer_dir(self, layer: LayerRef) -> Path:
        layer_path = Path(layer.path)
        if not layer_path.is_absolute():
            layer_path = self._storage_root / layer_path
        if not layer_path.is_dir():
            raise LayerStackStorageError(
                f"manifest references missing layer {layer.layer_id}: {layer.path}",
                layer_id=layer.layer_id,
            )
        return layer_path

    def _apply_layer(
        self,
        layer_dir: Path,
        dest: Path,
        *,
        share_inodes: bool = False,
    ) -> None:
        opaques: list[Path] = []
        whiteouts: list[Path] = []
        regulars: list[Path] = []
        for entry in sorted(layer_dir.rglob("*"), key=lambda item: item.as_posix()):
            if entry.name == OPAQUE_MARKER:
                opaques.append(entry)
            elif _is_whiteout(entry.name):
                whiteouts.append(entry)
            else:
                regulars.append(entry)

        for marker in opaques:
            _clear_directory(dest / marker.parent.relative_to(layer_dir))

        for whiteout in whiteouts:
            rel = whiteout.relative_to(layer_dir)
            remove_path(dest / rel.parent / whiteout.name[len(WHITEOUT_PREFIX) :])

        for entry in regulars:
            target = dest / entry.relative_to(layer_dir)
            if entry.is_symlink():
                _replace_symlink(target, os.readlink(entry))
            elif entry.is_dir():
                target.mkdir(parents=True, exist_ok=True)
            elif entry.is_file():
                target.parent.mkdir(parents=True, exist_ok=True)
                remove_path(target)
                if share_inodes:
                    _link_or_copy(entry, target)
                else:
                    shutil.copy2(entry, target)


def _direct_child_segment(name: str, prefix: str) -> str | None:
    """Return the first path segment of ``name`` under ``prefix``, or None.

    ``prefix`` is ``""`` for the root directory or ``"<dir>/"`` for any
    other directory. ``name`` is a layer-index path (e.g. an entry of
    ``index.files`` / ``index.whiteouts`` / ``index.opaque_dirs``).
    Returns the direct-child segment if ``name`` is a strict descendant
    of the directory; otherwise ``None``.
    """
    if prefix:
        if not name.startswith(prefix):
            return None
        rest = name[len(prefix) :]
    else:
        rest = name
    if not rest:
        return None
    head, _, _ = rest.partition("/")
    return head or None


def _lookup_blocked_by_layer(rel: str, index: LayerIndex) -> bool:
    return has_ancestor_in(rel, index.files) or has_ancestor_in(rel, index.opaque_dirs)


def _is_whiteout(name: str) -> bool:
    return name.startswith(WHITEOUT_PREFIX) and name != OPAQUE_MARKER


def _stale_layer_error(layer: LayerRef, rel: str) -> LayerStackStorageError:
    return LayerStackStorageError(
        f"layer no longer present while reading {rel}: {layer.layer_id}",
        layer_id=layer.layer_id,
    )


def _clear_directory(path: Path) -> None:
    # If an upper layer converts a file/symlink path into an opaque dir,
    # the merged view materializer hits this with `path` already pointing
    # at the previous-layer file or symlink. `mkdir(exist_ok=True)` would
    # raise FileExistsError in that case (a legitimate transition, not an
    # error). Remove the non-directory entry first so the opaque-dir apply
    # can proceed.
    if path.is_symlink() or (path.exists() and not path.is_dir()):
        remove_path(path)
    path.mkdir(parents=True, exist_ok=True)
    for child in path.iterdir():
        remove_path(child)


def _link_or_copy(src: Path, dst: Path) -> None:
    """Hardlink ``src`` into ``dst``; copy on EXDEV (cross-FS) or EPERM."""
    try:
        os.link(src, dst)
    except OSError as exc:
        if exc.errno not in (errno.EXDEV, errno.EPERM):
            raise
        shutil.copy2(src, dst)


def _replace_symlink(path: Path, target: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    remove_path(path)
    os.symlink(target, path)
