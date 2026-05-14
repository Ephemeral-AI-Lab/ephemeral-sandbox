"""Durable workspace binding for layer-stack-backed sandbox reads."""

from __future__ import annotations

import json
import os
from collections.abc import Mapping
from dataclasses import dataclass
from pathlib import Path

from sandbox.layer_stack.layer_change import normalize_layer_path


WORKSPACE_BINDING_FILE = "workspace.json"


class WorkspaceBindingError(RuntimeError):
    """Raised when layer-stack workspace binding state is invalid or missing."""


@dataclass(frozen=True, kw_only=True)
class WorkspaceBinding:
    workspace_root: str
    layer_stack_root: str
    active_manifest_version: int
    active_root_hash: str
    base_manifest_version: int
    base_root_hash: str

    def to_dict(self) -> dict[str, object]:
        return {
            "workspace_root": self.workspace_root,
            "layer_stack_root": self.layer_stack_root,
            "active_manifest_version": self.active_manifest_version,
            "active_root_hash": self.active_root_hash,
            "base_manifest_version": self.base_manifest_version,
            "base_root_hash": self.base_root_hash,
        }

    @classmethod
    def from_dict(cls, payload: Mapping[str, object]) -> WorkspaceBinding:
        return cls(
            workspace_root=str(payload["workspace_root"]),
            layer_stack_root=str(payload["layer_stack_root"]),
            active_manifest_version=int(payload["active_manifest_version"]),
            active_root_hash=str(payload["active_root_hash"]),
            base_manifest_version=int(payload["base_manifest_version"]),
            base_root_hash=str(payload["base_root_hash"]),
        )

    def layer_path_from_relative(self, path: str) -> str:
        """Translate a repo-relative path to a layer path."""
        raw = str(path or "").strip()
        if not raw:
            raise WorkspaceBindingError("path is required")
        if raw.startswith("/"):
            raise WorkspaceBindingError(f"path must be relative: {raw}")
        return normalize_layer_path(raw)

    def layer_path_from_absolute(self, path: str) -> str:
        """Translate a workspace-absolute path to a layer path."""
        raw = str(path or "").strip()
        if not raw:
            raise WorkspaceBindingError("path is required")
        if not raw.startswith("/"):
            raise WorkspaceBindingError(f"path must be absolute: {raw}")
        workspace = Path(self.workspace_root)
        candidate = Path(raw)
        try:
            relative = candidate.relative_to(workspace)
        except ValueError as exc:
            raise WorkspaceBindingError(
                f"path is outside bound workspace {self.workspace_root}: {raw}"
            ) from exc
        return normalize_layer_path(relative.as_posix())


def workspace_binding_path(layer_stack_root: str | Path) -> Path:
    return Path(layer_stack_root) / WORKSPACE_BINDING_FILE


def read_workspace_binding(layer_stack_root: str | Path) -> WorkspaceBinding | None:
    path = workspace_binding_path(layer_stack_root)
    if not path.exists():
        return None
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise WorkspaceBindingError("workspace binding payload must be an object")
    return WorkspaceBinding.from_dict(payload)


def require_workspace_binding(layer_stack_root: str | Path) -> WorkspaceBinding:
    binding = read_workspace_binding(layer_stack_root)
    if binding is None:
        raise WorkspaceBindingError(
            f"workspace binding is missing: {workspace_binding_path(layer_stack_root)}"
        )
    return binding


def write_workspace_binding_atomic(binding: WorkspaceBinding) -> None:
    validate_workspace_binding_paths(
        workspace_root=binding.workspace_root,
        layer_stack_root=binding.layer_stack_root,
    )
    path = workspace_binding_path(binding.layer_stack_root)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.tmp")
    data = json.dumps(binding.to_dict(), indent=2, sort_keys=True).encode("utf-8")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
    try:
        os.write(fd, data)
        os.fsync(fd)
    finally:
        os.close(fd)
    os.replace(tmp, path)
    dir_fd = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(dir_fd)
    finally:
        os.close(dir_fd)


def validate_workspace_binding_paths(
    *,
    workspace_root: str | Path,
    layer_stack_root: str | Path,
) -> None:
    workspace = Path(workspace_root)
    stack = Path(layer_stack_root)
    if not workspace.is_absolute():
        raise WorkspaceBindingError(f"workspace_root must be absolute: {workspace}")
    if not stack.is_absolute():
        raise WorkspaceBindingError(f"layer_stack_root must be absolute: {stack}")

    workspace_resolved = workspace.resolve(strict=False)
    stack_resolved = stack.resolve(strict=False)
    if workspace_resolved == stack_resolved or stack_resolved.is_relative_to(
        workspace_resolved
    ):
        raise WorkspaceBindingError(
            "layer_stack_root must be outside workspace_root: "
            f"{stack_resolved} is inside {workspace_resolved}"
        )


__all__ = [
    "WORKSPACE_BINDING_FILE",
    "WorkspaceBinding",
    "WorkspaceBindingError",
    "read_workspace_binding",
    "require_workspace_binding",
    "validate_workspace_binding_paths",
    "workspace_binding_path",
    "write_workspace_binding_atomic",
]
