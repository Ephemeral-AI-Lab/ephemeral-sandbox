"""Thin sandbox-cli wrapper: run an operation, return its parsed JSON.

The CLI writes its result as a single JSON line — to stdout on success (exit 0)
and to stderr on error (exit 1). We capture both and parse whichever carries the
JSON, so error responses come back as ``{"error": {...}}`` dicts rather than
exceptions. Tests assert on the structured result; they never read logs.

With ``E2E_PROGRESS=1`` we add the CLI's global ``--progress`` flag and stream
the daemon-side progress lines (e.g. workspace base copy/hash) live to the
``e2e.cli`` logger as they arrive, while still parsing the final JSON line.
"""

import json
import logging
import subprocess
import threading
import time

from .config import PROGRESS, REPO_ROOT, SANDBOX_CLI

_log = logging.getLogger("e2e.cli")
_timing_lock = threading.Lock()
_timing_records = []


class CliError(Exception):
    """The CLI produced output that was not a JSON line."""


def cli(*args, timeout=180):
    """Run ``sandbox-cli <args...>`` and return the parsed JSON response."""
    printable = " ".join(["sandbox-cli", *map(str, args)])
    _log.info("→ %s", printable)
    started = time.monotonic()
    if PROGRESS:
        stdout, stderr_lines, returncode = _run_streaming(args, timeout)
        raw = _select_json(stdout, stderr_lines)
    else:
        proc = subprocess.run(
            [str(SANDBOX_CLI), *map(str, args)],
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


def _run_streaming(args, timeout):
    """Run with --progress, streaming stderr (progress) live.

    Returns ``(stdout, stderr_lines, returncode)``.
    """
    cmd = [str(SANDBOX_CLI), "--progress", *map(str, args)]
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


def observability(operation, *args, **kwargs):
    """Run an observability-space operation."""
    return cli("observability", operation, *args, **kwargs)


def is_error(result):
    return isinstance(result, dict) and "error" in result
