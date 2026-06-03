"""Probe for ``sandbox.high_concurrency_layerstack_overlay_occ``.

The scenario owns the DAG shape; this module owns the sandbox workload so the
ScenarioLoopRunner bridge stays thin while the capacity workload can grow.
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
from tools.sandbox.edit_file import edit_file as edit_file_tool
from tools.sandbox.read_file import read_file as read_file_tool
from tools.sandbox.exec_command import exec_command as exec_command_tool
from tools.sandbox.write_file import write_file as write_file_tool

from test_runner.agent.mock.sandbox_probe import SandboxCheck
from test_runner.audit.events import EventType
from test_runner.scenarios.sandbox.high_concurrency_layerstack_overlay_occ import (
    WORKER_COUNT,
)


ROOT = "/testbed/.ephemeralos/sweevo-mock/high_concurrency_layerstack_overlay_occ"
SUMMARY_PATH = f"{ROOT}/summary.json"
WORKER_SCHEMA = "test_runner.high_concurrency.worker.v1"
SUMMARY_SCHEMA = "test_runner.high_concurrency.v1"
DATA_FILES_PER_WORKER = 1
CONFLICT_WORKER_COUNT = 4
READ_FILE_INDEXES = (0, DATA_FILES_PER_WORKER - 1)
READS_PER_WORKER = len(set(READ_FILE_INDEXES))

EmitStreamEvent = Callable[[StreamEvent], Awaitable[None]]
CallTool = Callable[..., Awaitable[ToolResult]]
PublishEvent = Callable[..., None]
PublishMockRecord = Callable[..., None]
RecordToolCheck = Callable[[str, ToolResult], None]


async def run_high_concurrency_seed_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """Seed shared files before the concurrent worker fanout."""
    seed_payload = {
        "schema": "test_runner.high_concurrency.seed.v1",
        "worker_count": WORKER_COUNT,
        "data_files_per_worker": DATA_FILES_PER_WORKER,
        "conflict_worker_count": CONFLICT_WORKER_COUNT,
    }
    setup = await call_tool(
        exec_command_tool,
        {
            "cmd": (
                f"mkdir -p {ROOT}/workers {ROOT}/fragments {ROOT}/shared "
                f"{ROOT}/shell {ROOT}/control"
            ),
            "timeout": 120,
        },
        metadata,
        emit,
    )
    record_tool_check("tool.exec_command.high_concurrency.seed_dirs", setup)

    conflict_seed = await call_tool(
        write_file_tool,
        {
            "file_path": f"{ROOT}/shared/conflict.txt",
            "content": "owner=seed\nrevision=0\n",
        },
        metadata,
        emit,
    )
    record_tool_check("tool.write_file.high_concurrency.conflict_seed", conflict_seed)

    control_path = f"{ROOT}/control/seed.json"
    control = await call_tool(
        write_file_tool,
        {
            "file_path": control_path,
            "content": json.dumps(seed_payload, indent=2, sort_keys=True) + "\n",
        },
        metadata,
        emit,
    )
    record_tool_check("tool.write_file.high_concurrency.control", control)
    return control_path


async def run_high_concurrency_worker_probe(
    *,
    index: int,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    publish: PublishEvent,
    publish_mock_record: PublishMockRecord,
    record_tool_check: RecordToolCheck,
) -> str:
    """Run one concurrent worker and write its metric fragment."""
    if index < 0 or index >= WORKER_COUNT:
        raise RuntimeError(f"worker index {index} outside 0..{WORKER_COUNT - 1}")

    started = time.perf_counter()
    worker_dir = f"{ROOT}/workers/worker-{index:02d}"
    tool_metadata: list[dict[str, Any]] = []

    for file_index in range(DATA_FILES_PER_WORKER):
        path = f"{worker_dir}/file-{file_index:02d}.txt"
        seed = (
            f"worker={index:02d}\n"
            f"file={file_index:02d}\n"
            "value=seed\n"
            "status=pending\n"
        )
        write_result = await _call_checked(
            call_tool=call_tool,
            tool_obj=write_file_tool,
            raw_input={"file_path": path, "content": seed},
            metadata=metadata,
            emit=emit,
            record_tool_check=record_tool_check,
            check_name=f"tool.write_file.high_concurrency.worker_{index:02d}_{file_index:02d}",
        )
        tool_metadata.append(_capture_metadata("write_file", write_result))

        edit_result = await _call_checked(
            call_tool=call_tool,
            tool_obj=edit_file_tool,
            raw_input={
                "file_path": path,
                "old_text": "value=seed\nstatus=pending\n",
                "new_text": f"value={index:02d}-{file_index:02d}\nstatus=done\n",
                "description": (
                    "high-concurrency independent edit "
                    f"worker={index:02d} file={file_index:02d}"
                ),
            },
            metadata=metadata,
            emit=emit,
            record_tool_check=record_tool_check,
            check_name=f"tool.edit_file.high_concurrency.worker_{index:02d}_{file_index:02d}",
        )
        tool_metadata.append(_capture_metadata("edit_file", edit_result))

    for file_index in sorted(set(READ_FILE_INDEXES)):
        read_result = await _call_checked(
            call_tool=call_tool,
            tool_obj=read_file_tool,
            raw_input={
                "file_path": f"{worker_dir}/file-{file_index:02d}.txt",
                "start_line": 1,
                "end_line": 20,
            },
            metadata=metadata,
            emit=emit,
            record_tool_check=None,
            check_name="",
        )
        _assert_read_contains(
            read_result,
            f"value={index:02d}-{file_index:02d}",
            f"tool.read_file.high_concurrency.worker_{index:02d}_{file_index:02d}",
            publish_mock_record,
        )
        tool_metadata.append(_capture_metadata("read_file", read_result))

    conflict_payload = await _maybe_race_conflict(
        index=index,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        publish=publish,
        publish_mock_record=publish_mock_record,
    )
    if conflict_payload is not None:
        tool_metadata.append(conflict_payload["metadata"])

    fragment_path = f"{ROOT}/fragments/worker-{index:02d}.json"
    summary_payload = _worker_summary(
        index=index,
        started=started,
        tool_metadata=tool_metadata,
        conflict_payload=conflict_payload,
    )
    fragment_write = await call_tool(
        write_file_tool,
        {
            "file_path": fragment_path,
            "content": json.dumps(summary_payload, indent=2, sort_keys=True) + "\n",
        },
        metadata,
        emit,
    )
    record_tool_check(f"tool.write_file.high_concurrency.fragment_{index:02d}", fragment_write)
    return fragment_path


async def run_high_concurrency_reconcile_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """Aggregate worker fragments and verify the pressure-run contract."""
    payloads: list[dict[str, Any]] = []
    for index in range(WORKER_COUNT):
        fragment = await _call_checked(
            call_tool=call_tool,
            tool_obj=read_file_tool,
            raw_input={
                "file_path": f"{ROOT}/fragments/worker-{index:02d}.json",
                "start_line": 1,
                "end_line": 200,
            },
            metadata=metadata,
            emit=emit,
            record_tool_check=None,
            check_name="",
        )
        payloads.append(json.loads(_read_numbered_content(fragment)))

    summary = _reconcile_summary(payloads)
    await _call_checked(
        call_tool=call_tool,
        tool_obj=write_file_tool,
        raw_input={
            "file_path": SUMMARY_PATH,
            "content": json.dumps(summary, indent=2, sort_keys=True) + "\n",
        },
        metadata=metadata,
        emit=emit,
        record_tool_check=record_tool_check,
        check_name="tool.write_file.high_concurrency.reconcile_summary",
    )

    summary_read = await _call_checked(
        call_tool=call_tool,
        tool_obj=read_file_tool,
        raw_input={"file_path": SUMMARY_PATH, "start_line": 1, "end_line": 80},
        metadata=metadata,
        emit=emit,
        record_tool_check=None,
        check_name="",
    )
    _assert_output_contains(summary_read, SUMMARY_SCHEMA, "summary readback schema")
    return SUMMARY_PATH


def _reconcile_summary(payloads: list[dict[str, Any]]) -> dict[str, Any]:
    conflict_successes = sum(
        1 for item in payloads if item.get("conflict_status") == "success"
    )
    conflict_errors = sum(
        1 for item in payloads if item.get("conflict_status") == "conflict"
    )
    unexpected_errors = sum(int(item.get("unexpected_error_count", 0)) for item in payloads)
    worker_indexes = sorted(int(item["worker_index"]) for item in payloads)
    if len(payloads) != WORKER_COUNT:
        raise RuntimeError(f"expected {WORKER_COUNT} fragments, saw {len(payloads)}")
    if worker_indexes != list(range(WORKER_COUNT)):
        raise RuntimeError(f"missing worker indexes: {worker_indexes}")
    if conflict_successes < 1 or conflict_errors < 1:
        raise RuntimeError(
            "expected at least one shared OCC success and one conflict, "
            f"saw successes={conflict_successes} conflicts={conflict_errors}"
        )
    if unexpected_errors:
        raise RuntimeError(f"unexpected worker tool errors: {unexpected_errors}")

    return {
        "schema": SUMMARY_SCHEMA,
        "worker_count": len(payloads),
        "worker_indexes": worker_indexes,
        "conflict_successes": conflict_successes,
        "conflict_errors": conflict_errors,
        "total_write_calls": sum(int(item["write_count"]) for item in payloads),
        "total_edit_calls": sum(int(item["edit_count"]) for item in payloads),
        "total_read_calls": sum(int(item["read_count"]) for item in payloads),
        "total_shell_calls": sum(int(item["shell_count"]) for item in payloads),
        "max_auto_squash_depth_before": max(
            float(item.get("max_auto_squash_depth_before", 0.0)) for item in payloads
        ),
        "max_auto_squash_total_s": max(
            float(item.get("max_auto_squash_total_s", 0.0)) for item in payloads
        ),
        "max_commit_resume_wait_s": max(
            float(item.get("max_commit_resume_wait_s", 0.0)) for item in payloads
        ),
        "max_worker_duration_s": max(
            float(item.get("duration_s", 0.0)) for item in payloads
        ),
    }


async def _call_checked(
    *,
    call_tool: CallTool,
    tool_obj: BaseTool,
    raw_input: dict[str, Any],
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    record_tool_check: RecordToolCheck | None,
    check_name: str,
    allow_error: bool = False,
) -> ToolResult:
    result = await call_tool(
        tool_obj,
        raw_input,
        metadata,
        emit,
        allow_error=allow_error,
    )
    if record_tool_check is not None:
        record_tool_check(check_name, result)
    return result


async def _maybe_race_conflict(
    *,
    index: int,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    publish: PublishEvent,
    publish_mock_record: PublishMockRecord,
) -> dict[str, Any] | None:
    if index >= CONFLICT_WORKER_COUNT:
        return None

    result = await call_tool(
        edit_file_tool,
        {
            "file_path": f"{ROOT}/shared/conflict.txt",
            "old_text": "owner=seed\n",
            "new_text": f"owner=worker-{index:02d}\n",
            "description": (
                "shared OCC race for high-concurrency probe "
                f"worker={index:02d}"
            ),
        },
        metadata,
        emit,
        allow_error=True,
    )
    captured = _capture_metadata("edit_file", result)
    conflict_reason = str(
        result.metadata.get("conflict_reason")
        or result.output
        or "shared OCC conflict"
    )
    if result.is_error:
        status = "conflict"
        publish(
            EventType.SANDBOX_CONFLICT_DETECTED,
            metadata=metadata,
            payload={
                "worker_index": index,
                "conflict_reason": conflict_reason,
            },
        )
    else:
        status = "success"
    publish_mock_record(
        EventType.MOCK_SANDBOX_CHECK_RECORDED,
        SandboxCheck(
            name=f"tool.edit_file.high_concurrency.shared_conflict_{index:02d}",
            passed=True,
            detail=f"status={status} reason={conflict_reason!r}",
            changed_paths=tuple(
                str(path) for path in result.metadata.get("changed_paths", ())
            ),
        ),
    )
    return {
        "status": status,
        "reason": conflict_reason if result.is_error else "",
        "metadata": captured,
    }


def _worker_summary(
    *,
    index: int,
    started: float,
    tool_metadata: list[dict[str, Any]],
    conflict_payload: dict[str, Any] | None,
) -> dict[str, Any]:
    by_tool = _count_tools(tool_metadata)
    unexpected_errors = sum(
        1
        for entry in tool_metadata
        if bool(entry.get("is_error"))
        and not (
            entry.get("tool_name") == "edit_file"
            and conflict_payload is not None
            and entry is conflict_payload.get("metadata")
        )
    )
    conflict_status = "not_attempted"
    conflict_reason = ""
    if conflict_payload is not None:
        conflict_status = str(conflict_payload["status"])
        conflict_reason = str(conflict_payload["reason"])
    return {
        "schema": WORKER_SCHEMA,
        "worker_index": index,
        "task_id": str(tool_metadata[0].get("task_id") or "") if tool_metadata else "",
        "write_count": by_tool.get("write_file", 0),
        "edit_count": by_tool.get("edit_file", 0),
        "read_count": by_tool.get("read_file", 0),
        "shell_count": by_tool.get("exec_command", 0),
        "conflict_status": conflict_status,
        "conflict_reason": conflict_reason,
        "unexpected_error_count": unexpected_errors,
        "max_auto_squash_depth_before": _max_timing(
            tool_metadata, "layer_stack.auto_squash.depth_before"
        ),
        "max_auto_squash_total_s": _max_timing(
            tool_metadata, "layer_stack.auto_squash.total_s"
        ),
        "max_commit_resume_wait_s": _max_timing(
            tool_metadata, "occ.apply.commit_resume_wait_s"
        ),
        "duration_s": time.perf_counter() - started,
        "tool_call_count": len(tool_metadata),
    }


def _capture_metadata(tool_name: str, result: ToolResult) -> dict[str, Any]:
    metadata = dict(result.metadata or {})
    timings = metadata.get("timings")
    if not isinstance(timings, dict):
        timings = {}
    return {
        "tool_name": tool_name,
        "is_error": bool(result.is_error),
        "status": str(metadata.get("status") or ""),
        "task_id": str(metadata.get("task_id") or ""),
        "changed_path_count": len(list(metadata.get("changed_paths") or ())),
        "timings": {str(key): float(value) for key, value in timings.items()},
    }


def _count_tools(entries: list[dict[str, Any]]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for entry in entries:
        name = str(entry.get("tool_name") or "")
        counts[name] = counts.get(name, 0) + 1
    return counts


def _max_timing(entries: list[dict[str, Any]], key: str) -> float:
    values = [
        float(timings[key])
        for entry in entries
        for timings in (entry.get("timings") or {},)
        if key in timings
    ]
    return max(values, default=0.0)


def _assert_read_contains(
    result: ToolResult,
    needle: str,
    check_name: str,
    publish_mock_record: PublishMockRecord,
) -> None:
    content = _read_content(result)
    passed = needle in content
    publish_mock_record(
        EventType.MOCK_SANDBOX_CHECK_RECORDED,
        SandboxCheck(name=check_name, passed=passed, detail=f"needle={needle!r}"),
    )
    if not passed:
        raise RuntimeError(f"{check_name} did not find {needle!r}.")


def _read_content(result: ToolResult) -> str:
    try:
        payload = json.loads(result.output)
    except json.JSONDecodeError:
        return result.output
    return str(payload.get("content") or result.output)


def _read_numbered_content(result: ToolResult) -> str:
    lines: list[str] = []
    for line in _read_content(result).splitlines():
        if len(line) >= 6 and line[:4].strip().isdigit() and line[4:6] == ": ":
            lines.append(line[6:])
        else:
            lines.append(line)
    return "\n".join(lines)


def _assert_output_contains(result: ToolResult, needle: str, label: str) -> None:
    if needle not in result.output:
        raise RuntimeError(f"{label} missing {needle!r}: {result.output[:500]}")


__all__ = [
    "CONFLICT_WORKER_COUNT",
    "DATA_FILES_PER_WORKER",
    "READS_PER_WORKER",
    "ROOT",
    "SUMMARY_PATH",
    "SUMMARY_SCHEMA",
    "run_high_concurrency_reconcile_probe",
    "run_high_concurrency_seed_probe",
    "run_high_concurrency_worker_probe",
]
