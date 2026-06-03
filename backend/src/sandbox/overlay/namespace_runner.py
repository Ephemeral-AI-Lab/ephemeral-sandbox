"""Private mount namespace tool-call execution."""

from __future__ import annotations

import asyncio
import contextlib
import json
import os
import signal
import shutil
import subprocess
import sys
import threading
from collections.abc import Awaitable, Callable, Mapping
from pathlib import Path
from typing import Any

from sandbox._shared.command_exec_policy import CommandExecPolicy
from sandbox._shared.models import ToolCallRequest, ToolCallResult
from sandbox._shared.tool_primitives.cancellation import (
    NO_OP_CANCELLATION,
    ShellPgrpCancellation,
    VerbCancellation,
)
from sandbox.overlay.handle import OverlayHandle
from sandbox.overlay.namespace_entrypoint import WorkspaceMountMode
from sandbox.overlay.subprocess_runner import (
    CANCEL_POLL_INTERVAL_S,
    CANCEL_SIGKILL_GRACE_S,
    kill_process_group,
)

TOOL_CALL_COMMAND_POLICY = CommandExecPolicy(
    host_env_keys=frozenset(
        {
            "PATH",
            "HOME",
            "USER",
            "LANG",
            "LC_ALL",
            "TERM",
            "TZ",
        }
    ),
)


async def run_in_namespace(
    handle: OverlayHandle,
    req: ToolCallRequest,
    *,
    isolated_runner: Callable[[list[str], bytes | None, float | None], Awaitable[Mapping[str, Any]]]
    | None = None,
    cancellation: VerbCancellation | None = None,
) -> ToolCallResult:
    """Run one tool call through a fresh or already-open mount namespace."""
    effective_cancellation = cancellation or _build_verb_cancellation(req)
    if isolated_runner is not None:
        return await _run_tool_call_in_existing_namespace(
            handle,
            req,
            isolated_runner=isolated_runner,
            cancellation=effective_cancellation,
        )
    return await _run_tool_call_in_fresh_namespace(
        handle,
        req,
        cancellation=effective_cancellation,
    )


async def _run_tool_call_in_fresh_namespace(
    handle: OverlayHandle,
    req: ToolCallRequest,
    *,
    cancellation: VerbCancellation,
) -> ToolCallResult:
    run_dir = handle.run_dir
    stdout_ref = run_dir / "stdout.bin"
    stderr_ref = run_dir / "stderr.bin"
    timings_ref = run_dir / "namespace-tool-timings.json"
    result_ref = run_dir / "namespace-tool-result.json"
    payload_ref = run_dir / "namespace-tool-request.json"
    payload_ref.write_text(
        json.dumps(
            {
                "workspace_root": handle.workspace_root,
                "layer_paths": list(handle.layer_paths),
                "upperdir": handle.upperdir.as_posix(),
                "workdir": handle.workdir.as_posix(),
                "tool_call": req.to_payload(),
                "stdout_ref": str(stdout_ref),
                "stderr_ref": str(stderr_ref),
                "timings_ref": str(timings_ref),
                "result_ref": str(result_ref),
                "policy": TOOL_CALL_COMMAND_POLICY.to_payload(),
                "workspace_mount_mode": WorkspaceMountMode.MOUNT_OVERLAY,
            },
            separators=(",", ":"),
            sort_keys=True,
        ),
        encoding="utf-8",
    )
    stdout_ref.parent.mkdir(parents=True, exist_ok=True)
    stderr_ref.parent.mkdir(parents=True, exist_ok=True)
    child_task = asyncio.create_task(
        _run_namespace_entrypoint_async(
            payload_ref=payload_ref,
            stdout_ref=stdout_ref,
            stderr_ref=stderr_ref,
            timeout=_tool_timeout(req),
            cancel_event=cancellation.cancel_event,
            pid_recorder=cancellation.record_pid,
        )
    )
    try:
        exit_code = await asyncio.shield(child_task)
    except asyncio.CancelledError:
        cancellation.on_cancel()
        with contextlib.suppress(Exception):
            await asyncio.shield(child_task)
        raise
    if result_ref.exists():
        return _read_tool_result(result_ref)
    stderr = stderr_ref.read_text(encoding="utf-8", errors="replace") if stderr_ref.exists() else ""
    return {
        "success": False,
        "workspace": "ephemeral",
        "status": "error",
        "error": {
            "kind": "namespace_entrypoint_failed",
            "message": stderr.strip() or f"namespace entrypoint exited {exit_code}",
        },
        "timings": {},
    }


async def _run_tool_call_in_existing_namespace(
    handle: OverlayHandle,
    req: ToolCallRequest,
    *,
    isolated_runner: Callable[
        [list[str], bytes | None, float | None], Awaitable[Mapping[str, Any]]
    ],
    cancellation: VerbCancellation,
) -> ToolCallResult:
    payload = json.dumps(
        {
            "workspace_root": handle.workspace_root,
            "tool_call": req.to_payload(),
            "stdout_ref": (handle.run_dir / f"{req.invocation_id}.stdout").as_posix(),
            "stderr_ref": (handle.run_dir / f"{req.invocation_id}.stderr").as_posix(),
            "policy": TOOL_CALL_COMMAND_POLICY.to_payload(),
            "workspace_mount_mode": WorkspaceMountMode.EXISTING_MOUNT,
        },
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    src_root = Path(__file__).resolve().parents[2].as_posix()
    script = (
        "import json,sys;"
        f"sys.path.insert(0,{src_root!r});"
        "from sandbox.overlay.namespace_entrypoint import mount_and_execute_tool_payload;"
        "payload=json.loads(sys.stdin.buffer.read());"
        "print(json.dumps(mount_and_execute_tool_payload(payload),separators=(',',':'),sort_keys=True))"
    )
    try:
        response = await isolated_runner(
            [sys.executable, "-c", script],
            payload,
            _tool_timeout(req),
        )
    except asyncio.CancelledError:
        cancellation.on_cancel()
        raise
    if not response.get("success"):
        return dict(response)
    stdout = str(response.get("stdout") or "")
    try:
        result = json.loads(stdout)
    except json.JSONDecodeError:
        return {
            "success": False,
            "workspace": "isolated",
            "status": "error",
            "error": {
                "kind": "namespace_entrypoint_bad_json",
                "message": stdout or str(response.get("stderr") or ""),
            },
            "timings": {},
        }
    if isinstance(result, dict):
        result.setdefault("workspace", "isolated")
        return result
    return {
        "success": False,
        "workspace": "isolated",
        "status": "error",
        "error": {"kind": "namespace_entrypoint_bad_result", "message": repr(result)},
        "timings": {},
    }


def _read_tool_result(path: Path) -> ToolCallResult:
    raw = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(raw, dict):
        raise TypeError("namespace tool result must be a JSON object")
    return raw


def _tool_timeout(req: ToolCallRequest) -> float | None:
    raw = req.args.get("timeout_seconds", req.args.get("timeout"))
    if raw is None:
        return None
    try:
        return float(str(raw)) + 10.0
    except (TypeError, ValueError):
        return None


def _build_verb_cancellation(req: ToolCallRequest) -> VerbCancellation:
    if req.verb == "exec_command":
        return ShellPgrpCancellation()
    return NO_OP_CANCELLATION


async def _run_namespace_entrypoint_async(
    *,
    payload_ref: Path,
    stdout_ref: Path,
    stderr_ref: Path,
    timeout: float | None,
    cancel_event: threading.Event | None,
    pid_recorder: Callable[[int], None] | None,
) -> int:
    """Spawn the namespace entrypoint without consuming the default executor."""
    cmd = [
        _unshare_path(),
        "-Urm",
        sys.executable,
        "-m",
        "sandbox.overlay.namespace_entrypoint",
        str(payload_ref),
    ]
    with stdout_ref.open("wb") as stdout_file, stderr_ref.open("wb") as stderr_file:
        proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdout=stdout_file,
            stderr=stderr_file,
            start_new_session=True,
        )
        if pid_recorder is not None:
            try:
                pid_recorder(proc.pid)
            except Exception:
                pass
        try:
            return await _wait_for_process_with_cancel_async(
                proc,
                command=cmd,
                timeout_seconds=timeout,
                cancel_event=cancel_event,
            )
        except subprocess.TimeoutExpired:
            kill_process_group(proc.pid, signal.SIGKILL)
            with contextlib.suppress(asyncio.TimeoutError, TimeoutError):
                await asyncio.wait_for(proc.wait(), timeout=CANCEL_SIGKILL_GRACE_S)
            raise
        finally:
            if proc.returncode is None:
                kill_process_group(proc.pid, signal.SIGKILL)


async def _wait_for_process_with_cancel_async(
    proc: asyncio.subprocess.Process,
    *,
    command: list[str],
    timeout_seconds: float | None,
    cancel_event: threading.Event | None,
) -> int:
    if cancel_event is None:
        try:
            return int(await asyncio.wait_for(proc.wait(), timeout=timeout_seconds))
        except (asyncio.TimeoutError, TimeoutError) as exc:
            raise subprocess.TimeoutExpired(command, timeout_seconds) from exc

    loop = asyncio.get_running_loop()
    deadline = None if timeout_seconds is None else loop.time() + float(timeout_seconds)
    while True:
        if cancel_event.is_set():
            kill_process_group(proc.pid, signal.SIGTERM)
            try:
                return int(
                    await asyncio.wait_for(
                        proc.wait(), timeout=CANCEL_SIGKILL_GRACE_S
                    )
                )
            except (asyncio.TimeoutError, TimeoutError):
                kill_process_group(proc.pid, signal.SIGKILL)
                with contextlib.suppress(asyncio.TimeoutError, TimeoutError):
                    return int(
                        await asyncio.wait_for(
                            proc.wait(), timeout=CANCEL_SIGKILL_GRACE_S
                        )
                    )
                return -int(signal.SIGKILL)
        if proc.returncode is not None:
            return int(proc.returncode)
        if deadline is not None and loop.time() > deadline:
            raise subprocess.TimeoutExpired(command, timeout_seconds)
        await asyncio.sleep(CANCEL_POLL_INTERVAL_S)


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


def _unshare_path() -> str:
    return shutil.which("unshare") or ""


__all__ = [
    "TOOL_CALL_COMMAND_POLICY",
    "detect_private_mount_namespace",
    "run_in_namespace",
]
