"""Fan-out probes for ``sandbox.auto_squash_commit_resume``."""

from __future__ import annotations

import json
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
from test_runner.scenarios.sandbox._constants import AUTO_SQUASH_MAX_DEPTH


ROOT = "/testbed/.ephemeralos/sweevo-mock/auto_squash_commit_resume"
SUMMARY_PATH = f"{ROOT}/summary.json"
EDIT_TARGET = f"{ROOT}/edit-target.txt"
WRITE_COUNT = AUTO_SQUASH_MAX_DEPTH + 4
SQUASH_A_COUNT = 80
SQUASH_B_START = SQUASH_A_COUNT
SQUASH_B_COUNT = WRITE_COUNT - SQUASH_A_COUNT

EmitStreamEvent = Callable[[StreamEvent], Awaitable[None]]
CallTool = Callable[..., Awaitable[ToolResult]]
PublishEvent = Callable[..., None]
PublishMockRecord = Callable[..., None]
RecordToolCheck = Callable[[str, ToolResult], None]


async def run_auto_squash_seed_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    result = await _call_checked(
        call_tool=call_tool,
        tool_obj=exec_command_tool,
        raw_input={"cmd": f"mkdir -p {ROOT}/fragments {ROOT}/independent", "timeout": 60},
        metadata=metadata,
        emit=emit,
        record_tool_check=record_tool_check,
        check_name="tool.exec_command.auto_squash.seed_dirs",
    )
    if result.is_error:
        raise RuntimeError(f"auto-squash seed failed: {result.output}")
    return ROOT


async def run_auto_squash_squash_a_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    return await _run_write_slice(
        slice_name="squash_a",
        start=0,
        count=SQUASH_A_COUNT,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def run_auto_squash_squash_b_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    return await _run_write_slice(
        slice_name="squash_b",
        start=SQUASH_B_START,
        count=SQUASH_B_COUNT,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def run_auto_squash_independent_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    path = f"{ROOT}/independent/concurrent.txt"
    write = await _call_checked(
        call_tool=call_tool,
        tool_obj=write_file_tool,
        raw_input={"file_path": path, "content": "independent=seed\n"},
        metadata=metadata,
        emit=emit,
        record_tool_check=record_tool_check,
        check_name="tool.write_file.auto_squash.independent",
    )
    read = await _call_checked(
        call_tool=call_tool,
        tool_obj=read_file_tool,
        raw_input={"file_path": path, "start_line": 1, "end_line": 5},
        metadata=metadata,
        emit=emit,
        record_tool_check=record_tool_check,
        check_name="tool.read_file.auto_squash.independent",
    )
    _assert_contains(_read_content(read), "independent=seed", "independent readback")
    fragment_path = f"{ROOT}/fragments/independent.json"
    return await _write_json_fragment(
        fragment_path,
        {"path": path, "write_status": _status(write)},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        check_name="tool.write_file.auto_squash.independent_fragment",
    )


async def run_auto_squash_reconcile_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    publish: PublishEvent,
    publish_mock_record: PublishMockRecord,
    record_tool_check: RecordToolCheck,
) -> str:
    slice_a = await _read_json(
        f"{ROOT}/fragments/squash_a.json",
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
    )
    slice_b = await _read_json(
        f"{ROOT}/fragments/squash_b.json",
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
    )
    await _read_json(
        f"{ROOT}/fragments/independent.json",
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
    )
    write_paths = [*_string_list(slice_a.get("write_paths")), *_string_list(slice_b.get("write_paths"))]
    if len(write_paths) != WRITE_COUNT:
        raise RuntimeError(f"expected {WRITE_COUNT} write paths, saw {len(write_paths)}")

    edit_metadata: list[dict[str, Any]] = []
    seed = await _call_checked(
        call_tool=call_tool,
        tool_obj=write_file_tool,
        raw_input={"file_path": EDIT_TARGET, "content": "alpha=old\nbeta=old\n"},
        metadata=metadata,
        emit=emit,
        record_tool_check=record_tool_check,
        check_name="tool.write_file.auto_squash.edit_seed",
    )
    edit_metadata.append(dict(seed.metadata or {}))

    for index, (old_text, new_text) in enumerate(
        (("alpha=old\n", "alpha=new\n"), ("beta=old\n", "beta=new\n"))
    ):
        edit = await _call_checked(
            call_tool=call_tool,
            tool_obj=edit_file_tool,
            raw_input={
                "file_path": EDIT_TARGET,
                "old_text": old_text,
                "new_text": new_text,
                "description": f"auto-squash reconcile edit {index}",
            },
            metadata=metadata,
            emit=emit,
            record_tool_check=record_tool_check,
            check_name=f"tool.edit_file.auto_squash.post_threshold_{index}",
        )
        edit_metadata.append(dict(edit.metadata or {}))

    for label, path in (
        ("first", write_paths[0]),
        ("middle", write_paths[len(write_paths) // 2]),
        ("last", write_paths[-1]),
        ("edited", EDIT_TARGET),
    ):
        read_result = await _call_checked(
            call_tool=call_tool,
            tool_obj=read_file_tool,
            raw_input={"file_path": path, "start_line": 1, "end_line": 20},
            metadata=metadata,
            emit=emit,
            record_tool_check=record_tool_check,
            check_name=f"tool.read_file.auto_squash.after_squash_{label}",
        )
        if label == "edited":
            _assert_contains(_read_content(read_result), "alpha=new", "edited readback")

    shell_listing = await _call_checked(
        call_tool=call_tool,
        tool_obj=exec_command_tool,
        raw_input={"cmd": f"ls {ROOT} | sort | head -n 200 && cat {EDIT_TARGET}", "timeout": 60},
        metadata=metadata,
        emit=emit,
        record_tool_check=record_tool_check,
        check_name="tool.exec_command.auto_squash.readback",
    )
    _assert_contains(
        _shell_stdout(shell_listing),
        "alpha=new\nbeta=new\n",
        "shell readback",
    )

    conflict_result = await call_tool(
        edit_file_tool,
        {
            "file_path": EDIT_TARGET,
            "old_text": "missing-anchor-text\n",
            "new_text": "should-not-apply\n",
            "description": "intentional missing-anchor conflict for auto-squash probe",
        },
        metadata,
        emit,
        allow_error=True,
    )
    conflict_meta = dict(conflict_result.metadata or {})
    conflict_status = str(conflict_meta.get("status") or "")
    conflict_reason = str(conflict_meta.get("conflict_reason") or "")
    conflict_changed_paths = [str(p) for p in conflict_meta.get("changed_paths") or ()]
    conflict_passed = bool(conflict_result.is_error and conflict_reason)
    publish_mock_record(
        EventType.MOCK_SANDBOX_CHECK_RECORDED,
        SandboxCheck(
            name="tool.edit_file.intentional_conflict",
            passed=conflict_passed,
            detail=(
                f"status={conflict_status} reason={conflict_reason!r} "
                f"is_error={conflict_result.is_error}"
            ),
            changed_paths=tuple(conflict_changed_paths),
        ),
    )
    if not conflict_passed:
        raise RuntimeError("Intentional missing-anchor edit unexpectedly succeeded.")
    publish(
        EventType.SANDBOX_CONFLICT_DETECTED,
        metadata=metadata,
        payload={"conflict_reason": conflict_reason},
    )

    summary_payload = {
        "probe": "auto_squash_commit_resume",
        "write_count": WRITE_COUNT,
        "edit_target": EDIT_TARGET,
        "edit_paths": write_paths,
        "conflict_status": conflict_status,
        "conflict_reason": conflict_reason,
        "conflict_changed_paths": conflict_changed_paths,
        "conflict_is_error": bool(conflict_result.is_error),
        "max_depth_before": max(
            float(slice_a.get("max_depth_before") or 0.0),
            float(slice_b.get("max_depth_before") or 0.0),
            _max_timing(edit_metadata, "layer_stack.auto_squash.depth_before"),
        ),
        "max_commit_resume_wait_s": max(
            float(slice_a.get("max_commit_resume_wait_s") or 0.0),
            float(slice_b.get("max_commit_resume_wait_s") or 0.0),
            _max_timing(edit_metadata, "occ.apply.commit_resume_wait_s"),
        ),
        "max_auto_squash_total_s": max(
            float(slice_a.get("max_auto_squash_total_s") or 0.0),
            float(slice_b.get("max_auto_squash_total_s") or 0.0),
            _max_timing(edit_metadata, "layer_stack.auto_squash.total_s"),
        ),
    }
    return await _write_json_fragment(
        SUMMARY_PATH,
        summary_payload,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        check_name="tool.write_file.auto_squash.summary",
    )


async def _run_write_slice(
    *,
    slice_name: str,
    start: int,
    count: int,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    metadata.repo_root = "/testbed"
    metadata_records: list[dict[str, Any]] = []
    write_paths: list[str] = []
    for index in range(start, start + count):
        path = f"{ROOT}/write-{index:02d}.txt"
        result = await _call_checked(
            call_tool=call_tool,
            tool_obj=write_file_tool,
            raw_input={"file_path": path, "content": f"write-{index:02d}\n"},
            metadata=metadata,
            emit=emit,
            record_tool_check=record_tool_check,
            check_name=f"tool.write_file.auto_squash.{slice_name}_{index:02d}",
        )
        write_paths.append(path)
        metadata_records.append(dict(result.metadata or {}))

    return await _write_json_fragment(
        f"{ROOT}/fragments/{slice_name}.json",
        {
            "slice": slice_name,
            "start": start,
            "count": count,
            "write_paths": write_paths,
            "max_depth_before": _max_timing(
                metadata_records, "layer_stack.auto_squash.depth_before"
            ),
            "max_commit_resume_wait_s": _max_timing(
                metadata_records, "occ.apply.commit_resume_wait_s"
            ),
            "max_auto_squash_total_s": _max_timing(
                metadata_records, "layer_stack.auto_squash.total_s"
            ),
        },
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        check_name=f"tool.write_file.auto_squash.{slice_name}_fragment",
    )


async def _call_checked(
    *,
    call_tool: CallTool,
    tool_obj: BaseTool,
    raw_input: dict[str, Any],
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    record_tool_check: RecordToolCheck | None,
    check_name: str,
) -> ToolResult:
    result = await call_tool(tool_obj, raw_input, metadata, emit)
    if record_tool_check is not None:
        record_tool_check(check_name, result)
    return result


async def _write_json_fragment(
    path: str,
    payload: dict[str, Any],
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
    check_name: str,
) -> str:
    result = await _call_checked(
        call_tool=call_tool,
        tool_obj=write_file_tool,
        raw_input={"file_path": path, "content": json.dumps(payload, indent=2) + "\n"},
        metadata=metadata,
        emit=emit,
        record_tool_check=record_tool_check,
        check_name=check_name,
    )
    if result.is_error:
        raise RuntimeError(f"json fragment write failed for {path}: {result.output}")
    return path


async def _read_json(
    path: str,
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
) -> dict[str, Any]:
    result = await _call_checked(
        call_tool=call_tool,
        tool_obj=read_file_tool,
        raw_input={"file_path": path, "start_line": 1, "end_line": 200},
        metadata=metadata,
        emit=emit,
        record_tool_check=None,
        check_name="",
    )
    payload = json.loads(_read_content(result) or "{}")
    return payload if isinstance(payload, dict) else {}


def _max_timing(metadata_records: list[dict[str, Any]], key: str) -> float:
    values: list[float] = []
    for entry in metadata_records:
        timings = entry.get("timings") or {}
        if not isinstance(timings, dict):
            continue
        try:
            values.append(float(timings.get(key, 0.0)))
        except (TypeError, ValueError):
            continue
    return max(values, default=0.0)


def _read_content(result: ToolResult) -> str:
    try:
        parsed = json.loads(result.output or "{}")
    except json.JSONDecodeError:
        parsed = {}
    content = str(parsed.get("content") or result.output or "")
    lines: list[str] = []
    for line in content.splitlines():
        if len(line) >= 6 and line[:4].strip().isdigit() and line[4:6] == ": ":
            lines.append(line[6:])
        else:
            lines.append(line)
    return "\n".join(lines)


def _status(result: ToolResult) -> str:
    return str((result.metadata or {}).get("status") or "")


def _shell_stdout(result: ToolResult) -> str:
    try:
        parsed = json.loads(result.output or "{}")
    except json.JSONDecodeError:
        return (result.output or "").replace("\r\n", "\n")
    return str(parsed.get("stdout") or result.output or "").replace("\r\n", "\n")


def _string_list(value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    return [str(item) for item in value]


def _assert_contains(content: str, needle: str, label: str) -> None:
    if needle not in content:
        raise RuntimeError(f"{label} missing {needle!r}: {content[:500]}")
