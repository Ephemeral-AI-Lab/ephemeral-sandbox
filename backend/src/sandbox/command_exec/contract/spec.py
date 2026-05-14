"""Filesystem spec for guarded command execution."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class WorkspaceReplacementMountSpec:
    """Filesystem inputs for replacing the assigned workspace root."""

    workspace_root: str
    lowerdir: str
    upperdir: str
    workdir: str
    scratch_root: str

    def __post_init__(self) -> None:
        if not str(self.workspace_root).startswith("/"):
            raise ValueError("workspace_root must be absolute")
        if not str(self.scratch_root).strip():
            raise ValueError("scratch_root must not be empty")
        scratch_root = Path(self.scratch_root).resolve(strict=False)
        resolved_paths: dict[str, Path] = {}
        for field_name in ("lowerdir", "upperdir", "workdir"):
            if not str(getattr(self, field_name)).strip():
                raise ValueError(f"{field_name} must not be empty")
            path = Path(str(getattr(self, field_name))).resolve(strict=False)
            if path == scratch_root or not path.is_relative_to(scratch_root):
                raise ValueError(
                    f"{field_name} must be strictly under scratch_root: {path}"
                )
            resolved_paths[field_name] = path

        seen: dict[Path, str] = {}
        for field_name, path in resolved_paths.items():
            duplicate = seen.get(path)
            if duplicate is not None:
                raise ValueError(
                    f"{field_name} must be distinct from {duplicate}: {path}"
                )
            seen[path] = field_name


__all__ = ["WorkspaceReplacementMountSpec"]
