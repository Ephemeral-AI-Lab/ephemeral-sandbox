"""Private mount namespace command execution strategy."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

from sandbox.execution.contract import (
    CommandExecRequest,
    MountMode,
    ShellProcessResult,
    WorkspaceReplacementMountSpec,
)
from sandbox.execution.policy import (
    DEFAULT_COMMAND_EXEC_POLICY,
    CommandExecPolicy,
)
from sandbox.execution.strategy_base import ExecutionStrategy

NAMESPACE_INFRA_EXIT_CODE = 125
NAMESPACE_CONTROL_REF = "namespace-control.json"
NAMESPACE_FALLBACK_STRATEGY = "copy_backed"


class PrivateNamespaceStrategy(ExecutionStrategy):
    """Run a command by overlay-mounting the leased workspace in a namespace."""

    name = "private_namespace"

    def __init__(
        self,
        *,
        available: bool,
        policy: CommandExecPolicy = DEFAULT_COMMAND_EXEC_POLICY,
    ) -> None:
        self._available = available
        self._policy = policy

    def is_available(self) -> bool:
        return self._available

    def run(
        self,
        *,
        spec: WorkspaceReplacementMountSpec,
        request: CommandExecRequest,
        run_dir: Path,
        timings: dict[str, float],
    ) -> ShellProcessResult:
        stdout_ref = run_dir / "stdout.bin"
        stderr_ref = run_dir / "stderr.bin"
        timings_ref = run_dir / "namespace-timings.json"
        control_ref = run_dir / NAMESPACE_CONTROL_REF
        payload_ref = run_dir / "namespace-request.json"
        payload_ref.write_text(
            json.dumps(
                {
                    "workspace_root": spec.workspace_root,
                    "lowerdir": spec.lowerdir,
                    "upperdir": spec.upperdir,
                    "workdir": spec.workdir,
                    "command": list(request.command),
                    "cwd": request.cwd,
                    "env": dict(request.env),
                    "timeout_seconds": request.timeout_seconds,
                    "stdout_ref": str(stdout_ref),
                    "stderr_ref": str(stderr_ref),
                    "timings_ref": str(timings_ref),
                    "control_ref": str(control_ref),
                    "policy": self._policy.to_payload(),
                },
                separators=(",", ":"),
                sort_keys=True,
            ),
            encoding="utf-8",
        )
        timeout = (
            None if request.timeout_seconds is None else request.timeout_seconds + 10
        )
        stdout_ref.parent.mkdir(parents=True, exist_ok=True)
        stderr_ref.parent.mkdir(parents=True, exist_ok=True)
        with stdout_ref.open("wb") as stdout_file, stderr_ref.open("wb") as stderr_file:
            completed = subprocess.run(
                [
                    _unshare_path(),
                    "-Urm",
                    sys.executable,
                    "-m",
                    "sandbox.execution.namespace_child",
                    str(payload_ref),
                ],
                stdout=stdout_file,
                stderr=stderr_file,
                timeout=timeout,
                check=False,
            )
        _merge_namespace_timings(timings_ref, timings)
        return ShellProcessResult(
            exit_code=int(completed.returncode),
            stdout_ref=str(stdout_ref),
            stderr_ref=str(stderr_ref),
            mounted_workspace_root=spec.workspace_root,
            mount_mode=MountMode.PRIVATE_NAMESPACE,
        )

    def is_recoverable_failure(
        self,
        result: ShellProcessResult,
        *,
        run_dir: Path,
    ) -> bool:
        if (
            result.mount_mode != MountMode.PRIVATE_NAMESPACE
            or result.exit_code != NAMESPACE_INFRA_EXIT_CODE
        ):
            return False
        try:
            payload = json.loads(
                (run_dir / NAMESPACE_CONTROL_REF).read_text(encoding="utf-8")
            )
        except (OSError, json.JSONDecodeError):
            return False
        return (
            isinstance(payload, dict)
            and payload.get("error_kind") == "mount_failed"
            and payload.get("fallback") == NAMESPACE_FALLBACK_STRATEGY
        )


def detect_private_mount_namespace() -> bool:
    if os.name != "posix" or not sys.platform.startswith("linux"):
        return False
    if _unshare_path() == "" or shutil.which("mount") is None:
        return False
    try:
        result = subprocess.run(
            [_unshare_path(), "-Urm", "true"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=2,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return False
    return result.returncode == 0


def _merge_namespace_timings(path: Path, timings: dict[str, float]) -> None:
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return
    if not isinstance(raw, dict):
        return
    for key, value in raw.items():
        if isinstance(value, (int, float)):
            timings[str(key)] = float(value)


def _unshare_path() -> str:
    return shutil.which("unshare") or ""


__all__ = [
    "NAMESPACE_CONTROL_REF",
    "NAMESPACE_FALLBACK_STRATEGY",
    "NAMESPACE_INFRA_EXIT_CODE",
    "PrivateNamespaceStrategy",
    "detect_private_mount_namespace",
]
