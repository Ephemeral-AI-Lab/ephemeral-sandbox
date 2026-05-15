"""Helper executed inside a private mount namespace.

This is command-exec's workspace replacement helper. The older
``sandbox.overlay.namespace`` path is for snapshot-overlay requests and stays
separate because command-exec must capture an upperdir for OCC submission.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from contextlib import suppress
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from sandbox.execution.strategy_private_namespace import (
    NAMESPACE_FALLBACK_STRATEGY,
    NAMESPACE_INFRA_EXIT_CODE,
)
from sandbox.execution.env_policy import (
    DEFAULT_COMMAND_EXEC_POLICY,
    CommandExecPolicy,
)
from sandbox.execution.subprocess_runner import run_command_to_refs
from sandbox.timing import monotonic_now


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
    try:
        request = _payload_request(payload)
        request.stdout_ref.parent.mkdir(parents=True, exist_ok=True)
        request.stderr_ref.parent.mkdir(parents=True, exist_ok=True)
    except KeyError as exc:
        return _fail_bad_payload(
            payload,
            timings,
            f"missing payload key: {exc.args[0]}",
        )
    except Exception as exc:
        return _fail_bad_payload(payload, timings, str(exc))

    mount_inputs: _MountInputs | None = None
    try:
        mount_inputs = _validate_mount_inputs(
            workspace_root=request.workspace_root,
            lowerdir=request.lowerdir,
            upperdir=request.upperdir,
            workdir=request.workdir,
            policy=request.policy,
        )
        mount_start = monotonic_now()
        _mount_overlay(
            workspace_root=mount_inputs.workspace_root,
            lowerdir=mount_inputs.lowerdir,
            upperdir=mount_inputs.upperdir,
            workdir=mount_inputs.workdir,
            pass_fds=mount_inputs.fds,
        )
        timings["command_exec.mount_workspace_s"] = monotonic_now() - mount_start
    except subprocess.CalledProcessError as exc:
        stderr = str(exc.stderr or "").strip()
        stdout = str(exc.stdout or "").strip()
        detail = stderr or stdout
        message = f"{exc}; {detail}" if detail else str(exc)
        return _fail(request, timings, "mount_failed", message, recoverable=True)
    except ValueError as exc:
        return _fail(request, timings, "validation_failed", str(exc))
    except OSError as exc:
        return _fail(request, timings, "setup_failed", str(exc))
    except Exception as exc:
        return _fail(request, timings, "unexpected_setup_failed", str(exc))
    finally:
        if mount_inputs is not None:
            mount_inputs.close()

    try:
        run_start = monotonic_now()
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
            declared_workspace_root=request.workspace_root,
            mounted_workspace_root=request.workspace_root,
            cwd=str(payload.get("cwd") or "."),
            env=env,
            timeout_seconds=timeout,
            stdout_ref=request.stdout_ref,
            stderr_ref=request.stderr_ref,
            policy=request.policy,
        )
        timings["command_exec.run_command_s"] = monotonic_now() - run_start
        return exit_code
    except Exception as exc:
        with request.stderr_ref.open("ab") as stderr_file:
            stderr_file.write(
                _json_error_line("command_failed", str(exc)).encode()
            )
        return 126
    finally:
        _umount(request.workspace_root)
        _write_timings(request.timings_ref, timings)


@dataclass(frozen=True)
class _NamespaceRequest:
    workspace_root: Path
    lowerdir: Path
    upperdir: Path
    workdir: Path
    stdout_ref: Path
    stderr_ref: Path
    timings_ref: Path
    control_ref: Path | None
    policy: CommandExecPolicy


@dataclass(frozen=True)
class _MountInputs:
    workspace_root: Path
    lowerdir: Path
    upperdir: Path
    workdir: Path
    fds: tuple[int, ...]

    def close(self) -> None:
        for fd in self.fds:
            with suppress(OSError):
                os.close(fd)


def _payload_request(payload: dict[str, Any]) -> _NamespaceRequest:
    return _NamespaceRequest(
        workspace_root=Path(str(payload["workspace_root"])),
        lowerdir=Path(str(payload["lowerdir"])),
        upperdir=Path(str(payload["upperdir"])),
        workdir=Path(str(payload["workdir"])),
        stdout_ref=Path(str(payload["stdout_ref"])),
        stderr_ref=Path(str(payload["stderr_ref"])),
        timings_ref=Path(str(payload["timings_ref"])),
        control_ref=(
            Path(str(payload["control_ref"]))
            if payload.get("control_ref")
            else None
        ),
        policy=CommandExecPolicy.from_payload(
            payload["policy"] if isinstance(payload.get("policy"), dict) else {}
        ),
    )


def _mount_overlay(
    *,
    workspace_root: Path,
    lowerdir: Path,
    upperdir: Path,
    workdir: Path,
    pass_fds: tuple[int, ...] = (),
) -> None:
    options = f"lowerdir={lowerdir},upperdir={upperdir},workdir={workdir}"
    subprocess.run(
        ["mount", "-t", "overlay", "overlay", "-o", options, str(workspace_root)],
        check=True,
        capture_output=True,
        text=True,
        pass_fds=pass_fds,
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
    policy: CommandExecPolicy = DEFAULT_COMMAND_EXEC_POLICY,
) -> _MountInputs:
    fds: list[int] = []
    try:
        for path in (workspace_root, lowerdir, upperdir, workdir):
            policy.validate_overlay_path_text(path.as_posix())
        for path, label in (
            (workspace_root, "workspace root"),
            (lowerdir, "leased lowerdir"),
        ):
            if path.is_symlink():
                raise ValueError(f"{label} must not be a symlink: {path}")
            if not path.is_dir():
                raise ValueError(f"{label} is missing: {path}")
            fds.append(_open_dir_no_follow(path))
        for path in (upperdir, workdir):
            if path.is_symlink():
                raise ValueError(f"mount scratch dir must not be a symlink: {path}")
            if path.exists() and not path.is_dir():
                raise ValueError(f"mount scratch path is not a directory: {path}")
            path.mkdir(parents=True, exist_ok=True)
            fds.append(_open_dir_no_follow(path))
        return _MountInputs(
            workspace_root=Path(f"/proc/self/fd/{fds[0]}"),
            lowerdir=Path(f"/proc/self/fd/{fds[1]}"),
            upperdir=Path(f"/proc/self/fd/{fds[2]}"),
            workdir=Path(f"/proc/self/fd/{fds[3]}"),
            fds=tuple(fds),
        )
    except Exception:
        for fd in fds:
            with suppress(OSError):
                os.close(fd)
        raise


def _open_dir_no_follow(path: Path) -> int:
    flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    return os.open(path, flags)


def _write_error(path: Path, error_kind: str, detail: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(_json_error_line(error_kind, detail), encoding="utf-8")


def _json_error_line(error_kind: str, detail: str) -> str:
    return json.dumps(
        {"error_kind": error_kind, "detail": detail},
        separators=(",", ":"),
        sort_keys=True,
    ) + "\n"


def _write_timings(path: Path, timings: dict[str, float]) -> None:
    path.write_text(
        json.dumps(timings, separators=(",", ":"), sort_keys=True),
        encoding="utf-8",
    )


def _fail_bad_payload(
    payload: dict[str, Any],
    timings: dict[str, float],
    detail: str,
) -> int:
    try:
        stderr_ref = _resolve_fallback_ref(payload, "stderr_ref")
        timings_ref = _resolve_fallback_ref(payload, "timings_ref")
    except ValueError as exc:
        sys.stderr.write(f"bad namespace helper payload: {detail}; {exc}\n")
        return 2
    _write_error(stderr_ref, "bad_payload", detail)
    _write_timings(timings_ref, timings)
    return 126


def _resolve_fallback_ref(payload: dict[str, Any], key: str) -> Path:
    raw = payload.get(key)
    if not raw:
        raise ValueError(f"payload is missing {key}; no fallback ref available")
    path = Path(str(raw))
    path.parent.mkdir(parents=True, exist_ok=True)
    return path


def _fail(
    request: _NamespaceRequest,
    timings: dict[str, float],
    error_kind: str,
    detail: str,
    *,
    recoverable: bool = False,
) -> int:
    _write_error(request.stderr_ref, error_kind, detail)
    if recoverable and request.control_ref is not None:
        request.control_ref.parent.mkdir(parents=True, exist_ok=True)
        request.control_ref.write_text(
            json.dumps(
                {
                    "detail": detail,
                    "error_kind": error_kind,
                    "fallback": NAMESPACE_FALLBACK_STRATEGY,
                },
                separators=(",", ":"),
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
    _write_timings(request.timings_ref, timings)
    return NAMESPACE_INFRA_EXIT_CODE if recoverable else 126


__all__ = [
    "execute",
    "main",
]


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
