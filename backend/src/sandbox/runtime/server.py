"""Generic in-sandbox runtime dispatcher.

Host-to-guest contract: callers send a JSON object such as
``{"op": "overlay.run", "args": {...}}`` through argv[0] or stdin. Handlers
return JSON-safe values or dataclasses matching the public result types from
the sandbox API refactor plan. stdout receives that JSON result directly.

This dispatcher emits direct handler results rather than a nested command
envelope. Host callers use
``sandbox.runtime.command_client`` and receive handler results directly.
"""

from __future__ import annotations

import asyncio
import dataclasses
import inspect
import json
import sys
import traceback
from collections.abc import Callable, Mapping
from types import SimpleNamespace
from typing import Any

Handler = Callable[[dict[str, Any]], Any]

OP_TABLE: dict[str, Handler] = {}


def register_op(op: str, handler: Handler) -> None:
    """Register a runtime operation handler.

    Peer bootstrap modules call this at import time. Dispatch remains a table
    lookup; peer-specific branching belongs in peer handlers or pipelines.
    """
    if not isinstance(op, str) or not op:
        raise ValueError("op must be a non-empty string")
    if op in OP_TABLE:
        raise ValueError(f"runtime op already registered: {op}")
    OP_TABLE[op] = handler


def dispatch_json(raw: str) -> dict[str, Any]:
    """Decode and dispatch one JSON envelope."""
    try:
        envelope = json.loads(raw)
    except json.JSONDecodeError as exc:
        return _error(
            "bad_json",
            "runtime request must be valid JSON",
            {"message": str(exc)},
        )
    if not isinstance(envelope, Mapping):
        return _error("invalid_envelope", "runtime request must be a JSON object")
    return dispatch_envelope(envelope)


def dispatch_envelope(envelope: Mapping[str, Any]) -> dict[str, Any]:
    """Dispatch an already-decoded runtime envelope."""
    op = envelope.get("op")
    if not isinstance(op, str) or not op:
        return _error(
            "invalid_envelope",
            "runtime envelope requires a non-empty string op",
        )

    args_raw = envelope.get("args", {})
    if args_raw is None:
        args_raw = {}
    if not isinstance(args_raw, dict):
        return _error(
            "invalid_envelope",
            "runtime envelope args must be a JSON object",
            {"op": op},
        )

    handler = OP_TABLE.get(op)
    if handler is None:
        return _error("unknown_op", f"unknown op: {op}", {"op": op})

    try:
        result = handler(dict(args_raw))
        if inspect.isawaitable(result):
            result = asyncio.run(result)
        return _to_jsonable(result)
    except Exception as exc:
        return _error(
            "internal_error",
            str(exc),
            {"op": op, "traceback": traceback.format_exc()},
        )


def main(argv: list[str] | None = None) -> int:
    """CLI entrypoint for the deployed runtime script."""
    args = list(sys.argv[1:] if argv is None else argv)
    raw = args[0] if args else sys.stdin.read()
    response = dispatch_json(raw)
    sys.stdout.write(json.dumps(response, separators=(",", ":")))
    sys.stdout.write("\n")
    return 1 if "error" in response else 0


def _error(
    kind: str,
    message: str,
    details: dict[str, Any] | None = None,
) -> dict[str, Any]:
    return {
        "success": False,
        "warnings": [],
        "timings": {},
        "error": {
            "kind": kind,
            "message": message,
            "details": details or {},
        },
    }


def _to_jsonable(obj: Any) -> Any:
    if dataclasses.is_dataclass(obj) and not isinstance(obj, type):
        return {k: _to_jsonable(v) for k, v in dataclasses.asdict(obj).items()}
    if isinstance(obj, SimpleNamespace):
        return {str(k): _to_jsonable(v) for k, v in vars(obj).items()}
    if isinstance(obj, (list, tuple)):
        return [_to_jsonable(v) for v in obj]
    if isinstance(obj, dict):
        return {str(k): _to_jsonable(v) for k, v in obj.items()}
    return obj


def _load_peer_bootstraps() -> None:
    from sandbox.overlay.handlers import run as overlay_run
    from sandbox.overlay.handlers import shell as overlay_shell

    for op, handler in {
        "overlay.run": overlay_run.handle,
        "shell": overlay_shell.handle,
    }.items():
        existing = OP_TABLE.get(op)
        if existing is handler:
            continue
        if existing is not None:
            raise ValueError(f"runtime op already registered: {op}")
        register_op(op, handler)


_load_peer_bootstraps()


__all__ = [
    "Handler",
    "OP_TABLE",
    "dispatch_envelope",
    "dispatch_json",
    "main",
    "register_op",
]
