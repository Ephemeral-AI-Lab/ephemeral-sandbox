"""Workspace base construction for an empty layer stack."""

from __future__ import annotations

import hashlib
import os
import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Literal, TypeAlias

from sandbox.layer_stack.paths import fsync_path
from sandbox.layer_stack.manifest import (
    LAYERS_DIR,
    STAGING_DIR,
    LayerRef,
    Manifest,
    manifest_path,
    read_manifest,
    write_layer_digest_atomic,
    write_manifest_atomic,
)
from sandbox.layer_stack.workspace_binding import (
    WorkspaceBinding,
    WorkspaceBindingError,
    read_workspace_binding,
    validate_workspace_binding_paths,
    write_workspace_binding_atomic,
)
from sandbox.shared.clock import monotonic_now, record_elapsed

WORKSPACE_BASE_LAYER_ID = "B000001-base"


class WorkspaceBaseAlreadyExistsError(RuntimeError):
    """Raised when a workspace base is requested for non-empty stack state."""


class WorkspaceBaseIncompleteError(RuntimeError):
    """Raised when a full workspace base cannot represent every workspace path."""

    def __init__(
        self,
        *,
        special_file_rejections: tuple[str, ...],
        unstable_paths: tuple[str, ...],
    ) -> None:
        self.special_file_rejections = special_file_rejections
        self.unstable_paths = unstable_paths
        super().__init__(
            "workspace base must be a full copy; "
            f"special={len(special_file_rejections)}, "
            f"unstable={len(unstable_paths)}"
        )


@dataclass(frozen=True)
class _DirectoryEntry:
    path: str
    kind: Literal["directory"] = "directory"


@dataclass(frozen=True)
class _FileEntry:
    path: str
    source_path: Path
    size: int
    content_hash: str
    kind: Literal["file"] = "file"


@dataclass(frozen=True)
class _SymlinkEntry:
    path: str
    link_target: str
    kind: Literal["symlink"] = "symlink"


_BaseEntry: TypeAlias = _DirectoryEntry | _FileEntry | _SymlinkEntry


def build_workspace_base(
    *,
    workspace_root: str | Path,
    layer_stack_root: str | Path,
    reset: bool = False,
    timings: dict[str, float] | None = None,
) -> WorkspaceBinding:
    """Build *workspace_root* as manifest version 1.

    The base build is a full workspace copy. It either represents every regular
    file and symlink from the assigned workspace or fails before publishing
    workspace truth.
    """
    workspace = Path(workspace_root)
    stack = Path(layer_stack_root)
    validate_workspace_binding_paths(workspace_root=workspace, layer_stack_root=stack)
    if not workspace.is_dir():
        raise WorkspaceBindingError(f"workspace_root does not exist: {workspace}")

    if reset:
        shutil.rmtree(stack, ignore_errors=True)
    prepare_start = monotonic_now()
    _prepare_empty_stack(stack)
    _reject_existing_base_state(stack)
    record_elapsed(timings, "workspace_base.prepare_stack_s", prepare_start)

    collect_start = monotonic_now()
    entries, root_hash = _collect_base_entries(workspace)
    record_elapsed(timings, "workspace_base.collect_s", collect_start)
    if timings is not None:
        files = sum(1 for e in entries if isinstance(e, _FileEntry))
        dirs = sum(1 for e in entries if isinstance(e, _DirectoryEntry))
        symlinks = sum(1 for e in entries if isinstance(e, _SymlinkEntry))
        bytes_total = sum(e.size for e in entries if isinstance(e, _FileEntry))
        timings["workspace_base.inventory.files"] = float(files)
        timings["workspace_base.inventory.dirs"] = float(dirs)
        timings["workspace_base.inventory.symlinks"] = float(symlinks)
        timings["workspace_base.inventory.bytes"] = float(bytes_total)
    write_layer_start = monotonic_now()
    # _write_base_layer's per-file content_hash recheck already catches
    # mid-flight file edits at write time; no second full-tree rescan.
    layer_ref = _write_base_layer(stack, entries)
    write_layer_digest_atomic(stack, layer_ref.layer_id, root_hash)
    record_elapsed(timings, "workspace_base.write_layer_s", write_layer_start)
    manifest = Manifest(version=1, layers=(layer_ref,))
    write_manifest_start = monotonic_now()
    write_manifest_atomic(manifest_path(stack), manifest)
    record_elapsed(timings, "workspace_base.write_manifest_s", write_manifest_start)
    binding = WorkspaceBinding(
        workspace_root=workspace.as_posix(),
        layer_stack_root=stack.as_posix(),
        active_manifest_version=manifest.version,
        active_root_hash=root_hash,
        base_manifest_version=manifest.version,
        base_root_hash=root_hash,
    )
    write_binding_start = monotonic_now()
    write_workspace_binding_atomic(binding)
    record_elapsed(timings, "workspace_base.write_binding_s", write_binding_start)
    return binding


def _prepare_empty_stack(stack: Path) -> None:
    stack.mkdir(parents=True, exist_ok=True)
    (stack / LAYERS_DIR).mkdir(exist_ok=True)
    (stack / STAGING_DIR).mkdir(exist_ok=True)


def _reject_existing_base_state(stack: Path) -> None:
    binding = read_workspace_binding(stack)
    if binding is not None:
        raise WorkspaceBaseAlreadyExistsError(f"workspace base already exists at {stack}")
    active = read_manifest(manifest_path(stack))
    if active.version != 0 or active.layers:
        raise WorkspaceBaseAlreadyExistsError(
            f"layer stack is not empty: manifest version {active.version}"
        )
    layers = stack / LAYERS_DIR
    staging = stack / STAGING_DIR
    if any(layers.iterdir()) or any(staging.iterdir()):
        raise WorkspaceBaseAlreadyExistsError(
            f"layer stack has existing layer or staging state: {stack}"
        )


def _collect_base_entries(workspace: Path) -> tuple[tuple[_BaseEntry, ...], str]:
    special: list[str] = []
    unstable: list[str] = []
    entries: list[_BaseEntry] = []
    digest = hashlib.sha256()

    for current_root, dirnames, filenames in os.walk(workspace, topdown=True, followlinks=False):
        current = Path(current_root)
        dirnames.sort()
        filenames.sort()

        kept_dirs: list[str] = []
        for dirname in dirnames:
            path = current / dirname
            rel = _relative(workspace, path)
            if path.is_symlink():
                entries.append(_symlink_entry(path=path, rel=rel))
                continue
            entries.append(_DirectoryEntry(path=rel))
            kept_dirs.append(dirname)
        dirnames[:] = kept_dirs

        for filename in filenames:
            path = current / filename
            rel = _relative(workspace, path)
            if path.is_symlink():
                entries.append(_symlink_entry(path=path, rel=rel))
                continue
            try:
                stat = path.lstat()
            except FileNotFoundError:
                unstable.append(rel)
                continue
            if not path.is_file():
                special.append(rel)
                continue
            size = int(stat.st_size)
            try:
                content_hash = _file_hash(path)
            except FileNotFoundError:
                unstable.append(rel)
                continue
            except OSError:
                special.append(rel)
                continue
            entries.append(
                _FileEntry(path=rel, source_path=path, size=size, content_hash=content_hash)
            )

    if special or unstable:
        raise WorkspaceBaseIncompleteError(
            special_file_rejections=tuple(sorted(special)),
            unstable_paths=tuple(sorted(unstable)),
        )
    entries.sort(key=lambda item: item.path)
    for entry in entries:
        _update_root_hash(digest, entry)
    return tuple(entries), digest.hexdigest()


def _symlink_entry(*, path: Path, rel: str) -> _SymlinkEntry:
    target = os.readlink(path)
    return _SymlinkEntry(path=rel, link_target=target)


def _write_base_layer(stack: Path, entries: tuple[_BaseEntry, ...]) -> LayerRef:
    layer_id = WORKSPACE_BASE_LAYER_ID
    layer_dir = stack / LAYERS_DIR / layer_id
    staging_dir = stack / STAGING_DIR / f"{layer_id}.staging"
    if layer_dir.exists() or staging_dir.exists():
        raise WorkspaceBaseAlreadyExistsError(f"base layer already exists: {layer_dir}")
    staging_dir.mkdir(parents=True)
    try:
        for entry in entries:
            target = staging_dir.joinpath(*entry.path.split("/"))
            target.parent.mkdir(parents=True, exist_ok=True)
            if isinstance(entry, _FileEntry):
                try:
                    current_hash = _file_hash(entry.source_path)
                except FileNotFoundError as exc:
                    raise WorkspaceBaseIncompleteError(
                        special_file_rejections=(),
                        unstable_paths=(entry.path,),
                    ) from exc
                if current_hash != entry.content_hash:
                    raise WorkspaceBaseIncompleteError(
                        special_file_rejections=(),
                        unstable_paths=(entry.path,),
                    )
                shutil.copy2(entry.source_path, target)
                fsync_path(target)
            elif isinstance(entry, _SymlinkEntry):
                os.symlink(entry.link_target, target)
            elif isinstance(entry, _DirectoryEntry):
                target.mkdir(parents=True, exist_ok=True)
        fsync_path(staging_dir)
        layer_dir.parent.mkdir(parents=True, exist_ok=True)
        os.replace(staging_dir, layer_dir)
        fsync_path(layer_dir.parent)
    except Exception:
        shutil.rmtree(staging_dir, ignore_errors=True)
        shutil.rmtree(layer_dir, ignore_errors=True)
        raise
    return LayerRef(layer_id=layer_id, path=f"{LAYERS_DIR}/{layer_id}")


def _file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _update_root_hash(digest: hashlib._Hash, entry: _BaseEntry) -> None:
    digest.update(entry.kind.encode("utf-8"))
    digest.update(b"\0")
    digest.update(entry.path.encode("utf-8"))
    digest.update(b"\0")
    if isinstance(entry, _FileEntry):
        digest.update(str(entry.size).encode("ascii"))
        digest.update(b"\0")
        digest.update(entry.content_hash.encode("ascii"))
    elif isinstance(entry, _SymlinkEntry):
        digest.update(entry.link_target.encode("utf-8"))
    digest.update(b"\0")


def _relative(workspace: Path, path: Path) -> str:
    return path.relative_to(workspace).as_posix()


__all__ = [
    "WORKSPACE_BASE_LAYER_ID",
    "WorkspaceBaseAlreadyExistsError",
    "WorkspaceBaseIncompleteError",
    "build_workspace_base",
]
