"""Workspace base construction for an empty layer stack."""

from __future__ import annotations

import hashlib
import os
import shutil
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

from sandbox.layer_stack.manifest import (
    LAYERS_DIR,
    STAGING_DIR,
    LayerRef,
    Manifest,
    manifest_path,
    read_manifest,
    write_manifest_atomic,
)
from sandbox.layer_stack.workspace import (
    WorkspaceBinding,
    WorkspaceBindingError,
    read_workspace_binding,
    validate_workspace_binding_paths,
    write_workspace_binding_atomic,
)


WORKSPACE_BASE_LAYER_ID = "L000001-base"


class WorkspaceBaseAlreadyExistsError(RuntimeError):
    """Raised when a workspace base is requested for non-empty stack state."""


class WorkspaceBaseIncompleteError(WorkspaceBindingError):
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
class _BaseEntry:
    path: str
    kind: Literal["directory", "file", "symlink"]
    source_path: Path | None = None
    link_target: str | None = None
    size: int = 0
    content_hash: str = ""


def build_workspace_base(
    *,
    workspace_root: str | Path,
    layer_stack_root: str | Path,
    reset: bool = False,
) -> WorkspaceBinding:
    """Build *workspace_root* as manifest version 1.

    The base build is a full workspace copy. It either represents every regular
    file and symlink from the assigned workspace or fails before publishing
    workspace truth.
    """
    workspace = Path(workspace_root)
    stack = Path(layer_stack_root)
    validate_workspace_binding_paths(
        workspace_root=workspace,
        layer_stack_root=stack,
    )
    if not workspace.is_dir():
        raise WorkspaceBindingError(f"workspace_root does not exist: {workspace}")

    if reset:
        shutil.rmtree(stack, ignore_errors=True)
    _prepare_empty_stack(stack)
    _reject_existing_base_state(stack)

    entries, root_hash = _collect_base_entries(workspace)
    layer_ref = _write_base_layer(stack, entries)
    manifest = Manifest(version=1, layers=(layer_ref,))
    write_manifest_atomic(manifest_path(stack), manifest)
    binding = WorkspaceBinding(
        workspace_root=workspace.as_posix(),
        layer_stack_root=stack.as_posix(),
        active_manifest_version=manifest.version,
        active_root_hash=root_hash,
        base_manifest_version=manifest.version,
        base_root_hash=root_hash,
    )
    write_workspace_binding_atomic(stack, binding)
    return binding


def _prepare_empty_stack(stack: Path) -> None:
    stack.mkdir(parents=True, exist_ok=True)
    (stack / LAYERS_DIR).mkdir(exist_ok=True)
    (stack / STAGING_DIR).mkdir(exist_ok=True)


def _reject_existing_base_state(stack: Path) -> None:
    binding = read_workspace_binding(stack)
    if binding is not None:
        raise WorkspaceBaseAlreadyExistsError(
            f"workspace base already exists at {stack}"
        )
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


def _collect_base_entries(
    workspace: Path,
) -> tuple[tuple[_BaseEntry, ...], str]:
    special: list[str] = []
    unstable: list[str] = []
    entries: list[_BaseEntry] = []
    digest = hashlib.sha256()

    for current_root, dirnames, filenames in os.walk(
        workspace,
        topdown=True,
        followlinks=False,
    ):
        current = Path(current_root)
        dirnames.sort()
        filenames.sort()

        kept_dirs: list[str] = []
        for dirname in dirnames:
            path = current / dirname
            rel = _relative(workspace, path)
            if path.is_symlink():
                entry = _symlink_entry(
                    path=path,
                    rel=rel,
                )
                entries.append(entry)
                continue
            entries.append(_BaseEntry(path=rel, kind="directory"))
            kept_dirs.append(dirname)
        dirnames[:] = kept_dirs

        for filename in filenames:
            path = current / filename
            rel = _relative(workspace, path)
            if path.is_symlink():
                entry = _symlink_entry(
                    path=path,
                    rel=rel,
                )
                entries.append(entry)
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
            content_hash = _file_hash(path)
            entries.append(
                _BaseEntry(
                    path=rel,
                    kind="file",
                    source_path=path,
                    size=size,
                    content_hash=content_hash,
                )
            )

    for entry in sorted(entries, key=lambda item: item.path):
        _update_root_hash(digest, entry)

    if special or unstable:
        raise WorkspaceBaseIncompleteError(
            special_file_rejections=tuple(sorted(special)),
            unstable_paths=tuple(sorted(unstable)),
        )
    return tuple(sorted(entries, key=lambda item: item.path)), digest.hexdigest()


def _symlink_entry(
    *,
    path: Path,
    rel: str,
) -> _BaseEntry:
    target = os.readlink(path)
    return _BaseEntry(path=rel, kind="symlink", link_target=target)


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
            if entry.kind == "file":
                if entry.source_path is None:
                    raise ValueError(f"missing source path for {entry.path}")
                if _file_hash(entry.source_path) != entry.content_hash:
                    raise RuntimeError(
                        f"workspace file changed during base build: {entry.path}"
                    )
                shutil.copy2(entry.source_path, target)
            elif entry.kind == "symlink":
                os.symlink(str(entry.link_target or ""), target)
            elif entry.kind == "directory":
                target.mkdir(parents=True, exist_ok=True)
        layer_dir.parent.mkdir(parents=True, exist_ok=True)
        os.replace(staging_dir, layer_dir)
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


def _update_root_hash(digest: "hashlib._Hash", entry: _BaseEntry) -> None:
    digest.update(entry.kind.encode("utf-8"))
    digest.update(b"\0")
    digest.update(entry.path.encode("utf-8"))
    digest.update(b"\0")
    if entry.kind == "file":
        digest.update(str(entry.size).encode("ascii"))
        digest.update(b"\0")
        digest.update(entry.content_hash.encode("ascii"))
    elif entry.kind == "symlink":
        digest.update(str(entry.link_target or "").encode("utf-8"))
    digest.update(b"\0")


def _relative(workspace: Path, path: Path) -> str:
    return path.relative_to(workspace).as_posix()


__all__ = [
    "WORKSPACE_BASE_LAYER_ID",
    "WorkspaceBaseAlreadyExistsError",
    "WorkspaceBaseIncompleteError",
    "build_workspace_base",
]
