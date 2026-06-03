"""Probe for ``sandbox.heavy_io_zoned_concurrent``.

Each worker runs three long-running shell calls, one per placement zone:

- **gitincluded**: under ``/testbed/perf_load_tracked/worker_NN``.
- **gitignored**: under ``/testbed/build/perf_load_worker_NN`` (``build/`` is
  matched by the SWE-EVO repo ``.gitignore``).
- **outside**: under ``/tmp/heavy_io_zoned/worker_NN`` (outside workspace
  binding).

After each long shell, a short follow-up shell reads the directory back
through a fresh lease so the test can prove the prior OCC merge published
the writes. The per-worker fragment records changed_path counts and
durations per zone for the reconcile/summary contract.
"""

from __future__ import annotations

import json
import time
from collections.abc import Awaitable, Callable
from typing import Any

from message.events import StreamEvent
from tools._framework.core.base import BaseTool
from tools._framework.core.results import ToolResult
from tools._framework.core.runtime import ExecutionMetadata
from tools.sandbox.read_file import read_file as read_file_tool
from tools.sandbox.exec_command import exec_command as exec_command_tool
from tools.sandbox.write_file import write_file as write_file_tool

from test_runner.agent.mock.sandbox_probe import SandboxCheck
from test_runner.audit.events import EventType
from test_runner.scenarios.sandbox.heavy_io_zoned_concurrent import WORKER_COUNT


WORKSPACE_ROOT = "/testbed"
OUTSIDE_ROOT = "/tmp/heavy_io_zoned"
ROOT = f"{WORKSPACE_ROOT}/.ephemeralos/sweevo-mock/heavy_io_zoned"
FRAGMENTS_DIR = f"{ROOT}/fragments"
SUMMARY_PATH = f"{ROOT}/summary.json"

WORKER_SCHEMA = "test_runner.heavy_io_zoned.worker.v1"
SUMMARY_SCHEMA = "test_runner.heavy_io_zoned.v1"

# 11 chunks * 3 MB == 33 MB per zone, 3 zones == ~99 MB per worker.
# 11 sleeps * 3 s == 33 s minimum wall time per shell (plus dd cost).
CHUNK_COUNT = 11
CHUNK_MB = 3
CHUNK_SLEEP_S = 3
SHELL_TIMEOUT_S = 180

ZONE_NAMES: tuple[str, ...] = ("gitincluded", "gitignored", "outside")

EmitStreamEvent = Callable[[StreamEvent], Awaitable[None]]
CallTool = Callable[..., Awaitable[ToolResult]]
PublishEvent = Callable[..., None]
PublishMockRecord = Callable[..., None]
RecordToolCheck = Callable[[str, ToolResult], None]


def _zone_dir(zone: str, index: int) -> str:
    label = f"worker_{index:02d}"
    if zone == "gitincluded":
        return f"{WORKSPACE_ROOT}/perf_load_tracked/{label}"
    if zone == "gitignored":
        return f"{WORKSPACE_ROOT}/build/perf_load_{label}"
    if zone == "outside":
        return f"{OUTSIDE_ROOT}/{label}"
    raise RuntimeError(f"unknown zone {zone!r}")


def _long_write_command(zone_dir: str) -> str:
    """Long-running shell: many small dd writes paced by sleeps."""
    return (
        f"set -e; mkdir -p {zone_dir}; "
        f"for i in $(seq 1 {CHUNK_COUNT}); do "
        f"  dd if=/dev/urandom of={zone_dir}/chunk_${{i}}.bin "
        f"     bs=1M count={CHUNK_MB} status=none; "
        f"  sleep {CHUNK_SLEEP_S}; "
        f"done; "
        f"ls -1 {zone_dir} | wc -l; "
        f"du -sk {zone_dir} | awk '{{print $1}}'"
    )


def _readback_command(zone_dir: str) -> str:
    """Short shell run through a fresh lease to prove merge published."""
    return (
        f"set -e; ls -1 {zone_dir} | wc -l; "
        f"du -sk {zone_dir} | awk '{{print $1}}'"
    )


async def run_heavy_io_zoned_seed_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """Seed shared directories before workers fan out."""
    setup = await call_tool(
        exec_command_tool,
        {
            "cmd": (
                f"mkdir -p {ROOT} {FRAGMENTS_DIR} "
                f"{WORKSPACE_ROOT}/perf_load_tracked "
                f"{WORKSPACE_ROOT}/build "
                f"{OUTSIDE_ROOT}"
            ),
            "timeout": 120,
        },
        metadata,
        emit,
    )
    record_tool_check("tool.exec_command.heavy_io_zoned.seed_dirs", setup)

    control_path = f"{ROOT}/control/seed.json"
    control_payload = {
        "schema": "test_runner.heavy_io_zoned.seed.v1",
        "worker_count": WORKER_COUNT,
        "chunk_count": CHUNK_COUNT,
        "chunk_mb": CHUNK_MB,
        "chunk_sleep_s": CHUNK_SLEEP_S,
        "zones": list(ZONE_NAMES),
    }
    control = await call_tool(
        write_file_tool,
        {
            "file_path": control_path,
            "content": json.dumps(control_payload, indent=2, sort_keys=True) + "\n",
        },
        metadata,
        emit,
    )
    record_tool_check("tool.write_file.heavy_io_zoned.control", control)
    return control_path


async def run_heavy_io_zoned_worker_probe(
    *,
    index: int,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    publish: PublishEvent,
    publish_mock_record: PublishMockRecord,
    record_tool_check: RecordToolCheck,
) -> str:
    """Run one zoned worker: long write + readback for each zone."""
    if index < 0 or index >= WORKER_COUNT:
        raise RuntimeError(f"worker index {index} outside 0..{WORKER_COUNT - 1}")
    del publish  # reserved for future per-zone conflict signaling

    started = time.perf_counter()
    zone_results: list[dict[str, Any]] = []

    for zone in ZONE_NAMES:
        zone_dir = _zone_dir(zone, index)
        zone_started = time.perf_counter()

        write_result = await call_tool(
            exec_command_tool,
            {"cmd": _long_write_command(zone_dir), "timeout": SHELL_TIMEOUT_S},
            metadata,
            emit,
            background_task_id=f"heavy_io_zoned.worker_{index:02d}.{zone}.write",
        )
        record_tool_check(
            f"tool.exec_command.heavy_io_zoned.worker_{index:02d}.{zone}.write",
            write_result,
        )
        write_meta = _capture_metadata("exec_command", write_result)
        write_duration = time.perf_counter() - zone_started

        readback_started = time.perf_counter()
        readback_result = await call_tool(
            exec_command_tool,
            {"cmd": _readback_command(zone_dir), "timeout": 60},
            metadata,
            emit,
        )
        record_tool_check(
            f"tool.exec_command.heavy_io_zoned.worker_{index:02d}.{zone}.readback",
            readback_result,
        )
        readback_meta = _capture_metadata("exec_command", readback_result)
        readback_duration = time.perf_counter() - readback_started

        readback_stdout = _shell_stdout(readback_result)
        readback_exit = _shell_exit_code(readback_result)
        readback_lines = [
            line for line in readback_stdout.splitlines() if line.strip()
        ]
        observed_file_count = _safe_int(readback_lines[0] if readback_lines else "")
        observed_kib = _safe_int(readback_lines[1] if len(readback_lines) > 1 else "")
        merged_ok = readback_exit == 0 and observed_file_count == CHUNK_COUNT

        publish_mock_record(
            EventType.MOCK_SANDBOX_CHECK_RECORDED,
            SandboxCheck(
                name=f"heavy_io_zoned.merge.{zone}.worker_{index:02d}",
                passed=merged_ok,
                detail=(
                    f"observed_file_count={observed_file_count} "
                    f"observed_kib={observed_kib} "
                    f"expected_file_count={CHUNK_COUNT}"
                ),
                changed_paths=tuple(
                    str(path) for path in (readback_result.metadata or {}).get(
                        "changed_paths", ()
                    )
                ),
            ),
        )
        write_exit = _shell_exit_code(write_result)
        if (
            write_result.is_error
            or readback_result.is_error
            or write_exit != 0
            or not merged_ok
        ):
            raise RuntimeError(
                f"worker {index:02d} zone {zone} failed: "
                f"write_error={write_result.is_error} "
                f"write_exit={write_exit} "
                f"readback_error={readback_result.is_error} "
                f"readback_exit={readback_exit} "
                f"observed_file_count={observed_file_count} "
                f"readback_stdout={readback_stdout[:200]!r}"
            )

        zone_results.append(
            {
                "zone": zone,
                "zone_dir": zone_dir,
                "write": write_meta,
                "readback": readback_meta,
                "observed_file_count": observed_file_count,
                "observed_kib": observed_kib,
                "write_duration_s": write_duration,
                "readback_duration_s": readback_duration,
                "merged_ok": merged_ok,
            }
        )

    fragment_path = f"{FRAGMENTS_DIR}/worker-{index:02d}.json"
    summary_payload = {
        "schema": WORKER_SCHEMA,
        "worker_index": index,
        "duration_s": time.perf_counter() - started,
        "zones": zone_results,
        "tool_call_count": 2 * len(zone_results),
    }
    fragment_write = await call_tool(
        write_file_tool,
        {
            "file_path": fragment_path,
            "content": json.dumps(summary_payload, indent=2, sort_keys=True) + "\n",
        },
        metadata,
        emit,
    )
    record_tool_check(
        f"tool.write_file.heavy_io_zoned.fragment_{index:02d}",
        fragment_write,
    )
    return fragment_path


async def run_heavy_io_zoned_reconcile_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """Aggregate per-worker fragments and assert the zoned merge contract."""
    expected_zones = list(ZONE_NAMES)
    expected_kib = CHUNK_COUNT * CHUNK_MB * 1024  # 33 MB == 33792 KiB nominal
    command = f"""python3 - <<'PY'
import json
from pathlib import Path

root = Path({ROOT!r})
fragments = sorted((root / "fragments").glob("worker-*.json"))
payloads = [json.loads(path.read_text(encoding="utf-8")) for path in fragments]
if len(payloads) != {WORKER_COUNT}:
    raise SystemExit(f"expected {WORKER_COUNT} fragments, saw {{len(payloads)}}")

expected_zones = {expected_zones!r}
worker_indexes = sorted(int(item["worker_index"]) for item in payloads)
if worker_indexes != list(range({WORKER_COUNT})):
    raise SystemExit(f"missing worker indexes: {{worker_indexes}}")

per_zone: dict[str, dict[str, int]] = {{
    zone: {{"file_count_sum": 0, "kib_sum": 0, "workspace_changed_paths": 0, "outside_changed_paths": 0, "merges_ok": 0}}
    for zone in expected_zones
}}
for item in payloads:
    seen = [str(z.get("zone")) for z in item.get("zones", [])]
    if seen != expected_zones:
        raise SystemExit(f"worker {{item.get('worker_index')}} zones {{seen}} != {{expected_zones}}")
    for entry in item["zones"]:
        zone = str(entry["zone"])
        bucket = per_zone[zone]
        bucket["file_count_sum"] += int(entry.get("observed_file_count", 0))
        bucket["kib_sum"] += int(entry.get("observed_kib", 0))
        if entry.get("merged_ok"):
            bucket["merges_ok"] += 1
        write_changed = int(((entry.get("write") or {{}}).get("changed_path_count", 0)))
        if zone == "outside":
            bucket["outside_changed_paths"] += write_changed
        else:
            bucket["workspace_changed_paths"] += write_changed

for zone in expected_zones:
    bucket = per_zone[zone]
    if bucket["merges_ok"] != {WORKER_COUNT}:
        raise SystemExit(
            f"zone {{zone}} merges_ok={{bucket['merges_ok']}} expected {WORKER_COUNT}"
        )
    expected_files_total = {WORKER_COUNT} * {CHUNK_COUNT}
    if bucket["file_count_sum"] != expected_files_total:
        raise SystemExit(
            f"zone {{zone}} file_count_sum={{bucket['file_count_sum']}} "
            f"expected {{expected_files_total}}"
        )

summary = {{
    "schema": {SUMMARY_SCHEMA!r},
    "worker_count": {WORKER_COUNT},
    "worker_indexes": worker_indexes,
    "chunk_count": {CHUNK_COUNT},
    "chunk_mb": {CHUNK_MB},
    "per_zone": per_zone,
    "max_worker_duration_s": max(float(item.get("duration_s", 0.0)) for item in payloads),
    "expected_kib_per_zone_per_worker": {expected_kib},
}}
(root / "summary.json").write_text(
    json.dumps(summary, indent=2, sort_keys=True) + "\\n", encoding="utf-8"
)
print(json.dumps(summary, sort_keys=True))
PY"""
    shell_result = await call_tool(
        exec_command_tool,
        {"cmd": command, "timeout": 180},
        metadata,
        emit,
    )
    record_tool_check("tool.exec_command.heavy_io_zoned.reconcile", shell_result)
    if SUMMARY_SCHEMA not in (shell_result.output or ""):
        raise RuntimeError(
            f"reconcile summary missing schema marker: {(shell_result.output or '')[:500]}"
        )

    summary_read = await call_tool(
        read_file_tool,
        {"file_path": SUMMARY_PATH, "start_line": 1, "end_line": 200},
        metadata,
        emit,
    )
    record_tool_check("tool.read_file.heavy_io_zoned.summary", summary_read)
    if SUMMARY_SCHEMA not in (summary_read.output or ""):
        raise RuntimeError(
            f"summary readback missing schema marker: {(summary_read.output or '')[:500]}"
        )
    return SUMMARY_PATH


def _capture_metadata(tool_name: str, result: ToolResult) -> dict[str, Any]:
    metadata = dict(result.metadata or {})
    timings = metadata.get("timings")
    if not isinstance(timings, dict):
        timings = {}
    return {
        "tool_name": tool_name,
        "is_error": bool(result.is_error),
        "status": str(metadata.get("status") or ""),
        "changed_path_count": len(list(metadata.get("changed_paths") or ())),
        "changed_paths": [
            str(path) for path in (metadata.get("changed_paths") or ())
        ],
        "timings": {str(key): float(value) for key, value in timings.items()},
    }


def _safe_int(value: str) -> int:
    try:
        return int(value.strip())
    except (TypeError, ValueError):
        return -1


def _shell_stdout(result: ToolResult) -> str:
    try:
        payload = json.loads(result.output)
    except (json.JSONDecodeError, TypeError):
        return str(result.output or "")
    return str(payload.get("stdout") or "")


def _shell_exit_code(result: ToolResult) -> int:
    try:
        payload = json.loads(result.output)
    except (json.JSONDecodeError, TypeError):
        return 0 if not result.is_error else 1
    raw = payload.get("exit_code")
    try:
        return int(raw)
    except (TypeError, ValueError):
        return 0 if not result.is_error else 1


# Silence unused-import warnings for symbols referenced via call_tool below.
_USED_TOOLS: tuple[BaseTool, ...] = (exec_command_tool, write_file_tool, read_file_tool)


__all__ = [
    "CHUNK_COUNT",
    "CHUNK_MB",
    "CHUNK_SLEEP_S",
    "OUTSIDE_ROOT",
    "ROOT",
    "SHELL_TIMEOUT_S",
    "SUMMARY_PATH",
    "SUMMARY_SCHEMA",
    "WORKSPACE_ROOT",
    "WORKER_SCHEMA",
    "ZONE_NAMES",
    "run_heavy_io_zoned_reconcile_probe",
    "run_heavy_io_zoned_seed_probe",
    "run_heavy_io_zoned_worker_probe",
]
