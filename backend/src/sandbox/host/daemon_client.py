"""Provider-backed client for the bundled in-sandbox daemon dispatcher."""

from __future__ import annotations

import asyncio
import json
import logging
import shlex
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Protocol

from sandbox._shared.models import RawExecResult
from sandbox.daemon.paths import (
    BUNDLE_REMOTE_DIR,
    DAEMON_ENV_SIGNATURE_PATH,
    DAEMON_LAUNCH_SCRIPT_PATH,
    DAEMON_LOG_PATH,
    DAEMON_PID_PATH,
    DAEMON_SOCKET_PATH,
    DAEMON_THIN_CLIENT_PATH,
    DEFAULT_LAYER_STACK_ROOT,
)
from sandbox.host.runtime_bundle import bundle_hash
from sandbox.provider.registry import get_adapter

logger = logging.getLogger(__name__)

# Daemon spawned once per sandbox via provider.exec; subsequent calls hit the
# warm process via an AF_UNIX thin client (one envelope per call).
_DAEMON_SOCKET = DAEMON_SOCKET_PATH
_DAEMON_PID = DAEMON_PID_PATH
_DAEMON_LOG = DAEMON_LOG_PATH
_DAEMON_ENV = DAEMON_ENV_SIGNATURE_PATH
_PYTHON_CANDIDATES = ("python3.13", "python3.12", "python3.11", "python3.10", "python3")
_THIN_CLIENT_CONNECT_FAILED = 97
_THIN_CLIENT_IO_FAILED = 98
_DAEMON_SPAWN_TIMEOUT = 20
# Bounded retry on CONNECT_FAILED: under parallel agent load the daemon's
# accept queue can transiently refuse new connections immediately after spawn.
# These delays give the daemon time to bind/accept before declaring readiness
# a hard failure. Total worst-case added latency: ~3.5s before raising.
_CONNECT_RETRY_DELAYS_S: tuple[float, ...] = (0.25, 0.5, 1.0, 2.0)
DAEMON_PROTOCOL_VERSION = 1
DAEMON_PROTOCOL_FIELD = "_eos_daemon_protocol_version"
DAEMON_AUTH_FIELD = "_eos_daemon_auth_token"


@dataclass(frozen=True)
class _DaemonTcpEndpoint:
    host: str
    port: int
    internal_port: int | None
    auth_token: str


class _DaemonDispatchError(RuntimeError):
    """Raised when daemon dispatch fails before typed decoding."""

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


class _DaemonReadinessError(_DaemonDispatchError):
    """Raised when a relaunched daemon does not become ready."""


class _TcpConnectFailed(RuntimeError):
    """Raised when the host cannot connect to the daemon TCP endpoint."""


class _TcpIoFailed(RuntimeError):
    """Raised when an established daemon TCP stream fails."""


class _DaemonExec(Protocol):
    async def __call__(
        self,
        sandbox_id: str,
        command: str,
        *,
        cwd: str | None = None,
        timeout: int | None = None,
    ) -> Any: ...


async def _call_daemon(
    *,
    exec_fn: _DaemonExec,
    sandbox_id: str,
    op: str,
    args: dict[str, Any],
    cwd: str = BUNDLE_REMOTE_DIR,
    timeout: int | None = None,
    tcp_endpoint: _DaemonTcpEndpoint | None = None,
) -> dict[str, Any]:
    """Dispatch one JSON envelope to the resident in-sandbox daemon."""
    raw_payload = json.dumps(
        {"op": op, "args": _without_none(args)},
        separators=(",", ":"),
    )
    result = await _dispatch_once_with_retry(
        exec_fn=exec_fn,
        sandbox_id=sandbox_id,
        op=op,
        args=_without_none(args),
        raw_payload=raw_payload,
        cwd=cwd,
        timeout=timeout,
        tcp_endpoint=tcp_endpoint,
    )
    try:
        response = _decode_response(getattr(result, "stdout", ""))
    except _DaemonDispatchError:
        if _exit_code(result) != 0:
            _raise_exec_failed(result)
        raise
    if "error" in response:
        error = response.get("error") or {}
        raise _DaemonDispatchError(
            kind=str(error.get("kind") or "RuntimeError"),
            message=str(error.get("message") or ""),
            details=error.get("details") if isinstance(error.get("details"), dict) else {},
        )
    if _exit_code(result) != 0:
        _raise_exec_failed(result)
    return response


async def call_daemon_api(
    sandbox_id: str,
    op: str,
    args: dict[str, Any],
    *,
    timeout: int = 60,
    layer_stack_root: str = DEFAULT_LAYER_STACK_ROOT,
) -> dict[str, Any]:
    """Call one guarded API operation inside the preinstalled daemon bundle."""
    daemon_args = {
        "layer_stack_root": layer_stack_root,
        **args,
    }
    adapter = get_adapter(sandbox_id)
    tcp_endpoint = await _resolve_daemon_tcp_endpoint(adapter, sandbox_id)
    return await _call_daemon(
        exec_fn=adapter.exec,
        sandbox_id=sandbox_id,
        op=op,
        args=daemon_args,
        timeout=timeout,
        tcp_endpoint=tcp_endpoint,
    )


def versioned_payload(payload: Mapping[str, object]) -> dict[str, object]:
    """Attach the daemon protocol version while preserving caller payloads."""
    return {
        DAEMON_PROTOCOL_FIELD: DAEMON_PROTOCOL_VERSION,
        **dict(payload),
    }


async def ensure_daemon_current(
    sandbox_id: str,
    *,
    timeout: int = _DAEMON_SPAWN_TIMEOUT,
) -> None:
    """Ensure the resident daemon is running for the current runtime bundle."""
    adapter = get_adapter(sandbox_id)
    tcp_endpoint = await _resolve_daemon_tcp_endpoint(adapter, sandbox_id)
    result = await adapter.exec(
        sandbox_id,
        _daemon_spawn_command(tcp_endpoint=tcp_endpoint),
        cwd=BUNDLE_REMOTE_DIR,
        timeout=timeout,
    )
    if _exit_code(result) != 0:
        _raise_exec_failed(result)


# TCP endpoint per sandbox is fixed once the container is up (host port binding
# does not change for a running container). The docker resolver runs two HTTP
# round-trips (`containers.get()` + `container.reload()`); without caching we
# pay that cost on every tool dispatch. The cache is invalidated on TCP
# CONNECT_FAILED (stale port mapping after a container restart) and after
# ``ensure_daemon_current`` (defensive, in case the daemon was respawned).
_tcp_endpoint_cache: dict[str, _DaemonTcpEndpoint | None] = {}
_tcp_endpoint_cache_locks: dict[str, asyncio.Lock] = {}


def invalidate_daemon_tcp_endpoint(sandbox_id: str) -> None:
    """Drop the cached TCP endpoint for ``sandbox_id``; next call re-resolves."""
    _tcp_endpoint_cache.pop(sandbox_id, None)


async def _resolve_daemon_tcp_endpoint(
    adapter: Any,
    sandbox_id: str,
) -> _DaemonTcpEndpoint | None:
    if sandbox_id in _tcp_endpoint_cache:
        return _tcp_endpoint_cache[sandbox_id]
    lock = _tcp_endpoint_cache_locks.setdefault(sandbox_id, asyncio.Lock())
    async with lock:
        if sandbox_id in _tcp_endpoint_cache:
            return _tcp_endpoint_cache[sandbox_id]
        resolver = getattr(adapter, "get_daemon_tcp_endpoint", None)
        if not callable(resolver):
            _tcp_endpoint_cache[sandbox_id] = None
            return None
        try:
            raw_endpoint = await asyncio.to_thread(resolver, sandbox_id)
        except Exception:
            logger.debug(
                "daemon TCP endpoint resolution failed for sandbox %s",
                sandbox_id,
                exc_info=True,
            )
            return None
        endpoint = _normalize_daemon_tcp_endpoint(raw_endpoint)
        _tcp_endpoint_cache[sandbox_id] = endpoint
        return endpoint


def _normalize_daemon_tcp_endpoint(raw: Any) -> _DaemonTcpEndpoint | None:
    if not isinstance(raw, Mapping):
        return None
    host = str(raw.get("host") or "127.0.0.1").strip()
    if not host:
        return None
    try:
        port = int(raw.get("port") or 0)
    except (TypeError, ValueError):
        return None
    if port <= 0:
        return None
    internal_port: int | None
    try:
        raw_internal_port = raw.get("internal_port")
        internal_port = int(raw_internal_port) if raw_internal_port is not None else None
    except (TypeError, ValueError):
        internal_port = None
    return _DaemonTcpEndpoint(
        host=host,
        port=port,
        internal_port=internal_port,
        auth_token=str(raw.get("auth_token") or ""),
    )


async def _dispatch_once_with_retry(
    *,
    exec_fn: _DaemonExec,
    sandbox_id: str,
    op: str,
    args: dict[str, Any],
    raw_payload: str,
    cwd: str,
    timeout: int | None,
    tcp_endpoint: _DaemonTcpEndpoint | None,
) -> Any:
    result = await _call_daemon_payload(
        exec_fn=exec_fn,
        sandbox_id=sandbox_id,
        payload=raw_payload,
        cwd=cwd,
        timeout=timeout,
        tcp_endpoint=tcp_endpoint,
    )
    if _exit_code(result) != _THIN_CLIENT_CONNECT_FAILED:
        return result

    spawn_result = await exec_fn(
        sandbox_id,
        _daemon_spawn_command(tcp_endpoint=tcp_endpoint),
        cwd=cwd,
        timeout=_DAEMON_SPAWN_TIMEOUT,
    )
    if _exit_code(spawn_result) != 0:
        return spawn_result

    layer_stack_root = args.get("layer_stack_root")
    if not str(layer_stack_root or "").strip():
        raise _DaemonReadinessError(
            kind="MissingLayerStackRoot",
            message="daemon readiness check requires layer_stack_root",
            details={"op": op},
        )

    readiness_payload = json.dumps(
        {
            "op": "api.runtime.ready",
            "args": {"layer_stack_root": str(layer_stack_root)},
        },
        separators=(",", ":"),
    )
    readiness_result = await _call_thin_client_with_connect_retry(
        exec_fn=exec_fn,
        sandbox_id=sandbox_id,
        payload=readiness_payload,
        cwd=cwd,
        timeout=30,
        tcp_endpoint=tcp_endpoint,
    )
    if _exit_code(readiness_result) != 0:
        raise _DaemonReadinessError(
            kind="RuntimeReadinessFailed",
            message=str(
                getattr(readiness_result, "stderr", "")
                or getattr(readiness_result, "stdout", "")
            ),
            details={"exit_code": _exit_code(readiness_result), "original_op": op},
        )
    try:
        response = _decode_response(getattr(readiness_result, "stdout", ""))
    except _DaemonDispatchError as exc:
        raise _DaemonReadinessError(
            kind="BadRuntimeReadinessResponse",
            message=exc.message,
            details={**exc.details, "original_op": op},
        ) from exc
    if "error" in response:
        error = response.get("error") or {}
        raise _DaemonReadinessError(
            kind=str(error.get("kind") or "RuntimeReadinessFailed"),
            message=str(error.get("message") or ""),
            details={
                **(
                    error.get("details")
                    if isinstance(error.get("details"), dict)
                    else {}
                ),
                "original_op": op,
            },
        )
    if response.get("ready") is not True:
        if _is_bootstrap_ready_response(op, response):
            logger.warning(
                "daemon-readiness: declaring %s ready despite control_plane "
                "WorkspaceBindingError; original op will retry against an "
                "unbound workspace and its own error path will surface the "
                "binding failure if it persists",
                op,
            )
        else:
            raise _DaemonReadinessError(
                kind="RuntimeNotReady",
                message="daemon readiness check failed",
                details={"response": response, "original_op": op},
            )

    return await _call_thin_client_with_connect_retry(
        exec_fn=exec_fn,
        sandbox_id=sandbox_id,
        payload=raw_payload,
        cwd=cwd,
        timeout=timeout,
        tcp_endpoint=tcp_endpoint,
    )


async def _call_thin_client_with_connect_retry(
    *,
    exec_fn: _DaemonExec,
    sandbox_id: str,
    payload: str,
    cwd: str,
    timeout: int | None,
    tcp_endpoint: _DaemonTcpEndpoint | None = None,
) -> Any:
    """Dispatch one envelope, retrying transient CONNECT_FAILED responses.

    The in-sandbox daemon's accept queue can transiently refuse connections
    immediately after spawn, or while many parallel agent runs land on the
    socket at once. A bounded backoff retry absorbs that without surfacing a
    user-visible tool failure.
    """
    last_result: Any = None
    for delay in _CONNECT_RETRY_DELAYS_S:
        last_result = await _call_daemon_payload(
            exec_fn=exec_fn,
            sandbox_id=sandbox_id,
            payload=payload,
            cwd=cwd,
            timeout=timeout,
            tcp_endpoint=tcp_endpoint,
        )
        if _exit_code(last_result) != _THIN_CLIENT_CONNECT_FAILED:
            return last_result
        await asyncio.sleep(delay)
    return await _call_daemon_payload(
        exec_fn=exec_fn,
        sandbox_id=sandbox_id,
        payload=payload,
        cwd=cwd,
        timeout=timeout,
        tcp_endpoint=tcp_endpoint,
    )


async def _call_daemon_payload(
    *,
    exec_fn: _DaemonExec,
    sandbox_id: str,
    payload: str,
    cwd: str,
    timeout: int | None,
    tcp_endpoint: _DaemonTcpEndpoint | None,
) -> Any:
    if tcp_endpoint is not None:
        tcp_result = await _call_tcp_daemon(tcp_endpoint, payload, timeout=timeout)
        if _exit_code(tcp_result) != _THIN_CLIENT_CONNECT_FAILED:
            return tcp_result
        # Cached endpoint produced CONNECT_FAILED — drop it so the next call
        # re-resolves the (possibly remapped) host port via the docker adapter.
        invalidate_daemon_tcp_endpoint(sandbox_id)
    return await exec_fn(
        sandbox_id,
        _daemon_thin_client_command(payload),
        cwd=cwd,
        timeout=timeout,
    )


async def _call_tcp_daemon(
    endpoint: _DaemonTcpEndpoint,
    payload: str,
    *,
    timeout: int | None,
) -> RawExecResult:
    client_timeout = float(timeout if timeout is not None else 60)
    try:
        stdout = await asyncio.wait_for(
            _call_tcp_daemon_inner(endpoint, _authenticated_payload(payload, endpoint)),
            timeout=client_timeout,
        )
        if not stdout.strip():
            return RawExecResult(
                success=False,
                exit_code=_THIN_CLIENT_CONNECT_FAILED,
                stdout="",
                stderr="EOS_DAEMON_CONNECT_FAILED:empty_response",
            )
    except _TcpConnectFailed as exc:
        cause = exc.__cause__ or exc
        return RawExecResult(
            success=False,
            exit_code=_THIN_CLIENT_CONNECT_FAILED,
            stdout="",
            stderr=f"EOS_DAEMON_CONNECT_FAILED:{cause.__class__.__name__}",
        )
    except _TcpIoFailed as exc:
        cause = exc.__cause__ or exc
        return RawExecResult(
            success=False,
            exit_code=_THIN_CLIENT_IO_FAILED,
            stdout="",
            stderr=f"EOS_DAEMON_IO_FAILED:{cause.__class__.__name__}",
        )
    except asyncio.TimeoutError:
        return RawExecResult(
            success=False,
            exit_code=_THIN_CLIENT_IO_FAILED,
            stdout="",
            stderr="EOS_DAEMON_IO_FAILED:asyncio.TimeoutError",
        )
    return RawExecResult(success=True, exit_code=0, stdout=stdout, stderr="")


async def _call_tcp_daemon_inner(
    endpoint: _DaemonTcpEndpoint,
    payload: str,
) -> str:
    try:
        reader, writer = await asyncio.open_connection(endpoint.host, endpoint.port)
    except OSError as exc:
        raise _TcpConnectFailed(exc) from exc
    try:
        writer.write(payload.encode("utf-8") + b"\n")
        if writer.can_write_eof():
            writer.write_eof()
        await writer.drain()
        chunks: list[bytes] = []
        while True:
            chunk = await reader.read(65536)
            if not chunk:
                break
            chunks.append(chunk)
        return b"".join(chunks).decode("utf-8", errors="replace")
    except OSError as exc:
        raise _TcpIoFailed(exc) from exc
    finally:
        writer.close()
        try:
            await writer.wait_closed()
        except Exception:
            logger.debug("daemon TCP writer.close failed", exc_info=True)


def _is_bootstrap_ready_response(
    original_op: str,
    response: dict[str, Any],
) -> bool:
    if original_op not in {"api.ensure_workspace_base", "api.build_workspace_base"}:
        return False
    probes = response.get("probes")
    if not isinstance(probes, list):
        return False
    by_name = {
        str(probe.get("name")): probe
        for probe in probes
        if isinstance(probe, dict)
    }
    control_plane = by_name.get("control_plane")
    if not isinstance(control_plane, dict):
        return False
    details = control_plane.get("details")
    if not isinstance(details, dict):
        return False
    if (
        control_plane.get("status") != "down"
        or details.get("error_type") != "WorkspaceBindingError"
    ):
        return False
    return all(
        isinstance(probe, dict) and probe.get("status") == "ok"
        for name, probe in by_name.items()
        if name != "control_plane"
    )


def _daemon_thin_client_command(raw_payload: str) -> str:
    """Launch the bundled thin client with one daemon envelope."""
    return (
        f"sh -c {shlex.quote(_thin_client_python_launcher())} daemon "
        f"{shlex.quote(_python_candidates_arg())} "
        f"{shlex.quote(DAEMON_THIN_CLIENT_PATH)} "
        f"{shlex.quote(_DAEMON_SOCKET)} "
        f"{shlex.quote(raw_payload)}"
    )


def _daemon_spawn_command(
    tcp_endpoint: _DaemonTcpEndpoint | None = None,
) -> str:
    """Launch the bundled daemon supervisor. Idempotent: returns 0 when
    an existing daemon's socket is bound and its PID is alive."""
    return " ".join(
        shlex.quote(part)
        for part in (
            "sh",
            DAEMON_LAUNCH_SCRIPT_PATH,
            _python_candidates_arg(),
            _DAEMON_SOCKET,
            _DAEMON_PID,
            _DAEMON_LOG,
            _DAEMON_ENV,
            _daemon_env_signature(tcp_endpoint=tcp_endpoint),
            "sandbox.daemon",
        )
    )


def _thin_client_python_launcher() -> str:
    return """\
candidates=$1
script=$2
socket_path=$3
payload=$4
for py in $candidates; do
    if command -v "$py" >/dev/null 2>&1 && "$py" -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 10) else 1)' >/dev/null 2>&1; then
        exec "$py" "$script" "$socket_path" "$payload"
    fi
done
echo 'sandbox daemon requires Python >= 3.10' >&2
exit 127
"""


def _daemon_env_signature(
    *,
    tcp_endpoint: _DaemonTcpEndpoint | None = None,
) -> str:
    parts = [f"runtime_bundle_sha={bundle_hash()}"]
    if tcp_endpoint is not None:
        tcp_port = tcp_endpoint.internal_port or tcp_endpoint.port
        parts.append(f"daemon_tcp_port={tcp_port}")
    return ";".join(parts)


def _python_candidates_arg() -> str:
    return " ".join(_PYTHON_CANDIDATES)


def _without_none(args: dict[str, Any]) -> dict[str, Any]:
    return {key: value for key, value in args.items() if value is not None}


def _authenticated_payload(
    payload: str,
    endpoint: _DaemonTcpEndpoint,
) -> str:
    if not endpoint.auth_token:
        return payload
    envelope = json.loads(payload)
    if not isinstance(envelope, dict):
        return payload
    envelope[DAEMON_AUTH_FIELD] = endpoint.auth_token
    return json.dumps(envelope, separators=(",", ":"))


def _decode_response(stdout: str) -> dict[str, Any]:
    try:
        decoded = json.loads((stdout or "").strip())
    except json.JSONDecodeError as exc:
        raise _DaemonDispatchError(
            "BadRuntimeResponse",
            "daemon returned invalid JSON",
            {"stdout": stdout},
        ) from exc
    if not isinstance(decoded, dict):
        raise _DaemonDispatchError(
            "BadRuntimeResponse",
            "daemon returned a non-object JSON response",
            {"response": decoded},
        )
    return decoded


def _raise_exec_failed(result: Any) -> None:
    exit_code = _exit_code(result)
    raise _DaemonDispatchError(
        kind="RuntimeExecFailed",
        message=str(getattr(result, "stderr", "") or getattr(result, "stdout", "")),
        details={"exit_code": exit_code},
    )


def _exit_code(result: Any) -> int:
    raw = getattr(result, "exit_code", None)
    if raw is None:
        raise _DaemonDispatchError(
            kind="BadExecResult",
            message="provider exec result is missing exit_code",
            details={"result_type": type(result).__name__},
        )
    try:
        return int(raw)
    except (TypeError, ValueError) as exc:
        raise _DaemonDispatchError(
            kind="BadExecResult",
            message=f"provider exec result has invalid exit_code: {raw!r}",
            details={"result_type": type(result).__name__},
        ) from exc


__all__: list[str] = []
