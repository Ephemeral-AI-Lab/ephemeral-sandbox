"""Apply an LSP WorkspaceEdit inside a private overlay mount namespace."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

from plugins.catalog.lsp.runtime.apply import _apply_edit_payload
from sandbox.execution.overlay.kernel_mount import (
    MountInputs,
    mount_overlay,
    umount,
    validate_mount_inputs,
)


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if len(args) != 1:
        sys.stderr.write("lsp apply helper requires one JSON payload path\n")
        return 2
    payload_path = Path(args[0])
    try:
        payload = json.loads(payload_path.read_text(encoding="utf-8"))
        if not isinstance(payload, dict):
            raise ValueError("payload must be an object")
        request = _Request(payload)
    except Exception as exc:
        sys.stderr.write(f"bad lsp apply payload: {exc}\n")
        return 2

    mount_inputs: MountInputs | None = None
    try:
        mount_inputs = validate_mount_inputs(
            workspace_root=request.workspace_root,
            layer_paths=request.layer_paths,
            upperdir=request.upperdir,
            workdir=request.workdir,
        )
        mount_overlay(
            workspace_root=mount_inputs.workspace_root,
            layer_paths=mount_inputs.layer_paths,
            upperdir=mount_inputs.upperdir,
            workdir=mount_inputs.workdir,
            pass_fds=mount_inputs.fds,
        )
        changed_paths = _apply_edit_payload(
            request.edit,
            workspace_root=request.workspace_root.as_posix(),
        )
        request.output_ref.write_text(
            json.dumps(
                {"changed_paths": changed_paths},
                separators=(",", ":"),
                sort_keys=True,
            ),
            encoding="utf-8",
        )
        return 0
    except Exception as exc:
        sys.stderr.write(f"failed to apply workspace edit in overlay: {exc}\n")
        return 126
    finally:
        if mount_inputs is not None:
            mount_inputs.close()
        umount(request.workspace_root)


class _Request:
    def __init__(self, payload: dict[str, Any]) -> None:
        self.workspace_root = Path(str(payload["workspace_root"]))
        raw_layers = payload["layer_paths"]
        if not isinstance(raw_layers, list) or not raw_layers:
            raise ValueError("layer_paths must be a non-empty list")
        self.layer_paths = tuple(Path(str(path)) for path in raw_layers)
        self.upperdir = Path(str(payload["upperdir"]))
        self.workdir = Path(str(payload["workdir"]))
        self.output_ref = Path(str(payload["output_ref"]))
        raw_edit = payload.get("edit")
        self.edit = raw_edit if isinstance(raw_edit, dict) else {}


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
