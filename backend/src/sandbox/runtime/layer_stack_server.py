"""Runtime-local workspace server for layer-stack base construction."""

from __future__ import annotations

from pathlib import Path

from sandbox.layer_stack.workspace_base import build_workspace_base
from sandbox.layer_stack.manifest import manifest_path, read_manifest
from sandbox.layer_stack.workspace import (
    WorkspaceBinding,
    WorkspaceBindingError,
    read_workspace_binding,
)


class LayerStackWorkspaceServer:
    """Owns binding and first base build for one layer-stack root."""

    def __init__(self, layer_stack_root: str | Path) -> None:
        self.layer_stack_root = Path(layer_stack_root)

    def build_workspace_base(
        self,
        *,
        workspace_root: str | Path,
        reset: bool = False,
    ) -> WorkspaceBinding:
        return build_workspace_base(
            workspace_root=workspace_root,
            layer_stack_root=self.layer_stack_root,
            reset=reset,
        )

    def ensure_workspace_base(
        self,
        *,
        workspace_root: str | Path,
    ) -> tuple[WorkspaceBinding, bool]:
        binding = read_workspace_binding(self.layer_stack_root)
        if binding is not None:
            manifest_file = manifest_path(self.layer_stack_root)
            if not manifest_file.exists():
                raise WorkspaceBindingError(
                    f"active manifest is missing for workspace binding: {manifest_file}"
                )
            active = read_manifest(manifest_file)
            if active.version <= 0:
                raise WorkspaceBindingError(
                    f"active manifest is empty for workspace binding: {manifest_file}"
                )
            if Path(binding.workspace_root) != Path(workspace_root):
                raise WorkspaceBindingError(
                    "workspace binding points at a different workspace: "
                    f"{binding.workspace_root} != {workspace_root}"
                )
            return binding, False
        return self.build_workspace_base(
            workspace_root=workspace_root,
        ), True


__all__ = ["LayerStackWorkspaceServer"]
