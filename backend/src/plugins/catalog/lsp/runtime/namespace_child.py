"""Run Pyright inside a private mount namespace with /testbed overlaid."""

from __future__ import annotations

import json
import os
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
        sys.stderr.write("lsp namespace helper requires one JSON payload path\n")
        return 2
    try:
        payload = json.loads(Path(args[0]).read_text(encoding="utf-8"))
        if not isinstance(payload, dict):
            raise ValueError("payload must be an object")
        request = _request(payload)
        mount_inputs = validate_mount_inputs(
            workspace_root=request.workspace_root,
            layer_paths=request.layer_paths,
            upperdir=request.upperdir,
            workdir=request.workdir,
        )
        _mount_and_exec(request, mount_inputs)
    except Exception as exc:
        sys.stderr.write(f"failed to start lsp namespace: {exc}\n")
        return 126
    return 126


class _Request:
    def __init__(self, payload: dict[str, Any]) -> None:
        self.workspace_root = Path(str(payload["workspace_root"]))
        raw_layers = payload["layer_paths"]
        if not isinstance(raw_layers, list) or not raw_layers:
            raise ValueError("layer_paths must be a non-empty list")
        self.layer_paths = tuple(Path(str(path)) for path in raw_layers)
        self.upperdir = Path(str(payload["upperdir"]))
        self.workdir = Path(str(payload["workdir"]))
        raw_argv = payload["argv"]
        if not isinstance(raw_argv, list) or not raw_argv:
            raise ValueError("argv must be a non-empty list")
        self.argv = [str(part) for part in raw_argv]
        raw_env = payload.get("env")
        self.env = (
            {str(key): str(value) for key, value in raw_env.items()}
            if isinstance(raw_env, dict)
            else os.environ.copy()
        )


def _request(payload: dict[str, Any]) -> _Request:
    return _Request(payload)


def _mount_and_exec(request: _Request, mount_inputs: MountInputs) -> None:
    try:
        mount_overlay(
            workspace_root=mount_inputs.workspace_root,
            layer_paths=mount_inputs.layer_paths,
            upperdir=mount_inputs.upperdir,
            workdir=mount_inputs.workdir,
            pass_fds=mount_inputs.fds,
        )
    finally:
        mount_inputs.close()
    os.execvpe(request.argv[0], request.argv, request.env)


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
