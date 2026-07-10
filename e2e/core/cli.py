"""Thin sandbox CLI wrapper: run an operation, return its parsed JSON.

Operations are addressed with a leading *space* token — ``manager``,
``observability``, or ``runtime`` — which :func:`route_cli` maps to the right
purpose-built binary (``sandbox-manager-cli`` for manager,
``sandbox-runtime-cli`` for runtime, and ``sandbox-observability-cli`` for
observability) and rewrites into that binary's argv. This keeps every caller
and the timing classifier space-addressed while the three binaries stay
dependency-isolated.

Each CLI writes its result as a single JSON line — to stdout on success (exit
0) and to stderr on operation or usage errors (exit 1 or 2). We capture both
and parse whichever carries the JSON, so error responses come back as
``{"error": {...}}`` dicts rather than exceptions. Tests assert on the
structured result; they never read logs.

With ``E2E_PROGRESS=1`` we add the manager CLI's global ``--progress`` flag and
stream the daemon-side progress lines (e.g. workspace base copy/hash) live to
the ``e2e.cli`` logger as they arrive, while still parsing the final JSON line.
``--progress`` is manager-only, so runtime and observability operations never
stream.
"""

import json
import logging
import os
import socket
import subprocess
import threading
import time
import uuid
from pathlib import Path

from .config import (
    PROGRESS,
    REPO_ROOT,
    SANDBOX_MANAGER_CLI,
    SANDBOX_OBSERVABILITY_CLI,
    SANDBOX_RUNTIME_CLI,
)

_log = logging.getLogger("e2e.cli")
_timing_lock = threading.Lock()
_timing_records = []


class CliError(Exception):
    """The CLI produced output that was not a JSON line."""


class InternalGatewayResult:
    """Raw-result shape for daemon-internal E2E gateway calls."""

    def __init__(self, operation, args, response, elapsed_ms):
        self.args = [operation, args]
        self.json = response
        self.elapsed_ms = elapsed_ms
        self.returncode = 1 if is_error(response) else 0
        encoded = json.dumps(response, sort_keys=True)
        self.stdout = encoded if self.returncode == 0 else ""
        self.stderr = encoded if self.returncode != 0 else ""

    @property
    def ok(self):
        return self.returncode == 0


def route_cli(args):
    """Map space-prefixed ``args`` to ``(binary, argv, supports_progress)``.

    Each space routes to its independently grantable binary. Runtime keeps its
    global ``--sandbox-id …`` prefix; observability keeps ``--sandbox-id`` in
    the operation argv because aggregate ``snapshot`` may omit it.
    """
    argv = [str(arg) for arg in args]
    if not argv:
        return SANDBOX_MANAGER_CLI, [], True
    space, rest = argv[0], argv[1:]
    if space == "observability":
        return SANDBOX_OBSERVABILITY_CLI, rest, False
    if space == "runtime":
        return SANDBOX_RUNTIME_CLI, rest, False
    if space == "manager":
        return SANDBOX_MANAGER_CLI, rest, True
    return SANDBOX_MANAGER_CLI, argv, True


def cli(*args, timeout=180):
    """Run a space-addressed operation and return the parsed JSON response."""
    binary, argv, supports_progress = route_cli(args)
    printable = " ".join([binary.name, *argv])
    _log.info("→ %s", printable)
    started = time.monotonic()
    if PROGRESS and supports_progress:
        stdout, stderr_lines, returncode = _run_streaming(binary, argv, timeout)
        raw = _select_json(stdout, stderr_lines)
    else:
        proc = subprocess.run(
            [str(binary), *argv],
            cwd=str(REPO_ROOT),
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        returncode = proc.returncode
        raw = proc.stdout.strip() or proc.stderr.strip()
    elapsed = time.monotonic() - started
    _record_timing(args, returncode, elapsed)
    _log.info("← %s  (exit=%s, %.2fs)", printable, returncode, elapsed)
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        raise CliError(
            f"non-JSON CLI output (exit {returncode}): {raw!r}"
        ) from exc


def _run_streaming(binary, argv, timeout):
    """Run with --progress, streaming stderr (progress) live.

    Returns ``(stdout, stderr_lines, returncode)``.
    """
    cmd = [str(binary), "--progress", *argv]
    proc = subprocess.Popen(
        cmd,
        cwd=str(REPO_ROOT),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    stderr_lines = []

    def drain():
        for line in iter(proc.stderr.readline, ""):
            stripped = line.rstrip("\n")
            stderr_lines.append(stripped)
            if stripped:
                _log.info("  ‖ %s", stripped)

    drainer = threading.Thread(target=drain, daemon=True)
    drainer.start()

    timed_out = []
    watchdog = threading.Timer(timeout, lambda: (timed_out.append(True), proc.kill()))
    watchdog.start()
    try:
        stdout = proc.stdout.read()
        proc.wait()
    finally:
        watchdog.cancel()
        drainer.join(timeout=5)
    if timed_out:
        raise subprocess.TimeoutExpired(cmd, timeout)
    return stdout, stderr_lines, proc.returncode


def _select_json(stdout, stderr_lines):
    """Pick the final JSON line: stdout on success, else the last JSON-looking
    stderr line (progress lines are ``[progress …]``/``[Output]``, never JSON)."""
    out = stdout.strip()
    if out:
        return out
    for line in reversed(stderr_lines):
        stripped = line.strip()
        if stripped.startswith("{"):
            return stripped
    return ""


def _record_timing(args, returncode, elapsed):
    scope, operation = _classify_operation(args)
    with _timing_lock:
        _timing_records.append(
            {
                "scope": scope,
                "operation": operation,
                "operation_key": f"{scope}.{operation}",
                "returncode": returncode,
                "duration_ms": round(elapsed * 1000.0, 3),
            }
        )


def _classify_operation(args):
    args = tuple(map(str, args))
    if not args:
        return "unknown", "unknown"
    if args[0] == "runtime":
        return "runtime", args[3] if len(args) > 3 else "unknown"
    if args[0] in {"manager", "observability"}:
        return args[0], args[1] if len(args) > 1 else "unknown"
    return args[0], args[1] if len(args) > 1 else "unknown"


def operation_timing_records():
    with _timing_lock:
        return list(_timing_records)


def operation_timing_summary():
    grouped = {}
    for record in operation_timing_records():
        grouped.setdefault(record["operation_key"], []).append(record)

    rows = []
    for operation_key, records in grouped.items():
        values = sorted(record["duration_ms"] for record in records)
        sub_50 = _threshold_summary(values, 50.0)
        sub_100 = _threshold_summary(values, 100.0)
        sub_200 = _threshold_summary(values, 200.0)
        rows.append(
            {
                "operation": operation_key,
                "count": len(values),
                "min_ms": values[0],
                "p50_ms": round(_percentile(values, 0.50), 3),
                "p95_ms": round(_percentile(values, 0.95), 3),
                "max_ms": values[-1],
                "sub_50ms_count": sub_50["count"],
                "sub_50ms_pct": sub_50["pct"],
                "sub_100ms_count": sub_100["count"],
                "sub_100ms_pct": sub_100["pct"],
                "sub_200ms_count": sub_200["count"],
                "sub_200ms_pct": sub_200["pct"],
                "cli_error_count": sum(
                    1 for record in records if record["returncode"] != 0
                ),
            }
        )
    return sorted(rows, key=lambda row: row["operation"])


def _threshold_summary(values, threshold_ms):
    count = sum(1 for value in values if value < threshold_ms)
    return {"count": count, "pct": round((count / len(values)) * 100.0, 1)}


def _percentile(values, quantile):
    if len(values) == 1:
        return values[0]
    rank = (len(values) - 1) * quantile
    lower = int(rank)
    upper = min(lower + 1, len(values) - 1)
    weight = rank - lower
    return (values[lower] * (1.0 - weight)) + (values[upper] * weight)


def manager(operation, *args, **kwargs):
    """Run a manager-space operation."""
    return cli("manager", operation, *args, **kwargs)


def runtime(sandbox_id, operation, *args, **kwargs):
    """Run a runtime-space operation, routed to ``sandbox_id``.

    The ``--sandbox-id`` flag must precede the operation name.
    """
    return cli("runtime", "--sandbox-id", sandbox_id, operation, *args, **kwargs)


def internal_runtime(sandbox_id, operation, args=None, *, timeout=180):
    """Call a daemon-internal runtime operation through the authenticated gateway."""
    return internal_runtime_result(
        sandbox_id,
        operation,
        args,
        timeout=timeout,
    ).json


def internal_runtime_result(
    sandbox_id,
    operation,
    args=None,
    *,
    timeout=180,
    recorder=None,
):
    """Return a raw-result-shaped internal gateway response for report suites."""
    request_args = dict(args or {})
    request = {
        "op": operation,
        "request_id": f"e2e-internal-{uuid.uuid4()}",
        "scope": {"kind": "sandbox", "sandbox_id": sandbox_id},
        "args": request_args,
        "_stream_logs": False,
    }
    token = _gateway_auth_token()
    if token is not None:
        request["_sandbox_gateway_auth_token"] = token

    started = time.monotonic()
    host, port = _gateway_endpoint()
    with socket.create_connection((host, port), timeout=timeout) as stream:
        stream.settimeout(timeout)
        stream.sendall(json.dumps(request, separators=(",", ":")).encode() + b"\n")
        stream.shutdown(socket.SHUT_WR)
        with stream.makefile("rb") as reader:
            response_line = reader.readline()
    elapsed_ms = round((time.monotonic() - started) * 1000.0, 3)
    if not response_line.endswith(b"\n"):
        raise CliError("internal gateway response was not newline terminated")
    try:
        response = json.loads(response_line)
    except json.JSONDecodeError as exc:
        raise CliError(f"non-JSON internal gateway output: {response_line!r}") from exc

    result = InternalGatewayResult(operation, request_args, response, elapsed_ms)
    if recorder is not None:
        recorder.add_command(
            {
                "cmd": ["internal-runtime", operation, json.dumps(request_args, sort_keys=True)],
                "exit_code": result.returncode,
                "elapsed_ms": elapsed_ms,
                "stdout": result.stdout,
                "stderr": result.stderr,
                "parsed_json": response,
            }
        )
    return result


def _gateway_endpoint():
    address = os.environ.get("SANDBOX_GATEWAY_SOCKET", "127.0.0.1:7878")
    if address.startswith("["):
        host, separator, port = address[1:].partition("]:")
    else:
        host, separator, port = address.rpartition(":")
    if not separator or not host or not port:
        raise CliError(f"invalid SANDBOX_GATEWAY_SOCKET: {address!r}")
    try:
        return host, int(port)
    except ValueError as exc:
        raise CliError(f"invalid SANDBOX_GATEWAY_SOCKET port: {address!r}") from exc


def _gateway_auth_token():
    token = os.environ.get("SANDBOX_GATEWAY_AUTH_TOKEN")
    if token:
        return token
    token_file = Path(os.environ.get("SANDBOX_GATEWAY_TOKEN_FILE", "/tmp/eos-gateway.token"))
    if not token_file.is_file():
        return None
    token = token_file.read_text(encoding="utf-8").strip()
    return token or None


def observability(operation, *args, **kwargs):
    """Run an observability-space operation."""
    return cli("observability", operation, *args, **kwargs)


def is_error(result):
    return isinstance(result, dict) and "error" in result
