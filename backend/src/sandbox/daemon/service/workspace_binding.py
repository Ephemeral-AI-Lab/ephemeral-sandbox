"""Runtime-local OCC binding reader."""

from __future__ import annotations

from pathlib import Path

from sandbox.layer_stack.manifest import manifest_path, read_manifest
from sandbox.layer_stack.workspace_binding import require_workspace_binding
from sandbox.occ.ports import WorkspaceBindingSnapshot


class RuntimeWorkspaceBindingReader:
    """Binding reader that fails closed before OCC mutation dispatch."""

    def require_workspace_binding(
        self,
        workspace_ref: str,
    ) -> WorkspaceBindingSnapshot:
        if not workspace_ref:
            raise ValueError("workspace_ref is required")
        binding = require_workspace_binding(workspace_ref)
        manifest_file = manifest_path(workspace_ref)
        if not manifest_file.exists():
            raise RuntimeError(
                f"active manifest is missing for workspace binding: {workspace_ref}"
            )
        if read_manifest(manifest_file).version <= 0:
            raise RuntimeError(
                f"active manifest is empty for workspace binding: {workspace_ref}"
            )
        return WorkspaceBindingSnapshot(
            workspace_ref=workspace_ref,
            workspace_root=binding.workspace_root,
            layer_stack_root=Path(workspace_ref).as_posix(),
        )


__all__ = ["RuntimeWorkspaceBindingReader"]
