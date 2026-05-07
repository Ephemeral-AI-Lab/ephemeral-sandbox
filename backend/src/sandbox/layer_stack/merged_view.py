"""Newest-first merged reads for layer-stack manifests."""

from __future__ import annotations

import errno
import os
import shutil
from pathlib import Path, PurePosixPath

from sandbox.layer_stack.changes import normalize_layer_path
from sandbox.layer_stack.layer_index import (
    OPAQUE_MARKER,
    WHITEOUT_PREFIX,
    LayerIndex,
    build_layer_index,
    has_ancestor_in,
)
from sandbox.layer_stack.manifest import LayerRef, Manifest


__all__ = ["LayerStackStorageError", "MergedView", "OPAQUE_MARKER", "WHITEOUT_PREFIX"]


class LayerStackStorageError(RuntimeError):
    """Raised when a manifest references missing or invalid layer storage."""


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

        Called by ``LayerStackManager`` after a layer dir is removed; without
        this the cache grows unboundedly on long-running daemons.
        """
        self._layer_index_cache.pop(layer_id, None)

    def read_bytes(self, path: str, manifest: Manifest) -> tuple[bytes | None, bool]:
        rel = normalize_layer_path(path)
        for layer in manifest.layers:
            index = self._layer_index(layer)
            if rel in index.whiteouts:
                return None, False
            if rel in index.files:
                layer_dir = self._layer_dir(layer)
                candidate = _join_rel(layer_dir, rel)
                if candidate.is_symlink():
                    return os.readlink(candidate).encode("utf-8"), True
                if candidate.is_file():
                    return candidate.read_bytes(), True
            if has_ancestor_in(rel, index.files):
                return None, False
            if has_ancestor_in(rel, index.opaque_dirs):
                return None, False
        return None, False

    def read_text(self, path: str, manifest: Manifest) -> tuple[str, bool]:
        content, exists = self.read_bytes(path, manifest)
        if not exists:
            return "", False
        if content is None:
            return "", True
        return content.decode("utf-8"), True

    def read_symlink(self, path: str, manifest: Manifest) -> tuple[str, bool]:
        rel = normalize_layer_path(path)
        for layer in manifest.layers:
            index = self._layer_index(layer)
            if rel in index.whiteouts:
                return "", False
            if rel in index.files:
                layer_dir = self._layer_dir(layer)
                candidate = _join_rel(layer_dir, rel)
                if candidate.is_symlink():
                    return os.readlink(candidate), True
                # rel resolves to a regular file (not a symlink) in this
                # layer — same answer as the old `candidate.exists()`
                # branch: the path exists but is not a symlink.
                return "", False
            if has_ancestor_in(rel, index.files):
                return "", False
            if has_ancestor_in(rel, index.opaque_dirs):
                return "", False
        return "", False

    def list_dir(self, path: str, manifest: Manifest) -> tuple[str, ...]:
        rel = normalize_layer_path(path, allow_root=True)
        names: set[str] = set()
        hidden: set[str] = set()
        prefix = f"{rel}/" if rel else ""

        for layer in manifest.layers:
            index = self._layer_index(layer)

            # rel itself is a regular file/symlink in this layer, or any
            # of its strict ancestors is. Either way, rel is not a
            # directory in any layer at or below this one.
            if rel and rel in index.files:
                return tuple(sorted(names))
            if rel and has_ancestor_in(rel, index.files):
                return tuple(sorted(names))
            # An opaque marker on a strict ancestor means rel can't
            # exist in any older layer either (matching read_bytes /
            # read_symlink semantics).
            if rel and has_ancestor_in(rel, index.opaque_dirs):
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

            # rel itself is whited out in this layer. If this same layer
            # has no children of rel, the directory is gone — stop.
            # If it does have children, the layer effectively re-creates
            # rel as a directory; keep iterating older layers BUT they
            # cannot show through (case D below catches the opaque-on-rel
            # path; otherwise the whiteout-only re-creation never happens
            # in practice — overlayfs writes `.wh.<rel>` only when rel
            # is being deleted).
            if rel and rel in index.whiteouts:
                has_children_here = (
                    any(name.startswith(prefix) for name in index.files)
                    or any(name.startswith(prefix) for name in index.whiteouts)
                    or any(
                        opaque == rel or opaque.startswith(prefix)
                        for opaque in index.opaque_dirs
                    )
                )
                if not has_children_here:
                    return tuple(sorted(names))

            # rel itself is opaque in this layer → stop after collecting
            # this layer's children; older layers can't contribute.
            if rel in index.opaque_dirs:
                return tuple(sorted(names))

        return tuple(sorted(names))

    def materialize(
        self,
        destination: str | Path,
        manifest: Manifest,
        *,
        link_ok: bool = False,
    ) -> None:
        """Materialise *manifest* into *destination*.

        ``link_ok=True`` hardlinks regular files from source layers. Only safe
        when the caller treats *destination* as read-only (e.g. the overlay
        lowerdir from :meth:`LayerStackManager.prepare_workspace_snapshot`);
        a writer would corrupt the source layer through the shared inode.
        """
        dest = Path(destination)
        if dest.exists():
            shutil.rmtree(dest)
        dest.mkdir(parents=True)

        for layer in reversed(manifest.layers):
            self._apply_layer(self._layer_dir(layer), dest, link_ok=link_ok)

    def _layer_dir(self, layer: LayerRef) -> Path:
        layer_path = Path(layer.path)
        if not layer_path.is_absolute():
            layer_path = self._storage_root / layer_path
        if not layer_path.is_dir():
            raise LayerStackStorageError(
                f"manifest references missing layer {layer.layer_id}: {layer.path}"
            )
        return layer_path

    def _apply_layer(
        self,
        layer_dir: Path,
        dest: Path,
        *,
        link_ok: bool = False,
    ) -> None:
        entries = tuple(sorted(layer_dir.rglob("*"), key=lambda item: item.as_posix()))

        for marker in entries:
            if marker.name != OPAQUE_MARKER:
                continue
            target = dest / marker.parent.relative_to(layer_dir)
            _clear_directory(target)

        for whiteout in entries:
            if not _is_whiteout(whiteout.name):
                continue
            rel = whiteout.relative_to(layer_dir)
            target = dest / rel.parent / whiteout.name[len(WHITEOUT_PREFIX) :]
            _remove_path(target)

        for entry in entries:
            if entry.name == OPAQUE_MARKER or _is_whiteout(entry.name):
                continue
            rel = entry.relative_to(layer_dir)
            target = dest / rel
            if entry.is_symlink():
                _replace_symlink(target, os.readlink(entry))
            elif entry.is_dir():
                target.mkdir(parents=True, exist_ok=True)
            elif entry.is_file():
                target.parent.mkdir(parents=True, exist_ok=True)
                _remove_path(target)
                if link_ok:
                    _link_or_copy(entry, target)
                else:
                    shutil.copy2(entry, target)


def _join_rel(root: Path, rel: str) -> Path:
    if not rel:
        return root
    return root.joinpath(*PurePosixPath(rel).parts)


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


def _is_whiteout(name: str) -> bool:
    return name.startswith(WHITEOUT_PREFIX) and name != OPAQUE_MARKER


def _clear_directory(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    for child in path.iterdir():
        _remove_path(child)


def _remove_path(path: Path) -> None:
    if path.is_symlink() or path.is_file():
        path.unlink(missing_ok=True)
    elif path.is_dir():
        shutil.rmtree(path)


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
    _remove_path(path)
    os.symlink(target, path)
