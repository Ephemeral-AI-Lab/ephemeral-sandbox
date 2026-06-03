"""Provider-backed client for the bundled in-sandbox daemon dispatcher."""

from __future__ import annotations

import asyncio
import json
import logging
import os
import shlex
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Protocol
from uuid import uuid4

from sandbox._shared.models import RawExecResult
from sandbox.host.paths import (
    BUNDLE_REMOTE_DIR,
    DAEMON_ENV_SIGNATURE_PATH,
    DAEMON_LOG_PATH,
    DAEMON_PID_PATH,
    DAEMON_SOCKET_PATH,
    DEFAULT_LAYER_STACK_ROOT,
    EOSD_REMOTE_PATH,
    EOSD_SHA_MARKER,
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
_EOSD_REMOTE_PATH = EOSD_REMOTE_PATH
_EOSD_SHA_MARKER = EOSD_SHA_MARKER
_THIN_CLIENT_CONNECT_FAILED = 97
_THIN_CLIENT_IO_FAILED = 98
_EMPTY_RESPONSE_MESSAGE = "EOS_DAEMON_IO_FAILED:empty_response"
_DAEMON_SPAWN_TIMEOUT = 20
# Bounded retry on CONNECT_FAILED: under parallel agent load the daemon's
# accept queue can transiently refuse new connections immediately after spawn.
# These delays give the daemon time to bind/accept before declaring readiness
# a hard failure. Total worst-case added latency: ~3.5s before raising.
_CONNECT_RETRY_DELAYS_S: tuple[float, ...] = (0.25, 0.5, 1.0, 2.0)
DAEMON_PROTOCOL_VERSION = 1
DAEMON_PROTOCOL_FIELD = "_eos_daemon_protocol_version"
DAEMON_AUTH_FIELD = "_eos_daemon_auth_token"
SANDBOX_RUNTIME_ENV = "EOS_SANDBOX_RUNTIME"
_SUPPORTED_SANDBOX_RUNTIMES = frozenset({"rust"})


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
    clean_args = _without_none(args)
    if op == "api.v1.cancel":
        invocation_id = uuid4().hex
    else:
        invocation_id = str(clean_args.get("invocation_id") or uuid4().hex)
        clean_args["invocation_id"] = invocation_id
    envelope_json = json.dumps(
        {"op": op, "invocation_id": invocation_id, "args": clean_args},
        separators=(",", ":"),
    )
    result = await _dispatch_with_daemon_spawn_recovery(
        exec_fn=exec_fn,
        sandbox_id=sandbox_id,
        op=op,
        args=clean_args,
        envelope_json=envelope_json,
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
    error = response.get("error")
    if error is not None and not _is_handler_level_error_result(response):
        if not isinstance(error, dict):
            raise _DaemonDispatchError(
                kind="RuntimeError",
                message=str(error),
                details={},
            )
        raise _DaemonDispatchError(
            kind=str(error.get("kind") or "RuntimeError"),
            message=str(error.get("message") or ""),
            details=error.get("details") if isinstance(error.get("details"), dict) else {},
        )
    if _exit_code(result) != 0:
        _raise_exec_failed(result)
    return response


def _is_handler_level_error_result(response: Mapping[str, Any]) -> bool:
    """Return true for handler-level policy results, not daemon transport errors."""
    return (
        response.get("success") is False
        and isinstance(response.get("status"), str)
        and bool(str(response.get("status")).strip())
    )


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


def with_daemon_protocol_version(payload: Mapping[str, object]) -> dict[str, object]:
    """Attach the daemon protocol version while preserving caller payloads."""
    return {
        DAEMON_PROTOCOL_FIELD: DAEMON_PROTOCOL_VERSION,
        **dict(payload),
    }


def selected_sandbox_runtime() -> str:
    """Return the requested sandbox runtime.

    The Python sandbox daemon has been retired; ``rust`` is the only supported
    daemon-side runtime.
    """
    runtime = os.environ.get(SANDBOX_RUNTIME_ENV, "rust").strip().lower() or "rust"
    if runtime not in _SUPPORTED_SANDBOX_RUNTIMES:
        supported = ", ".join(sorted(_SUPPORTED_SANDBOX_RUNTIMES))
        raise ValueError(f"{SANDBOX_RUNTIME_ENV} must be one of: {supported}")
    return runtime


async def ensure_daemon_current(
    sandbox_id: str,
    *,
    timeout: int = _DAEMON_SPAWN_TIMEOUT,
) -> None:
    """Ensure the resident daemon is running for the current runtime bundle."""
    selected_sandbox_runtime()
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


async def _dispatch_with_daemon_spawn_recovery(
    *,
    exec_fn: _DaemonExec,
    sandbox_id: str,
    op: str,
    args: dict[str, Any],
    envelope_json: str,
    cwd: str,
    timeout: int | None,
    tcp_endpoint: _DaemonTcpEndpoint | None,
) -> Any:
    result = await _send_daemon_envelope(
        exec_fn=exec_fn,
        sandbox_id=sandbox_id,
        envelope_json=envelope_json,
        cwd=cwd,
        timeout=timeout,
        tcp_endpoint=tcp_endpoint,
    )
    if _exit_code(result) != _THIN_CLIENT_CONNECT_FAILED and not (
        _is_empty_response(result) and _can_retry_empty_response(op)
    ):
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

    readiness_envelope_json = json.dumps(
        {
            "op": "api.runtime.ready",
            "invocation_id": uuid4().hex,
            "args": {"layer_stack_root": str(layer_stack_root)},
        },
        separators=(",", ":"),
    )
    readiness_result = await _call_daemon_envelope_with_connect_retry(
        exec_fn=exec_fn,
        sandbox_id=sandbox_id,
        envelope_json=readiness_envelope_json,
        cwd=cwd,
        timeout=30,
        tcp_endpoint=tcp_endpoint,
        retry_empty_response=True,
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
    error = response.get("error")
    if error is not None:
        if not isinstance(error, dict):
            raise _DaemonReadinessError(
                kind="RuntimeReadinessFailed",
                message=str(error),
                details={"original_op": op},
            )
        details_raw = error.get("details")
        details = dict(details_raw) if isinstance(details_raw, dict) else {}
        details["original_op"] = op
        raise _DaemonReadinessError(
            kind=str(error.get("kind") or "RuntimeReadinessFailed"),
            message=str(error.get("message") or ""),
            details=details,
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

    return await _call_daemon_envelope_with_connect_retry(
        exec_fn=exec_fn,
        sandbox_id=sandbox_id,
        envelope_json=envelope_json,
        cwd=cwd,
        timeout=timeout,
        tcp_endpoint=tcp_endpoint,
        retry_empty_response=_can_retry_empty_response(op),
    )


async def _call_daemon_envelope_with_connect_retry(
    *,
    exec_fn: _DaemonExec,
    sandbox_id: str,
    envelope_json: str,
    cwd: str,
    timeout: int | None,
    tcp_endpoint: _DaemonTcpEndpoint | None = None,
    retry_empty_response: bool = False,
) -> Any:
    """Dispatch one envelope, retrying transient connection failures.

    The in-sandbox daemon's accept queue can transiently refuse connections
    immediately after spawn, or while many parallel agent runs land on the
    socket at once. Docker's forwarded TCP path can also briefly connect and
    then close without a response after a hard daemon kill/rebind. A bounded
    backoff retry absorbs those failures for readiness and explicitly
    retryable read/control operations.
    """
    last_result: Any = None
    for delay in _CONNECT_RETRY_DELAYS_S:
        last_result = await _send_daemon_envelope(
            exec_fn=exec_fn,
            sandbox_id=sandbox_id,
            envelope_json=envelope_json,
            cwd=cwd,
            timeout=timeout,
            tcp_endpoint=tcp_endpoint,
        )
        if _exit_code(last_result) != _THIN_CLIENT_CONNECT_FAILED and not (
            retry_empty_response and _is_empty_response(last_result)
        ):
            return last_result
        await asyncio.sleep(delay)
    return await _send_daemon_envelope(
        exec_fn=exec_fn,
        sandbox_id=sandbox_id,
        envelope_json=envelope_json,
        cwd=cwd,
        timeout=timeout,
        tcp_endpoint=tcp_endpoint,
    )


async def _send_daemon_envelope(
    *,
    exec_fn: _DaemonExec,
    sandbox_id: str,
    envelope_json: str,
    cwd: str,
    timeout: int | None,
    tcp_endpoint: _DaemonTcpEndpoint | None,
) -> Any:
    if tcp_endpoint is not None:
        tcp_result = await _call_tcp_daemon(tcp_endpoint, envelope_json, timeout=timeout)
        if _is_empty_response(tcp_result):
            invalidate_daemon_tcp_endpoint(sandbox_id)
            return tcp_result
        if _exit_code(tcp_result) != _THIN_CLIENT_CONNECT_FAILED:
            return tcp_result
        # Cached endpoint produced CONNECT_FAILED — drop it so the next call
        # re-resolves the (possibly remapped) host port via the docker adapter.
        invalidate_daemon_tcp_endpoint(sandbox_id)
    return await exec_fn(
        sandbox_id,
        _daemon_thin_client_command(envelope_json),
        cwd=cwd,
        timeout=timeout,
    )


async def _call_tcp_daemon(
    endpoint: _DaemonTcpEndpoint,
    envelope_json: str,
    *,
    timeout: int | None,
) -> RawExecResult:
    client_timeout = float(timeout if timeout is not None else 60)
    try:
        stdout = await asyncio.wait_for(
            _call_tcp_daemon_inner(
                endpoint,
                _authenticated_envelope_json(envelope_json, endpoint),
            ),
            timeout=client_timeout,
        )
        if not stdout.strip():
            return RawExecResult(
                success=False,
                exit_code=_THIN_CLIENT_IO_FAILED,
                stdout="",
                stderr=_EMPTY_RESPONSE_MESSAGE,
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
    envelope_json: str,
) -> str:
    try:
        reader, writer = await asyncio.open_connection(endpoint.host, endpoint.port)
    except OSError as exc:
        raise _TcpConnectFailed(exc) from exc
    try:
        writer.write(envelope_json.encode("utf-8") + b"\n")
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


def _is_empty_response(result: Any) -> bool:
    return (
        _exit_code(result) == _THIN_CLIENT_IO_FAILED
        and str(getattr(result, "stderr", "")) == _EMPTY_RESPONSE_MESSAGE
    )


def _can_retry_empty_response(op: str) -> bool:
    """Retry only operations that cannot publish or mutate workspace content.

    Empty TCP output means the request reached a daemon process and then the
    process died or closed before returning a response. Replaying shell/write
    payloads after a daemon respawn can convert an isolated in-flight call into
    a default-mode publish, so mutation tools must fail closed. Lifecycle and
    control-plane calls still need retry to recover a stale daemon during setup.
    """
    return op not in {
        "api.edit_file",
        "api.v1.edit_file",
        "api.write_file",
        "api.v1.write_file",
        "api.v1.exec_command",
        "api.v1.write_stdin",
        "api.v1.command.write_stdin",
    } and not op.startswith("plugin.")


def _daemon_thin_client_command(envelope_json: str) -> str:
    """Launch ``eosd daemon --client`` with one daemon envelope."""
    selected_sandbox_runtime()
    return " ".join(
        shlex.quote(part)
        for part in (
            _EOSD_REMOTE_PATH,
            "daemon",
            "--client",
            _DAEMON_SOCKET,
            envelope_json,
        )
    )


def _daemon_spawn_command(
    tcp_endpoint: _DaemonTcpEndpoint | None = None,
) -> str:
    """Launch the bundled daemon supervisor. Idempotent: returns 0 when
    an existing daemon's socket is bound and its PID is alive.

    Sources ``/etc/environment`` so feature-flag env vars written there by
    the test fixture (e.g. ``EOS_ISOLATED_WORKSPACE_ENABLED=true``)
    propagate to the spawned daemon. ``docker exec`` uses a bare ``sh -c``
    by default which does NOT auto-source it; ``set -a`` exports every
    sourced variable so the daemon inherits them.
    """
    selected_sandbox_runtime()
    spawn_parts = [
        _EOSD_REMOTE_PATH,
        "daemon",
        "--spawn",
        "--socket",
        _DAEMON_SOCKET,
        "--pid-file",
        _DAEMON_PID,
        "--log-file",
        _DAEMON_LOG,
    ]
    if tcp_endpoint is not None:
        spawn_parts.extend(
            [
                "--tcp-host",
                "0.0.0.0",
                "--tcp-port",
                str(tcp_endpoint.internal_port or tcp_endpoint.port),
            ]
        )
        if tcp_endpoint.auth_token:
            spawn_parts.extend(["--auth-token", tcp_endpoint.auth_token])
    inner = _rust_daemon_spawn_shell(
        spawn_command=" ".join(shlex.quote(part) for part in spawn_parts),
        signature=_daemon_env_signature(tcp_endpoint=tcp_endpoint),
    )
    return (
        "if [ -r /etc/environment ]; then set -a; . /etc/environment; set +a; fi; "
        + inner
    )


def _rust_daemon_spawn_shell(*, spawn_command: str, signature: str) -> str:
    """Restart a resident daemon when the selected Rust runtime signature changes."""
    return " ".join(
        [
            f"daemon_env_sig={shlex.quote(signature)};",
            (
                f"if [ -f {shlex.quote(_EOSD_SHA_MARKER)} ]; then "
                f'daemon_env_sig="$daemon_env_sig;eosd_sha=$(cat {shlex.quote(_EOSD_SHA_MARKER)})"; '
                "fi;"
            ),
            (
                f"if [ -S {shlex.quote(_DAEMON_SOCKET)} ] && "
                f"[ -f {shlex.quote(_DAEMON_PID)} ]; then "
                f"if [ ! -f {shlex.quote(_DAEMON_ENV)} ] || "
                f"[ \"$(cat {shlex.quote(_DAEMON_ENV)})\" != \"$daemon_env_sig\" ]; then "
                f"daemon_pid=$(cat {shlex.quote(_DAEMON_PID)} 2>/dev/null || true); "
                'if [ -n "$daemon_pid" ]; then '
                'kill "$daemon_pid" 2>/dev/null || true; '
                'for _ in $(seq 1 50); do '
                'kill -0 "$daemon_pid" 2>/dev/null || break; '
                "sleep 0.02; "
                "done; "
                "fi; "
                f"rm -f {shlex.quote(_DAEMON_SOCKET)} {shlex.quote(_DAEMON_PID)}; "
                "fi; "
                "fi;"
            ),
            f"{spawn_command} && printf %s \"$daemon_env_sig\" > {shlex.quote(_DAEMON_ENV)}",
        ]
    )


def _daemon_env_signature(
    *,
    tcp_endpoint: _DaemonTcpEndpoint | None = None,
) -> str:
    parts = [
        f"sandbox_runtime={selected_sandbox_runtime()}",
        f"runtime_bundle_sha={bundle_hash()}",
    ]
    if tcp_endpoint is not None:
        tcp_port = tcp_endpoint.internal_port or tcp_endpoint.port
        parts.append(f"daemon_tcp_port={tcp_port}")
    return ";".join(parts)


def _without_none(args: dict[str, Any]) -> dict[str, Any]:
    return {key: value for key, value in args.items() if value is not None}


def _authenticated_envelope_json(
    envelope_json: str,
    endpoint: _DaemonTcpEndpoint,
) -> str:
    if not endpoint.auth_token:
        return envelope_json
    envelope = json.loads(envelope_json)
    if not isinstance(envelope, dict):
        return envelope_json
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
