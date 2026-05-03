"""Private host-side dispatch helper for bundled runtime-server calls."""

from __future__ import annotations

import json
import shlex
from typing import Any, Protocol

from sandbox.runtime.bundle import BUNDLE_REMOTE_DIR


class RuntimeDispatchError(RuntimeError):
    """Raised when a runtime-server call fails before typed decoding."""

    def __init__(
        self,
        kind: str,
        message: str,
        details: dict[str, Any] | None = None,
    ) -> None:
        super().__init__(f"{kind}: {message}")
        self.kind = kind
        self.message = message
        self.details = details or {}


class RuntimeExec(Protocol):
    async def __call__(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> Any: ...


async def call_runtime_server(
    *,
    exec_fn: RuntimeExec,
    sandbox_id: str,
    op: str,
    args: dict[str, Any],
    cwd: str = BUNDLE_REMOTE_DIR,
    timeout: int | None = None,
) -> dict[str, Any]:
    """Dispatch one JSON envelope through ``python -m sandbox.runtime.server``."""
    raw_payload = json.dumps(
        {"op": op, "args": _without_none(args)},
        separators=(",", ":"),
    )
    result = await exec_fn(
        sandbox_id,
        f"python3 -m sandbox.runtime.server {shlex.quote(raw_payload)}",
        cwd=cwd,
        timeout=timeout,
    )
    try:
        response = _decode_response(getattr(result, "stdout", ""))
    except RuntimeDispatchError:
        if getattr(result, "exit_code", 1) != 0:
            _raise_exec_failed(result)
        raise
    if "error" in response:
        error = response.get("error") or {}
        raise RuntimeDispatchError(
            kind=str(error.get("kind") or "RuntimeError"),
            message=str(error.get("message") or ""),
            details=error.get("details") if isinstance(error.get("details"), dict) else {},
        )
    if getattr(result, "exit_code", 1) != 0:
        _raise_exec_failed(result)
    return response


def _without_none(args: dict[str, Any]) -> dict[str, Any]:
    return {key: value for key, value in args.items() if value is not None}


def _decode_response(stdout: str) -> dict[str, Any]:
    try:
        decoded = json.loads((stdout or "").strip())
    except json.JSONDecodeError as exc:
        raise RuntimeDispatchError(
            "BadRuntimeResponse",
            "runtime server returned invalid JSON",
            {"stdout": stdout},
        ) from exc
    if not isinstance(decoded, dict):
        raise RuntimeDispatchError(
            "BadRuntimeResponse",
            "runtime server returned a non-object JSON response",
            {"response": decoded},
        )
    return decoded


def _raise_exec_failed(result: Any) -> None:
    exit_code = getattr(result, "exit_code", 1)
    raise RuntimeDispatchError(
        kind="RuntimeExecFailed",
        message=str(getattr(result, "stderr", "") or getattr(result, "stdout", "")),
        details={"exit_code": exit_code},
    )


__all__ = ["RuntimeDispatchError", "call_runtime_server"]
