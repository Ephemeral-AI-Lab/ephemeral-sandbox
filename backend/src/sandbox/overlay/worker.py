"""Worker entrypoint for one command against a leased snapshot overlay."""

from __future__ import annotations

import argparse
import json
import sys
from collections.abc import Mapping
from pathlib import Path
from typing import Any

from sandbox.layer_stack.manifest import Manifest
from sandbox.overlay.capture import capture_changes
from sandbox.overlay.command import run_user_command
from sandbox.overlay.mounts import cleanup_runtime_run_dir, mount_snapshot
from sandbox.overlay.request import OverlayShellRequest
from sandbox.overlay.result import OverlayCapture, write_overlay_capture
from sandbox.timing import monotonic_now


def execute_request(
    *,
    request_payload: Mapping[str, Any],
    manifest_payload: Mapping[str, Any],
    storage_root: str | Path,
    run_dir: str | Path,
) -> OverlayCapture:
    total_start = monotonic_now()
    timings: dict[str, float] = {}
    run_dir_path = Path(run_dir)
    try:
        request = OverlayShellRequest.from_dict(request_payload)
        manifest = Manifest.from_dict(manifest_payload)
        mount_start = monotonic_now()
        mounted = mount_snapshot(
            manifest=manifest,
            storage_root=storage_root,
            run_dir=run_dir,
            timings=timings,
        )
        timings["overlay.mount_snapshot_s"] = monotonic_now() - mount_start
        stdout_ref = run_dir_path / "stdout.bin"
        stderr_ref = run_dir_path / "stderr.bin"
        command_start = monotonic_now()
        command = run_user_command(
            command=request.command,
            workspace_root=mounted.workspace_root,
            cwd=request.cwd,
            env=dict(request.env),
            timeout_seconds=request.timeout_seconds,
            stdout_ref=stdout_ref,
            stderr_ref=stderr_ref,
        )
        timings["overlay.run_command_s"] = monotonic_now() - command_start
        capture_start = monotonic_now()
        changes = capture_changes(
            mounted.upperdir,
            lowerdir=mounted.lowerdir,
            workspace_root=mounted.workspace_root,
            timings=timings,
        )
        timings["overlay.capture_changes_s"] = monotonic_now() - capture_start
        timings["overlay.total_s"] = monotonic_now() - total_start
        capture = OverlayCapture(
            exit_code=command.exit_code,
            stdout_ref=command.stdout_ref,
            stderr_ref=command.stderr_ref,
            snapshot_version=manifest.version,
            changes=changes,
            snapshot_manifest=manifest,
            timings=timings,
        )
        write_overlay_capture(run_dir, capture)
        return capture
    finally:
        cleanup_runtime_run_dir(run_dir_path)


def _parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--request-json", required=True)
    parser.add_argument("--manifest-json", required=True)
    parser.add_argument("--storage-root", required=True)
    parser.add_argument("--run-dir", required=True)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv if argv is not None else sys.argv[1:])
    capture = execute_request(
        request_payload=json.loads(args.request_json),
        manifest_payload=json.loads(args.manifest_json),
        storage_root=args.storage_root,
        run_dir=args.run_dir,
    )
    sys.stdout.write(json.dumps(capture.to_dict(), separators=(",", ":")))
    sys.stdout.write("\n")
    return 0 if capture.exit_code == 0 else capture.exit_code


__all__ = [
    "execute_request",
    "main",
]


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
