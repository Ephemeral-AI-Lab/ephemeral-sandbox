"""Helper executed inside a private mount namespace."""

from __future__ import annotations

import json
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

from sandbox.command_exec.workspace.environment import run_command_to_refs


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if len(args) != 1:
        sys.stderr.write("namespace helper requires one JSON payload path\n")
        return 2
    payload = json.loads(Path(args[0]).read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        sys.stderr.write("namespace helper payload must be an object\n")
        return 2
    return execute(payload)


def execute(payload: dict[str, Any]) -> int:
    timings: dict[str, float] = {}
    workspace_root = Path(str(payload["workspace_root"]))
    lowerdir = Path(str(payload["lowerdir"]))
    upperdir = Path(str(payload["upperdir"]))
    workdir = Path(str(payload["workdir"]))
    stdout_ref = Path(str(payload["stdout_ref"]))
    stderr_ref = Path(str(payload["stderr_ref"]))
    timings_ref = Path(str(payload["timings_ref"]))
    stdout_ref.parent.mkdir(parents=True, exist_ok=True)
    stderr_ref.parent.mkdir(parents=True, exist_ok=True)

    try:
        _validate_mount_inputs(
            workspace_root=workspace_root,
            lowerdir=lowerdir,
            upperdir=upperdir,
            workdir=workdir,
        )
        upperdir.mkdir(parents=True, exist_ok=True)
        workdir.mkdir(parents=True, exist_ok=True)
        mount_start = time.perf_counter()
        _mount_overlay(
            workspace_root=workspace_root,
            lowerdir=lowerdir,
            upperdir=upperdir,
            workdir=workdir,
        )
        timings["command_exec.mount_workspace_s"] = time.perf_counter() - mount_start
    except subprocess.CalledProcessError as exc:
        message = _called_process_message(exc)
        stderr_ref.write_text(f"workspace replacement mount failed: {message}\n")
        _write_timings(timings_ref, timings)
        return 126
    except Exception as exc:
        stderr_ref.write_text(f"workspace replacement mount failed: {exc}\n")
        _write_timings(timings_ref, timings)
        return 126

    try:
        run_start = time.perf_counter()
        env_raw = payload.get("env") or {}
        env = (
            {str(key): str(value) for key, value in env_raw.items()}
            if isinstance(env_raw, dict)
            else {}
        )
        timeout_raw = payload.get("timeout_seconds")
        timeout = float(timeout_raw) if timeout_raw is not None else None
        exit_code = run_command_to_refs(
            command=[str(part) for part in payload["command"]],
            declared_workspace_root=workspace_root,
            mounted_workspace_root=workspace_root,
            cwd=str(payload.get("cwd") or "."),
            env=env,
            timeout_seconds=timeout,
            stdout_ref=stdout_ref,
            stderr_ref=stderr_ref,
        )
        timings["command_exec.run_command_s"] = time.perf_counter() - run_start
        return exit_code
    except Exception as exc:
        with stderr_ref.open("ab") as stderr_file:
            stderr_file.write(f"workspace command failed: {exc}\n".encode("utf-8"))
        return 126
    finally:
        _umount(workspace_root)
        _write_timings(timings_ref, timings)


def _mount_overlay(
    *,
    workspace_root: Path,
    lowerdir: Path,
    upperdir: Path,
    workdir: Path,
) -> None:
    options = f"lowerdir={lowerdir},upperdir={upperdir},workdir={workdir}"
    subprocess.run(
        ["mount", "-t", "overlay", "overlay", "-o", options, str(workspace_root)],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _umount(workspace_root: Path) -> None:
    subprocess.run(
        ["umount", str(workspace_root)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )


def _validate_mount_inputs(
    *,
    workspace_root: Path,
    lowerdir: Path,
    upperdir: Path,
    workdir: Path,
) -> None:
    if not workspace_root.is_dir():
        raise RuntimeError(f"workspace root is missing: {workspace_root}")
    if not lowerdir.is_dir():
        raise RuntimeError(f"leased lowerdir is missing: {lowerdir}")
    for path in (workspace_root, lowerdir, upperdir, workdir):
        if "," in path.as_posix():
            raise RuntimeError(f"overlay mount path cannot contain comma: {path}")


def _write_timings(path: Path, timings: dict[str, float]) -> None:
    path.write_text(
        json.dumps(timings, separators=(",", ":"), sort_keys=True),
        encoding="utf-8",
    )


def _called_process_message(exc: subprocess.CalledProcessError) -> str:
    stderr = str(exc.stderr or "").strip()
    stdout = str(exc.stdout or "").strip()
    detail = stderr or stdout
    if detail:
        return f"{exc}; {detail}"
    return str(exc)


__all__ = [
    "execute",
    "main",
]


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
