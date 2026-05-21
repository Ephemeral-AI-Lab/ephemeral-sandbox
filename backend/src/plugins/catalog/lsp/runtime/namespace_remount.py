"""Refresh a Pyright private mount namespace with a new workspace overlay."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path
from typing import Any

from sandbox.execution.overlay.kernel_mount import (
    MountInputs,
    mount_overlay,
    validate_mount_inputs,
)


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if len(args) != 1:
        sys.stderr.write("lsp namespace remount helper requires one JSON payload path\n")
        return 2
    payload_path = Path(args[0])
    try:
        payload = json.loads(payload_path.read_text(encoding="utf-8"))
        if not isinstance(payload, dict):
            raise ValueError("payload must be an object")
        request = _Request(payload)
    except Exception as exc:
        sys.stderr.write(f"bad lsp namespace remount payload: {exc}\n")
        return 2

    mount_inputs: MountInputs | None = None
    try:
        _detach_mount(request.workspace_root)
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
        return 0
    except Exception as exc:
        sys.stderr.write(f"failed to remount lsp namespace overlay: {exc}\n")
        return 126
    finally:
        if mount_inputs is not None:
            mount_inputs.close()


class _Request:
    def __init__(self, payload: dict[str, Any]) -> None:
        self.workspace_root = Path(str(payload["workspace_root"]))
        raw_layers = payload["layer_paths"]
        if not isinstance(raw_layers, list) or not raw_layers:
            raise ValueError("layer_paths must be a non-empty list")
        self.layer_paths = tuple(Path(str(path)) for path in raw_layers)
        self.upperdir = Path(str(payload["upperdir"]))
        self.workdir = Path(str(payload["workdir"]))


def _detach_mount(workspace_root: Path) -> None:
    for _ in range(64):
        if not _is_mountpoint(workspace_root):
            return
        result = subprocess.run(
            ["umount", str(workspace_root)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if result.returncode == 0:
            continue
        lazy = subprocess.run(
            ["umount", "-l", str(workspace_root)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if lazy.returncode != 0:
            raise RuntimeError(f"failed to detach existing mount: {workspace_root}")
        return


def _is_mountpoint(path: Path) -> bool:
    result = subprocess.run(
        ["mountpoint", "-q", str(path)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return result.returncode == 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
