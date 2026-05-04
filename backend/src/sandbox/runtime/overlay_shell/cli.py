"""CLI for running one command against a leased snapshot overlay."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from sandbox.layer_stack.manifest import Manifest
from sandbox.overlay.capture.upperdir import capture_changes
from sandbox.overlay.namespace.command import run_user_command
from sandbox.overlay.namespace.mounts import lowerdir_for, mount_snapshot
from sandbox.overlay.types import overlay_shell_request_from_dict
from sandbox.runtime.overlay_shell.result_envelope import (
    RuntimeResultEnvelope,
    write_result_envelope,
)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--request-json", required=True)
    parser.add_argument("--manifest-json", required=True)
    parser.add_argument("--storage-root", required=True)
    parser.add_argument("--run-dir", required=True)
    return parser.parse_args(argv)


def execute_request(
    *,
    request_payload: dict[str, Any],
    manifest_payload: dict[str, Any],
    storage_root: str | Path,
    run_dir: str | Path,
) -> RuntimeResultEnvelope:
    request = overlay_shell_request_from_dict(request_payload)
    manifest = Manifest.from_dict(manifest_payload)
    mounted = mount_snapshot(
        manifest=manifest,
        storage_root=storage_root,
        run_dir=run_dir,
    )
    stdout_ref = Path(run_dir) / "stdout.bin"
    stderr_ref = Path(run_dir) / "stderr.bin"
    command = run_user_command(
        command=request.command,
        workspace_root=mounted.workspace_root,
        cwd=request.cwd,
        env=dict(request.env),
        timeout_seconds=request.timeout_seconds,
        stdout_ref=stdout_ref,
        stderr_ref=stderr_ref,
    )
    upper_changes = capture_changes(
        mounted.upperdir,
        snapshot_manifest=mounted.manifest,
        lowerdir=lowerdir_for(mounted),
        workspace_root=mounted.workspace_root,
    )
    envelope = RuntimeResultEnvelope(
        exit_code=command.exit_code,
        stdout_ref=command.stdout_ref,
        stderr_ref=command.stderr_ref,
        snapshot_version=manifest.version,
        upper_changes=upper_changes,
    )
    write_result_envelope(run_dir, envelope)
    return envelope


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv if argv is not None else sys.argv[1:])
    envelope = execute_request(
        request_payload=json.loads(args.request_json),
        manifest_payload=json.loads(args.manifest_json),
        storage_root=args.storage_root,
        run_dir=args.run_dir,
    )
    sys.stdout.write(json.dumps(envelope.to_dict(), separators=(",", ":")))
    sys.stdout.write("\n")
    return 0 if envelope.exit_code == 0 else envelope.exit_code


__all__ = [
    "execute_request",
    "main",
    "parse_args",
]


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
