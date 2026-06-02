"""Background command probes that drive ``exec_command`` through
the mock-agent tool framework so the scenario harness records full
``sandbox_events.jsonl`` / ``performance_report.json`` artifacts under
``.sweevo_runs/scenario_logs/...``.

One async probe function per scenario action; each one writes a JSON
summary to a known workspace path that the matching test reads back via
``sandbox_api.read_file`` after the scenario report returns.

Background mode is enabled by passing ``background_task_id`` through
``call_tool``. The bridge maps that stable id to the returned command session id.
"""

from __future__ import annotations

import asyncio
import json
import os
import time
from collections.abc import Awaitable, Callable
from typing import Any
from uuid import uuid4

import sandbox.api as sandbox_api
from engine.background.task_supervisor import BackgroundTaskSupervisor
from message.events import StreamEvent
from tools._framework.core.base import BaseTool
from tools._framework.core.results import ToolResult
from tools._framework.core.runtime import ExecutionMetadata
from tools.isolated_workspace.enter_isolated_workspace import (
    enter_isolated_workspace as enter_isolated_workspace_tool,
)
from tools.isolated_workspace.exit_isolated_workspace import (
    exit_isolated_workspace as exit_isolated_workspace_tool,
)
from tools.sandbox.read_file import read_file as read_file_tool
from tools.sandbox.exec_command import exec_command as exec_command_tool
from tools.sandbox.write_stdin import write_stdin as write_stdin_tool
from tools.sandbox.write_file import write_file as write_file_tool

WORKSPACE_ROOT = "/testbed"
ROOT = f"{WORKSPACE_ROOT}/.ephemeralos/sweevo-mock/background_shell"
GOLDEN_SUMMARY = f"{ROOT}/golden/summary.json"
STOP_SUMMARY = f"{ROOT}/stop/summary.json"
INTERLEAVE_SUMMARY = f"{ROOT}/interleave/summary.json"
EXHAUSTION_SUMMARY = f"{ROOT}/exhaustion/summary.json"
PARTIAL_WRITE_SUMMARY = f"{ROOT}/partial_write/summary.json"
MAINTENANCE_SUMMARY = f"{ROOT}/maintenance/summary.json"
LATE_CANCEL_SUMMARY = f"{ROOT}/late_cancel/summary.json"
MIXED_CONFLICT_SUMMARY = f"{ROOT}/mixed_fg_bg_same_path_conflict/summary.json"
HEARTBEAT_LOSS_SUMMARY = f"{ROOT}/heartbeat_loss/summary.json"
EXIT_IWS_DRAIN_SUMMARY = f"{ROOT}/exit_iws_drain/summary.json"
ENGINE_RESTART_SUMMARY = f"{ROOT}/engine_restart/summary.json"
MANY_SMALL_WRITES_SUMMARY = f"{ROOT}/many_small_writes/summary.json"
MIXED_OP_CONCURRENT_SUMMARY = f"{ROOT}/mixed_op_concurrent/summary.json"

SUMMARY_SCHEMA = "test_runner.background_shell.v1"
BACKGROUND_IWS_LAYER_STACK_ROOT = "/eos/layer-stack"

EmitStreamEvent = Callable[[StreamEvent], Awaitable[None]]
# call_tool signature includes the background_task_id compatibility parameter
# consumed by the ScenarioLoopRunner bridge.
CallTool = Callable[..., Awaitable[ToolResult]]
RecordToolCheck = Callable[[str, ToolResult], None]
_BACKGROUND_DRAIN_TIMEOUT_S = 10.0


# ---- shared helpers --------------------------------------------------------


def _bg_id(label: str) -> str:
    return f"bg-{label}-{uuid4().hex[:8]}"


def _agent_id(metadata: ExecutionMetadata) -> str:
    return str(metadata.agent_run_id or metadata.agent_name or "").strip()


async def _wait_for_background_drain(metadata: ExecutionMetadata) -> None:
    sandbox_id = str(metadata.sandbox_id or "").strip()
    agent_id = _agent_id(metadata)
    if not sandbox_id or not agent_id:
        return
    deadline = time.perf_counter() + _BACKGROUND_DRAIN_TIMEOUT_S
    while time.perf_counter() < deadline:
        try:
            count = await sandbox_api.command_session_count(sandbox_id, agent_id)
        except Exception:
            return
        if count <= 0:
            return
        await asyncio.sleep(0.1)


async def _wait_for_tracked_task_settled(
    manager: BackgroundTaskSupervisor,
    task_id: str,
    *,
    timeout_s: float = 3.0,
) -> str:
    deadline = time.perf_counter() + timeout_s
    status = "missing"
    while time.perf_counter() < deadline:
        tracked = manager.get_task(task_id)
        status = str(tracked.status.value) if tracked is not None else "missing"
        if status != "running":
            return status
        await asyncio.sleep(0.1)
    return status


def _command_payload(result: ToolResult) -> dict[str, Any]:
    """Decode the JSON body exec_command writes into ``ToolResult.output``."""
    try:
        return json.loads(result.output or "{}")
    except json.JSONDecodeError:
        return {}


def _command_metadata(result: ToolResult) -> dict[str, Any]:
    meta = dict(result.metadata or {})
    return {
        "timings": dict(meta.get("timings") or {}),
        "changed_paths": list(meta.get("changed_paths") or ()),
        "status": meta.get("status"),
        "conflict_reason": meta.get("conflict_reason"),
    }


def _json_payload(result: ToolResult) -> dict[str, Any]:
    try:
        payload = json.loads(result.output or "{}")
    except json.JSONDecodeError:
        return {}
    return payload if isinstance(payload, dict) else {}


def _tool_metadata(result: ToolResult) -> dict[str, Any]:
    meta = dict(result.metadata or {})
    return {
        "timings": dict(meta.get("timings") or {}),
        "changed_paths": list(meta.get("changed_paths") or ()),
        "status": meta.get("status"),
        "conflict_reason": meta.get("conflict_reason"),
        "error_kind": meta.get("error_kind"),
        "mutation_source": meta.get("mutation_source"),
    }


def _tool_record(result: ToolResult) -> dict[str, Any]:
    payload = _json_payload(result)
    output = payload.get("output") if isinstance(payload.get("output"), dict) else {}
    return {
        "is_error": bool(result.is_error),
        "status": payload.get("status") or result.metadata.get("status"),
        "stdout": payload.get("stdout") or output.get("stdout"),
        "stderr": payload.get("stderr") or output.get("stderr"),
        "conflict_reason": (
            payload.get("conflict_reason") or result.metadata.get("conflict_reason")
        ),
        "changed_paths": list(
            payload.get("changed_paths") or result.metadata.get("changed_paths") or ()
        ),
        "error": payload.get("error") or result.metadata.get("error"),
        "metadata": _tool_metadata(result),
    }


def _read_content(result: ToolResult) -> str:
    payload = _json_payload(result)
    return str(payload.get("content") or result.output or "")


def _hook_failure_reason(result: ToolResult) -> str:
    """Pull a failing pre-hook's ``metadata['reason']`` tag out of a result.

    A prehook rejection is a ``hook_failure`` ToolResult whose user-facing
    output is the generic permission-deny JSON; the per-branch reason tag lives
    in the hook trace lifted into ``metadata['hook_trace']`` by the execution
    pipeline. Returns ``""`` when the result is not a hook failure.
    """
    trace = (result.metadata or {}).get("hook_trace")
    if not isinstance(trace, list):
        return ""
    for entry in reversed(trace):
        if isinstance(entry, dict) and entry.get("status") == "fail":
            meta = entry.get("metadata")
            if isinstance(meta, dict) and meta.get("reason"):
                return str(meta["reason"])
    return ""


async def _call_probe_tool(
    *,
    label: str,
    tool_obj: BaseTool,
    raw_input: dict[str, Any],
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck | None,
    allow_error: bool = False,
    background_task_id: str | None = None,
) -> ToolResult:
    result = await call_tool(
        tool_obj,
        raw_input,
        metadata,
        emit,
        allow_error=allow_error,
        background_task_id=background_task_id,
    )
    if record_tool_check is not None:
        record_tool_check(f"tool.{tool_obj.name}.background_shell.{label}", result)
    return result


async def _wait_for_command_session_count(
    *,
    sandbox_id: str,
    agent_id: str,
    minimum: int,
    timeout_s: float = 8.0,
) -> int:
    deadline = time.perf_counter() + timeout_s
    last_count = 0
    while time.perf_counter() < deadline:
        last_count = await sandbox_api.command_session_count(sandbox_id, agent_id)
        if last_count >= minimum:
            return last_count
        await asyncio.sleep(0.1)
    return last_count


async def _await_task_record(
    task: asyncio.Task[ToolResult],
) -> dict[str, Any]:
    try:
        result = await task
    except asyncio.CancelledError:
        return {"cancelled": True, "is_error": True, "exception": "CancelledError"}
    except Exception as exc:  # noqa: BLE001 - scenario summary should survive
        return {
            "cancelled": False,
            "is_error": True,
            "exception": type(exc).__name__,
            "message": str(exc)[:300],
        }
    payload = _command_payload(result)
    return {
        "cancelled": False,
        "is_error": bool(result.is_error),
        "exit_code": payload.get("exit_code"),
        "status": payload.get("status"),
        "stdout_excerpt": str(payload.get("stdout") or "")[:300],
        "stderr_excerpt": str(payload.get("stderr") or "")[:300],
        "shell_metadata": _command_metadata(result),
    }


async def _write_summary(
    *,
    path: str,
    payload: dict[str, Any],
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    body = json.dumps(payload, indent=2, sort_keys=True) + "\n"
    mkdir = await call_tool(
        exec_command_tool,
        {"command": f"mkdir -p $(dirname {path})", "timeout": 30},
        metadata,
        emit,
    )
    record_tool_check(f"tool.exec_command.background_shell.summary.mkdir.{path}", mkdir)
    if mkdir.is_error:
        raise RuntimeError(
            f"background_shell summary directory create failed for {path}: "
            f"{_command_payload(mkdir).get('stderr', '')[:200]}"
        )
    written = await call_tool(
        write_file_tool,
        {"file_path": path, "content": body},
        metadata,
        emit,
    )
    record_tool_check(f"tool.write_file.background_shell.summary.{path}", written)
    if written.is_error:
        raise RuntimeError(
            f"background_shell summary write failed for {path}: {written.output[:200]}"
        )
    return path


def _percentile(values: list[float], pct: float) -> float:
    if not values:
        return 0.0
    samples = sorted(values)
    if len(samples) == 1:
        return samples[0]
    rank = (pct / 100.0) * (len(samples) - 1)
    lo = int(rank)
    hi = min(lo + 1, len(samples) - 1)
    frac = rank - lo
    return samples[lo] * (1 - frac) + samples[hi] * frac


# ---- T1 golden -------------------------------------------------------------


GOLDEN_LAUNCH_COUNT = 3
GOLDEN_SLEEP_S = 5


async def run_background_shell_golden_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """T1: N concurrent background launches; wait for natural exit."""
    started = time.perf_counter()

    async def _one(index: int) -> dict[str, Any]:
        t0 = time.perf_counter()
        result = await call_tool(
            exec_command_tool,
            {
                "command": f"sleep {GOLDEN_SLEEP_S}; echo done-{index}",
                "timeout": 120,
            },
            metadata,
            emit,
            background_task_id=_bg_id(f"golden-{index}"),
        )
        record_tool_check(f"tool.exec_command.background_shell.golden.{index}", result)
        payload = _command_payload(result)
        return {
            "index": index,
            "duration_s": time.perf_counter() - t0,
            "exit_code": int(payload.get("exit_code", -1)),
            "status": str(payload.get("status") or ""),
            "stdout_excerpt": str(payload.get("stdout") or "")[:200],
            "is_error": bool(result.is_error),
            "shell_metadata": _command_metadata(result),
        }

    results = await asyncio.gather(
        *(_one(i) for i in range(GOLDEN_LAUNCH_COUNT)),
        return_exceptions=False,
    )
    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "golden",
        "launch_count": GOLDEN_LAUNCH_COUNT,
        "sleep_s": GOLDEN_SLEEP_S,
        "duration_s": time.perf_counter() - started,
        "launches": results,
    }
    return await _write_summary(
        path=GOLDEN_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


# ---- T2 cancel -------------------------------------------------------------


CANCEL_LAUNCH_COUNT = 3
CANCEL_AFTER_S = 1.0
CANCEL_SLEEP_S = 30


async def run_background_shell_stop_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """T2: launch + cancel mid-flight via asyncio.wait_for."""
    started = time.perf_counter()
    await _wait_for_background_drain(metadata)

    async def _one(index: int) -> dict[str, Any]:
        t0 = time.perf_counter()
        try:
            result = await asyncio.wait_for(
                call_tool(
                    exec_command_tool,
                    {
                        "command": (f"sleep {CANCEL_SLEEP_S}; echo done-{index}"),
                        "timeout": 120,
                    },
                    metadata,
                    emit,
                    background_task_id=_bg_id(f"cancel-{index}"),
                ),
                timeout=CANCEL_AFTER_S,
            )
            record_tool_check(f"tool.exec_command.background_shell.cancel.{index}", result)
            payload = _command_payload(result)
            return {
                "index": index,
                "duration_s": time.perf_counter() - t0,
                "cancelled": False,
                "exit_code": int(payload.get("exit_code", -1)),
                "status": str(payload.get("status") or ""),
                "is_error": bool(result.is_error),
                "shell_metadata": _command_metadata(result),
            }
        except asyncio.TimeoutError:
            return {
                "index": index,
                "duration_s": time.perf_counter() - t0,
                "cancelled": True,
                "exit_code": None,
                "status": "cancelled",
                "is_error": False,
                "shell_metadata": {},
            }

    cancel_results = await asyncio.gather(
        *(_one(i) for i in range(CANCEL_LAUNCH_COUNT)),
        return_exceptions=False,
    )
    await _wait_for_background_drain(metadata)

    # AC-3: post-cancel foreground command mount latency budget.
    fg_t0 = time.perf_counter()
    fg_result = await call_tool(
        exec_command_tool,
        {"command": "echo post-cancel-ok", "timeout": 30},
        metadata,
        emit,
    )
    record_tool_check("tool.exec_command.background_shell.cancel.post_foreground", fg_result)
    post_fg = {
        "duration_s": time.perf_counter() - fg_t0,
        "is_error": bool(fg_result.is_error),
        "shell_metadata": _command_metadata(fg_result),
    }

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "cancel",
        "launch_count": CANCEL_LAUNCH_COUNT,
        "cancel_after_s": CANCEL_AFTER_S,
        "sleep_s": CANCEL_SLEEP_S,
        "duration_s": time.perf_counter() - started,
        "launches": cancel_results,
        "post_cancel_foreground": post_fg,
    }
    return await _write_summary(
        path=STOP_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


# ---- T3 interleave --------------------------------------------------------


INTERLEAVE_FOREGROUND_COUNT = 5
INTERLEAVE_BACKGROUND_SLEEP_S = 30


async def run_background_shell_interleave_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """T3: 1 long background + M foreground commands; capture fg mount p95."""
    started = time.perf_counter()

    bg_task = asyncio.create_task(
        call_tool(
            exec_command_tool,
            {
                "command": (f"sleep {INTERLEAVE_BACKGROUND_SLEEP_S}; echo bg-done"),
                "timeout": 120,
            },
            metadata,
            emit,
            background_task_id=_bg_id("interleave-bg"),
        )
    )

    foreground_records: list[dict[str, Any]] = []
    try:
        for index in range(INTERLEAVE_FOREGROUND_COUNT):
            t0 = time.perf_counter()
            fg_result = await call_tool(
                exec_command_tool,
                {"command": f"echo fg-{index}", "timeout": 30},
                metadata,
                emit,
            )
            record_tool_check(
                f"tool.exec_command.background_shell.interleave.fg.{index}", fg_result
            )
            duration = time.perf_counter() - t0
            command_meta = _command_metadata(fg_result)
            mount_s = (
                float(command_meta["timings"].get("api.exec_command.total_s", 0.0))
                or duration
            )
            foreground_records.append(
                {
                    "index": index,
                    "wall_duration_s": duration,
                    "mount_s": mount_s,
                    "is_error": bool(fg_result.is_error),
                    "shell_metadata": command_meta,
                }
            )
    finally:
        try:
            bg_result = await asyncio.wait_for(bg_task, timeout=INTERLEAVE_BACKGROUND_SLEEP_S + 30)
            bg_payload = _command_payload(bg_result)
            bg_record = {
                "cancelled": False,
                "exit_code": int(bg_payload.get("exit_code", -1)),
                "status": str(bg_payload.get("status") or ""),
                "is_error": bool(bg_result.is_error),
                "shell_metadata": _command_metadata(bg_result),
            }
        except asyncio.TimeoutError:
            bg_task.cancel()
            bg_record = {
                "cancelled": True,
                "exit_code": None,
                "status": "cancelled",
                "is_error": False,
                "shell_metadata": {},
            }

    p95_mount_s = _percentile([r["mount_s"] for r in foreground_records], 95.0)

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "interleave",
        "foreground_count": INTERLEAVE_FOREGROUND_COUNT,
        "background_sleep_s": INTERLEAVE_BACKGROUND_SLEEP_S,
        "duration_s": time.perf_counter() - started,
        "foreground_p95_mount_s": p95_mount_s,
        "foreground": foreground_records,
        "background": bg_record,
    }
    return await _write_summary(
        path=INTERLEAVE_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


# ---- T5 executor exhaustion -----------------------------------------------


EXHAUSTION_LAUNCH_COUNT = 40
EXHAUSTION_BACKGROUND_SLEEP_S = 60
EXHAUSTION_CANCEL_DEADLINE_S = 2.0


async def run_background_shell_exhaustion_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """T5: N command-session launches cancelled in unison; assert AC-14 read budget."""
    started = time.perf_counter()

    async def _launch_one(index: int) -> dict[str, Any]:
        result = await _call_probe_tool(
            label=f"exhaustion.launch.{index}",
            tool_obj=exec_command_tool,
            raw_input={
                "command": (f"sleep {EXHAUSTION_BACKGROUND_SLEEP_S}; echo done-{index}"),
                "timeout": EXHAUSTION_BACKGROUND_SLEEP_S + 30,
                "yield_time_ms": 50,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
            allow_error=True,
        )
        payload = _json_payload(result)
        return {
            "index": index,
            "is_error": bool(result.is_error),
            "status": payload.get("status") or result.metadata.get("status"),
            "command_session_id": payload.get("command_session_id")
            or result.metadata.get("command_session_id"),
        }

    launch_records = await asyncio.gather(
        *(_launch_one(i) for i in range(EXHAUSTION_LAUNCH_COUNT)),
        return_exceptions=False,
    )
    session_ids = [
        str(record["command_session_id"])
        for record in launch_records
        if record.get("command_session_id")
    ]

    async def _cancel_one(command_session_id: str) -> dict[str, Any]:
        result = await _call_probe_tool(
            label=f"exhaustion.cancel.{command_session_id}",
            tool_obj=write_stdin_tool,
            raw_input={
                "command_session_id": command_session_id,
                "chars": "\u0003",
                "yield_time_ms": 50,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
            allow_error=True,
        )
        record = _tool_record(result)
        record["command_session_id"] = command_session_id
        return record

    cancel_t0 = time.perf_counter()
    cancel_records = await asyncio.gather(
        *(_cancel_one(command_session_id) for command_session_id in session_ids),
        return_exceptions=False,
    )
    cancel_elapsed = time.perf_counter() - cancel_t0
    await _wait_for_background_drain(metadata)

    # AC-14: a follow-up foreground read_file must complete in < 1 s, proving
    # the daemon RPC dispatcher executor is NOT shared with CommandExecutor.
    # Seed a target file (write via foreground command) so the read doesn't
    # depend on SWE-EVO repo layout.
    seed_path = f"{ROOT}/exhaustion/probe.txt"
    seed_result = await call_tool(
        exec_command_tool,
        {
            "command": (f"mkdir -p $(dirname {seed_path}) && echo probe-ok > {seed_path}"),
            "timeout": 30,
        },
        metadata,
        emit,
    )
    record_tool_check("tool.exec_command.background_shell.exhaustion.seed", seed_result)
    fg_t0 = time.perf_counter()
    read_result = await call_tool(
        read_file_tool,
        {"file_path": seed_path},
        metadata,
        emit,
    )
    record_tool_check("tool.read_file.background_shell.exhaustion.read", read_result)
    fg_elapsed = time.perf_counter() - fg_t0

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "exhaustion",
        "launch_count": EXHAUSTION_LAUNCH_COUNT,
        "cancel_deadline_s": EXHAUSTION_CANCEL_DEADLINE_S,
        "cancel_elapsed_s": cancel_elapsed,
        "duration_s": time.perf_counter() - started,
        "launches": launch_records,
        "cancellations": cancel_records,
        "outcomes": [
            (
                "cancelled"
                if not record.get("is_error") and record.get("status") == "cancelled"
                else f"error:{record.get('status') or 'unknown'}"
            )
            for record in cancel_records
        ],
        "cancelled_count": sum(
            1
            for record in cancel_records
            if not record.get("is_error") and record.get("status") == "cancelled"
        ),
        "ok_count": sum(1 for record in launch_records if not record.get("is_error")),
        "error_count": (
            sum(1 for record in launch_records if record.get("is_error"))
            + sum(1 for record in cancel_records if record.get("is_error"))
            + (EXHAUSTION_LAUNCH_COUNT - len(session_ids))
        ),
        "post_exhaustion_read_s": fg_elapsed,
        "post_exhaustion_read_error": bool(read_result.is_error),
    }
    return await _write_summary(
        path=EXHAUSTION_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


# ---- T6 partial-write cancel ----------------------------------------------


PARTIAL_WRITE_DD_COUNT_MB = 256
PARTIAL_WRITE_CANCEL_S = 2.0


async def run_background_shell_partial_write_cancel_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """T6: cancel a long ``dd`` mid-write; assert no leaked OCC publish."""
    started = time.perf_counter()
    target = f"{ROOT}/partial_write/tracked.bin"

    # Seed the parent directory via a separate foreground command. We have to
    # also create a sentinel file inside the dir because OCC only persists
    # files, not empty directories — without the sentinel the dd command would
    # land in a fresh lease whose snapshot has lost the dir.
    seed_result = await call_tool(
        exec_command_tool,
        {
            "command": (f"mkdir -p $(dirname {target}) && touch $(dirname {target})/.sentinel"),
            "timeout": 30,
        },
        metadata,
        emit,
    )
    record_tool_check("tool.exec_command.background_shell.partial_write.seed_dir", seed_result)

    dd_command = (
        f"for _ in 1; do "
        f"dd if=/dev/urandom of={target} "
        f"bs=1M count={PARTIAL_WRITE_DD_COUNT_MB} status=none; "
        f"done"
    )
    dd_completed = False
    try:
        result = await asyncio.wait_for(
            call_tool(
                exec_command_tool,
                {"command": dd_command, "timeout": 180},
                metadata,
                emit,
                background_task_id=_bg_id("partial-write"),
            ),
            timeout=PARTIAL_WRITE_CANCEL_S,
        )
        record_tool_check("tool.exec_command.background_shell.partial_write.dd", result)
        dd_completed = True
    except asyncio.TimeoutError:
        pass

    # ``read_file_tool`` raises is_error=True when the file doesn't exist;
    # pass allow_error so the probe can record the absence and assert on it
    # in the test instead of crashing the executor.
    read_result = await call_tool(
        read_file_tool,
        {"file_path": target},
        metadata,
        emit,
        allow_error=True,
    )

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "partial_write_cancel",
        "target": target,
        "dd_count_mb": PARTIAL_WRITE_DD_COUNT_MB,
        "cancel_deadline_s": PARTIAL_WRITE_CANCEL_S,
        "duration_s": time.perf_counter() - started,
        "dd_completed_before_cancel": dd_completed,
        "tracked_exists_after_cancel": not read_result.is_error,
        "read_is_error": bool(read_result.is_error),
    }
    return await _write_summary(
        path=PARTIAL_WRITE_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


# ---- T7 cancel-during-maintenance -----------------------------------------


async def run_background_shell_maintenance_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """T7: short shell + maintenance; verify OCC consistency after."""
    started = time.perf_counter()
    await _wait_for_background_drain(metadata)
    target = f"{ROOT}/maintenance/maint_test.txt"
    target_relative = target.removeprefix(f"{WORKSPACE_ROOT}/")
    result = await call_tool(
        exec_command_tool,
        {
            "command": (
                f"mkdir -p $(dirname {target}) && echo 'maintenance-test' > {target} && sleep 0.5"
            ),
            "timeout": 60,
        },
        metadata,
        emit,
        background_task_id=_bg_id("maintenance"),
    )
    record_tool_check("tool.exec_command.background_shell.maintenance.short_write", result)
    payload = _command_payload(result)
    changed = list(_command_metadata(result)["changed_paths"])

    read_result = await call_tool(
        read_file_tool,
        {"file_path": target},
        metadata,
        emit,
        allow_error=True,
    )
    record_tool_check("tool.read_file.background_shell.maintenance.fg_check", read_result)

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "cancel_during_maintenance",
        "target": target,
        "target_relative": target_relative,
        "duration_s": time.perf_counter() - started,
        "shell_is_error": bool(result.is_error),
        "shell_exit_code": int(payload.get("exit_code", -1)),
        "changed_paths": changed,
        "target_in_changed_paths": (target_relative in changed or target in changed),
        "read_exists": not read_result.is_error,
        "read_content_contains_marker": "maintenance-test" in str(read_result.output or ""),
    }
    return await _write_summary(
        path=MAINTENANCE_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


# ---- T8 late-cancel race ---------------------------------------------------


async def run_background_shell_late_cancel_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """T8: await full completion; late cancel must not mutate result."""
    started = time.perf_counter()
    result = await call_tool(
        exec_command_tool,
        {"command": "sleep 1; echo done-late-cancel", "timeout": 60},
        metadata,
        emit,
        background_task_id=_bg_id("late-cancel"),
    )
    record_tool_check("tool.exec_command.background_shell.late_cancel.short", result)
    payload = _command_payload(result)

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "late_cancel_race",
        "duration_s": time.perf_counter() - started,
        "shell_is_error": bool(result.is_error),
        "exit_code": int(payload.get("exit_code", -1)),
        "status": str(payload.get("status") or ""),
        "stdout_contains_marker": "done-late-cancel" in str(payload.get("stdout") or ""),
        "shell_metadata": _command_metadata(result),
    }
    return await _write_summary(
        path=LATE_CANCEL_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


# ---- 3.3.1 mixed foreground/background conflict ---------------------------


MIXED_CONFLICT_BG_SLEEP_S = 1.5


async def run_background_mixed_fg_bg_same_path_conflict_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """3.4.1: foreground direct write wins over a sleeping background command."""
    started = time.perf_counter()
    target = f"{ROOT}/mixed_fg_bg_same_path_conflict/bg-shared.txt"

    bg_task = asyncio.create_task(
        _call_probe_tool(
            label="mixed_conflict.background",
            tool_obj=exec_command_tool,
            raw_input={
                "command": (
                    f"mkdir -p $(dirname {target}) && "
                    f"sleep {MIXED_CONFLICT_BG_SLEEP_S} && "
                    f"printf 'background-win\\n' > {target}"
                ),
                "timeout": 60,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
            allow_error=True,
            background_task_id=_bg_id("mixed-conflict"),
        )
    )
    await asyncio.sleep(0.35)

    fg_t0 = time.perf_counter()
    fg_result = await _call_probe_tool(
        label="mixed_conflict.foreground_write",
        tool_obj=write_file_tool,
        raw_input={"file_path": target, "content": "foreground-win\n"},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        allow_error=True,
    )
    foreground = {
        **_tool_record(fg_result),
        "duration_s": time.perf_counter() - fg_t0,
    }
    background = await _await_task_record(bg_task)
    final_read = await _call_probe_tool(
        label="mixed_conflict.final_read",
        tool_obj=read_file_tool,
        raw_input={"file_path": target},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        allow_error=True,
    )
    final_content = _read_content(final_read)

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "mixed_fg_bg_same_path_conflict",
        "target": target,
        "duration_s": time.perf_counter() - started,
        "foreground": foreground,
        "background": background,
        "final_read_is_error": bool(final_read.is_error),
        "final_content": final_content,
        "foreground_won": "foreground-win" in final_content,
        "background_won": "background-win" in final_content,
    }
    return await _write_summary(
        path=MIXED_CONFLICT_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


# ---- 3.4.2 heartbeat loss --------------------------------------------------


HEARTBEAT_PROTECTED_SLEEP_S = 4
HEARTBEAT_STALE_SLEEP_S = 20
HEARTBEAT_STALE_WAIT_S = 2.6


async def run_background_heartbeat_loss_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """3.4.2: one command session completes while another is cancelled without publish."""
    started = time.perf_counter()
    sandbox_id = str(metadata.sandbox_id or "")
    agent_id = _agent_id(metadata)
    protected_target = f"{ROOT}/heartbeat_loss/protected.txt"
    stale_target = f"{ROOT}/heartbeat_loss/stale.txt"

    protected_task = asyncio.create_task(
        _call_probe_tool(
            label="heartbeat_loss.protected",
            tool_obj=exec_command_tool,
            raw_input={
                "command": (
                    f"mkdir -p $(dirname {protected_target}) && "
                    f"sleep {HEARTBEAT_PROTECTED_SLEEP_S} && "
                    f"echo protected-ok > {protected_target}"
                ),
                "timeout": 60,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
            allow_error=True,
            background_task_id=_bg_id("heartbeat-protected"),
        )
    )
    stale_task = asyncio.create_task(
        _call_probe_tool(
            label="heartbeat_loss.stale",
            tool_obj=exec_command_tool,
            raw_input={
                "command": (
                    f"mkdir -p $(dirname {stale_target}) && "
                    f"sleep {HEARTBEAT_STALE_SLEEP_S} && "
                    f"echo stale-published > {stale_target}"
                ),
                "timeout": 60,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
            allow_error=True,
            background_task_id=_bg_id("heartbeat-stale"),
        )
    )

    command_sessions_during_launch = await _wait_for_command_session_count(
        sandbox_id=sandbox_id,
        agent_id=agent_id,
        minimum=2,
    )
    await asyncio.sleep(HEARTBEAT_STALE_WAIT_S)

    fg_t0 = time.perf_counter()
    foreground = await _call_probe_tool(
        label="heartbeat_loss.foreground",
        tool_obj=exec_command_tool,
        raw_input={"command": "echo heartbeat-foreground-ok", "timeout": 30},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        allow_error=True,
    )
    foreground_record = {
        **_tool_record(foreground),
        "duration_s": time.perf_counter() - fg_t0,
        "payload": _command_payload(foreground),
    }

    protected_record = await _await_task_record(protected_task)
    stale_task.cancel()
    stale_record = await _await_task_record(stale_task)

    protected_read = await _call_probe_tool(
        label="heartbeat_loss.protected_read",
        tool_obj=read_file_tool,
        raw_input={"file_path": protected_target},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        allow_error=True,
    )
    stale_read = await _call_probe_tool(
        label="heartbeat_loss.stale_read",
        tool_obj=read_file_tool,
        raw_input={"file_path": stale_target},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=None,
        allow_error=True,
    )
    command_sessions_after = await sandbox_api.command_session_count(sandbox_id, agent_id)

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "heartbeat_loss",
        "duration_s": time.perf_counter() - started,
        "command_sessions_during_launch": command_sessions_during_launch,
        "command_sessions_after": command_sessions_after,
        "foreground": foreground_record,
        "protected": protected_record,
        "stale": stale_record,
        "protected_published": "protected-ok" in _read_content(protected_read),
        "stale_published": "stale-published" in _read_content(stale_read),
    }
    return await _write_summary(
        path=HEARTBEAT_LOSS_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


# ---- 3.4.3 isolated-workspace drain ---------------------------------------


async def run_background_exit_iws_drains_agent_tasks_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """3.4.3: iws enter rejects in-flight default bg and exit drains iws bg."""
    started = time.perf_counter()
    sandbox_id = str(metadata.sandbox_id or "")
    agent_id = _agent_id(metadata)
    default_target = f"{ROOT}/exit_iws_drain/default-should-not-publish.txt"
    iws_target = f"{ROOT}/exit_iws_drain/iws-should-not-publish.txt"

    default_task = asyncio.create_task(
        _call_probe_tool(
            label="exit_iws.default_background",
            tool_obj=exec_command_tool,
            raw_input={
                "command": (
                    f"sleep 20 && mkdir -p $(dirname {default_target}) && "
                    f"echo default-leak > {default_target}"
                ),
                "timeout": 60,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
            allow_error=True,
            background_task_id=_bg_id("iws-default-blocker"),
        )
    )
    default_command_sessions = await _wait_for_command_session_count(
        sandbox_id=sandbox_id,
        agent_id=agent_id,
        minimum=1,
    )
    blocked_enter = await _call_probe_tool(
        label="exit_iws.blocked_enter",
        tool_obj=enter_isolated_workspace_tool,
        raw_input={},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=None,
        allow_error=True,
    )
    default_task.cancel()
    default_record = await _await_task_record(default_task)

    iws_metadata = metadata.copy()
    iws_metadata.agent_name = "iws-exit-agent"
    iws_metadata.agent_run_id = f"{agent_id}:iws-exit"
    iws_metadata["layer_stack_root"] = BACKGROUND_IWS_LAYER_STACK_ROOT
    manager = BackgroundTaskSupervisor()
    iws_metadata.background_task_manager = manager

    iws_enter = await _call_probe_tool(
        label="exit_iws.enter_other_agent",
        tool_obj=enter_isolated_workspace_tool,
        raw_input={"layer_stack_root": BACKGROUND_IWS_LAYER_STACK_ROOT},
        metadata=iws_metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        allow_error=True,
    )
    iws_command = f"sleep 20 && mkdir -p $(dirname {iws_target}) && echo iws-leak > {iws_target}"
    task_id = manager.next_alias()
    manager.launch(
        task_id,
        "exec_command",
        {"cmd": iws_command, "timeout": 60},
        _call_probe_tool(
            label="exit_iws.iws_background",
            tool_obj=exec_command_tool,
            raw_input={"command": iws_command, "timeout": 60},
            metadata=iws_metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
            allow_error=True,
            background_task_id=task_id,
        ),
        agent_id=_agent_id(iws_metadata),
        uses_sandbox=True,
        sandbox_id=sandbox_id,
        sandbox_invocation_id=f"iws-manager-{uuid4().hex}",
    )
    await asyncio.sleep(0.5)
    # Exit is gated by the bg prehook: with a live sandbox-bound command session it
    # is refused (a hook_failure), not silently drained. The agent must cancel
    # the command session, then retry exit.
    blocked_exit = await _call_probe_tool(
        label="exit_iws.blocked_exit",
        tool_obj=exit_isolated_workspace_tool,
        raw_input={"grace_s": 1.0},
        metadata=iws_metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=None,
        allow_error=True,
    )
    cancel_bg = await _call_probe_tool(
        label="exit_iws.cancel_bg",
        tool_obj=write_stdin_tool,
        raw_input={
            "command_session_id": task_id,
            "chars": "\u0003",
            "yield_time_ms": 50,
        },
        metadata=iws_metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=None,
        allow_error=True,
    )
    # Let both the bridge task and daemon command session registry settle before retrying
    # so the retry's max(local, daemon) check sees no in-flight work.
    tracked_status = await _wait_for_tracked_task_settled(manager, task_id)
    await _wait_for_background_drain(iws_metadata)
    iws_exit = await _call_probe_tool(
        label="exit_iws.exit_after_cancel",
        tool_obj=exit_isolated_workspace_tool,
        raw_input={"grace_s": 1.0},
        metadata=iws_metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        allow_error=True,
    )
    iws_command_sessions_after = await sandbox_api.command_session_count(
        sandbox_id,
        _agent_id(iws_metadata),
    )
    tracked = manager.get_task(task_id)

    default_read = await _call_probe_tool(
        label="exit_iws.default_read",
        tool_obj=read_file_tool,
        raw_input={"file_path": default_target},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=None,
        allow_error=True,
    )
    iws_read = await _call_probe_tool(
        label="exit_iws.iws_read",
        tool_obj=read_file_tool,
        raw_input={"file_path": iws_target},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=None,
        allow_error=True,
    )

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "exit_iws_drain",
        "duration_s": time.perf_counter() - started,
        "default_command_sessions": default_command_sessions,
        "blocked_enter": _tool_record(blocked_enter),
        "blocked_enter_payload": _json_payload(blocked_enter),
        "blocked_enter_reason": _hook_failure_reason(blocked_enter),
        "default_background": default_record,
        "iws_enter": _tool_record(iws_enter),
        "iws_enter_payload": _json_payload(iws_enter),
        "blocked_exit": _tool_record(blocked_exit),
        "blocked_exit_reason": _hook_failure_reason(blocked_exit),
        "cancel_bg": _tool_record(cancel_bg),
        "iws_exit": _tool_record(iws_exit),
        "iws_exit_payload": _json_payload(iws_exit),
        "tracked_status_after_cancel": tracked_status,
        "tracked_status_after_exit": (
            str(tracked.status.value) if tracked is not None else "missing"
        ),
        "iws_command_sessions_after": iws_command_sessions_after,
        "default_published": "default-leak" in _read_content(default_read),
        "iws_published": "iws-leak" in _read_content(iws_read),
    }
    return await _write_summary(
        path=EXIT_IWS_DRAIN_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


# ---- 3.4.4 engine restart / abandon ---------------------------------------


ENGINE_RESTART_STALE_WAIT_S = 2.6


async def run_background_engine_restart_no_lease_leak_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """3.4.4: abandon a command session, then prove foreground recovery."""
    started = time.perf_counter()
    sandbox_id = str(metadata.sandbox_id or "")
    agent_id = _agent_id(metadata)
    abandoned_target = f"{ROOT}/engine_restart/abandoned.txt"
    recovery_target = f"{ROOT}/engine_restart/recovery.txt"

    abandoned_task = asyncio.create_task(
        _call_probe_tool(
            label="engine_restart.abandoned",
            tool_obj=exec_command_tool,
            raw_input={
                "command": (
                    f"mkdir -p $(dirname {abandoned_target}) && "
                    "for i in 1 2 3 4 5; do "
                    f"echo chunk-$i >> {abandoned_target}; sleep 1; "
                    "done"
                ),
                "timeout": 60,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
            allow_error=True,
            background_task_id=_bg_id("engine-abandon"),
        )
    )
    command_sessions_during_launch = await _wait_for_command_session_count(
        sandbox_id=sandbox_id,
        agent_id=agent_id,
        minimum=1,
    )
    await asyncio.sleep(ENGINE_RESTART_STALE_WAIT_S)
    abandoned_task.cancel()
    abandoned_record = await _await_task_record(abandoned_task)

    partial_read = await _call_probe_tool(
        label="engine_restart.abandoned_read",
        tool_obj=read_file_tool,
        raw_input={"file_path": abandoned_target},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=None,
        allow_error=True,
    )
    fg_shell = await _call_probe_tool(
        label="engine_restart.foreground_shell",
        tool_obj=exec_command_tool,
        raw_input={"command": "echo engine-restart-foreground-ok", "timeout": 30},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        allow_error=True,
    )
    recovery_write = await _call_probe_tool(
        label="engine_restart.recovery_write",
        tool_obj=write_file_tool,
        raw_input={"file_path": recovery_target, "content": "recovery-ok\n"},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        allow_error=True,
    )
    recovery_read = await _call_probe_tool(
        label="engine_restart.recovery_read",
        tool_obj=read_file_tool,
        raw_input={"file_path": recovery_target},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        allow_error=True,
    )
    command_sessions_after = await sandbox_api.command_session_count(sandbox_id, agent_id)

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "engine_restart_no_lease_leak",
        "duration_s": time.perf_counter() - started,
        "command_sessions_during_launch": command_sessions_during_launch,
        "command_sessions_after": command_sessions_after,
        "abandoned": abandoned_record,
        "abandoned_published": "chunk-" in _read_content(partial_read),
        "foreground_shell": {
            **_tool_record(fg_shell),
            "payload": _command_payload(fg_shell),
        },
        "recovery_write": _tool_record(recovery_write),
        "recovery_read_content": _read_content(recovery_read),
    }
    return await _write_summary(
        path=ENGINE_RESTART_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


# ---- 3.4.5 dispatcher under many small writes -----------------------------


MANY_SMALL_WRITES_BACKGROUND_COUNT = int(
    os.getenv("EOS_BACKGROUND_MANY_SMALL_WRITES_BACKGROUND_COUNT", "16")
)
MANY_SMALL_WRITES_FOREGROUND_COUNT = int(
    os.getenv("EOS_BACKGROUND_MANY_SMALL_WRITES_FOREGROUND_COUNT", "8")
)


async def run_background_many_small_writes_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """3.4.5: many small background command writes with foreground file calls."""
    started = time.perf_counter()
    sandbox_id = str(metadata.sandbox_id or "")
    agent_id = _agent_id(metadata)
    root = f"{ROOT}/many_small_writes"
    seed_path = f"{root}/foreground-seed.txt"
    await _call_probe_tool(
        label="many_small_writes.seed",
        tool_obj=write_file_tool,
        raw_input={"file_path": seed_path, "content": "seed\n"},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )

    async def _background_one(index: int) -> dict[str, Any]:
        path = f"{root}/bg-{index}.txt"
        task = asyncio.create_task(
            _call_probe_tool(
                label=f"many_small_writes.bg.{index}",
                tool_obj=exec_command_tool,
                raw_input={
                    "command": (f"mkdir -p {root} && echo bg-{index} > {path} && sleep 0.2"),
                    "timeout": 30,
                },
                metadata=metadata,
                emit=emit,
                call_tool=call_tool,
                record_tool_check=record_tool_check,
                allow_error=True,
                background_task_id=_bg_id(f"many-{index}"),
            )
        )
        record = await _await_task_record(task)
        record["path"] = path
        return record

    bg_tasks = [
        asyncio.create_task(_background_one(index))
        for index in range(MANY_SMALL_WRITES_BACKGROUND_COUNT)
    ]
    foreground_records: list[dict[str, Any]] = []
    for index in range(MANY_SMALL_WRITES_FOREGROUND_COUNT):
        write_path = f"{root}/fg-{index}.txt"
        t0 = time.perf_counter()
        write = await _call_probe_tool(
            label=f"many_small_writes.fg_write.{index}",
            tool_obj=write_file_tool,
            raw_input={"file_path": write_path, "content": f"fg-{index}\n"},
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=record_tool_check,
            allow_error=True,
        )
        read = await _call_probe_tool(
            label=f"many_small_writes.fg_read.{index}",
            tool_obj=read_file_tool,
            raw_input={"file_path": seed_path},
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=record_tool_check,
            allow_error=True,
        )
        foreground_records.append(
            {
                "index": index,
                "duration_s": time.perf_counter() - t0,
                "write": _tool_record(write),
                "read_is_error": bool(read.is_error),
            }
        )

    background_records = await asyncio.gather(*bg_tasks)
    verify_records: list[dict[str, Any]] = []
    for record in background_records:
        if record.get("is_error"):
            continue
        path = str(record["path"])
        read = await _call_probe_tool(
            label=f"many_small_writes.verify.{path.rsplit('/', 1)[-1]}",
            tool_obj=read_file_tool,
            raw_input={"file_path": path},
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=record_tool_check,
            allow_error=True,
        )
        verify_records.append(
            {
                "path": path,
                "is_error": bool(read.is_error),
                "content": _read_content(read),
            }
        )
    command_sessions_after = await sandbox_api.command_session_count(sandbox_id, agent_id)
    fg_durations = [float(item["duration_s"]) for item in foreground_records]

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "many_small_writes",
        "duration_s": time.perf_counter() - started,
        "background_count": MANY_SMALL_WRITES_BACKGROUND_COUNT,
        "foreground_count": MANY_SMALL_WRITES_FOREGROUND_COUNT,
        "foreground_p95_s": _percentile(fg_durations, 95.0),
        "background": background_records,
        "foreground": foreground_records,
        "verified_background_files": verify_records,
        "command_sessions_after": command_sessions_after,
        "background_success_count": sum(
            1 for item in background_records if not item.get("is_error")
        ),
    }
    return await _write_summary(
        path=MANY_SMALL_WRITES_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


# ---- 3.4.6 mixed-op concurrent background tasks ---------------------------


MIXED_OP_OVERLAP_WRITERS = 4
MIXED_OP_DISJOINT_WRITERS = 4
MIXED_OP_EDIT_LINES = 20


def _publish_accepted(record: dict[str, Any]) -> bool:
    return not bool(record.get("is_error"))


async def run_background_mixed_op_concurrent_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """3.4.6: heterogeneous + conflicting + disjoint concurrent background work.

    Three independent assertions land in one summary:

    * **Mixed ops** — a ``pytest`` run, a ``pip install``, and a ``python``
      edit-loop launched as concurrent background tasks each reach a terminal
      status (none stuck), proving the supervisor drives heterogeneous
      workloads to completion. (``pip install`` is offline/``--no-index`` so it
      terminates fast and deterministically without a network dependency.)
    * **Overlapping same-file edits** — N background command sessions overwrite one
      seeded path concurrently; each command reaches a terminal status, and the
      final file content is one complete writer payload.
    * **Disjoint edits** — N background commands write distinct paths; all land
      and read back their own content.
    """
    started = time.perf_counter()
    root = f"{ROOT}/mixed_op_concurrent"
    await _call_probe_tool(
        label="mixed_op.seed_root",
        tool_obj=exec_command_tool,
        raw_input={"command": f"mkdir -p {root}", "timeout": 30},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )

    # --- mixed heterogeneous ops, all must reach a terminal status ----------
    pytest_file = f"{root}/t_probe.py"
    await _call_probe_tool(
        label="mixed_op.seed_pytest",
        tool_obj=write_file_tool,
        raw_input={
            "file_path": pytest_file,
            "content": "def test_ok():\n    assert 1 + 1 == 2\n",
        },
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )
    edit_target = f"{root}/edit-loop.txt"
    mixed_specs = {
        "pytest": f"python3 -m pytest -q {pytest_file}",
        "pip": (
            "python3 -m pip install --no-input --disable-pip-version-check "
            "--no-index eos-nonexistent-probe-pkg"
        ),
        "edit_loop": (
            f"python3 -c \"open('{edit_target}','w').write("
            f"''.join('line-%d\\n' % i for i in range({MIXED_OP_EDIT_LINES})))\""
        ),
    }
    mixed_tasks = {
        name: asyncio.create_task(
            _call_probe_tool(
                label=f"mixed_op.mixed.{name}",
                tool_obj=exec_command_tool,
                raw_input={"command": command, "timeout": 120},
                metadata=metadata,
                emit=emit,
                call_tool=call_tool,
                record_tool_check=None,
                allow_error=True,
                background_task_id=_bg_id(f"mixed-{name}"),
            )
        )
        for name, command in mixed_specs.items()
    }
    mixed_records: dict[str, dict[str, Any]] = {}
    for name, task in mixed_tasks.items():
        record = await _await_task_record(task)
        record["terminal"] = (not record.get("cancelled")) and (
            record.get("exit_code") is not None or bool(record.get("status"))
        )
        mixed_records[name] = record

    # --- overlapping same-file command session edits converge to one final writer -------
    shared = f"{root}/overlap-shared.txt"
    await _call_probe_tool(
        label="mixed_op.overlap.seed",
        tool_obj=write_file_tool,
        raw_input={"file_path": shared, "content": "owner=seed\n"},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )

    async def _overlap_one(index: int) -> ToolResult:
        return await _call_probe_tool(
            label=f"mixed_op.overlap.{index}",
            tool_obj=exec_command_tool,
            raw_input={
                "command": f"sleep 0.3; printf 'writer-{index}\\n' > {shared}",
                "timeout": 60,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
            allow_error=True,
            background_task_id=_bg_id(f"overlap-{index}"),
        )

    overlap_results = await asyncio.gather(
        *(_overlap_one(index) for index in range(MIXED_OP_OVERLAP_WRITERS))
    )
    overlap_writers = [
        {
            "index": index,
            **_tool_record(result),
            "accepted": _publish_accepted(_tool_record(result)),
        }
        for index, result in enumerate(overlap_results)
    ]
    overlap_final = await _call_probe_tool(
        label="mixed_op.overlap.final_read",
        tool_obj=read_file_tool,
        raw_input={"file_path": shared},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        allow_error=True,
    )
    overlap_final_content = _read_content(overlap_final)

    # --- disjoint edits all land --------------------------------------------
    async def _disjoint_one(index: int) -> tuple[str, ToolResult]:
        path = f"{root}/disjoint-{index}.txt"
        result = await _call_probe_tool(
            label=f"mixed_op.disjoint.{index}",
            tool_obj=exec_command_tool,
            raw_input={
                "command": f"sleep 0.3; printf 'disjoint-{index}\\n' > {path}",
                "timeout": 60,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
            allow_error=True,
            background_task_id=_bg_id(f"disjoint-{index}"),
        )
        return path, result

    disjoint_pairs = await asyncio.gather(
        *(_disjoint_one(index) for index in range(MIXED_OP_DISJOINT_WRITERS))
    )
    disjoint_writers = [
        {"path": path, **_tool_record(result), "accepted": _publish_accepted(_tool_record(result))}
        for path, result in disjoint_pairs
    ]
    disjoint_readbacks: dict[str, str] = {}
    for path, _ in disjoint_pairs:
        read = await _call_probe_tool(
            label=f"mixed_op.disjoint.read.{path.rsplit('/', 1)[-1]}",
            tool_obj=read_file_tool,
            raw_input={"file_path": path},
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
            allow_error=True,
        )
        disjoint_readbacks[path] = _read_content(read)

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "mixed_op_concurrent",
        "duration_s": time.perf_counter() - started,
        "mixed": mixed_records,
        "overlap": {
            "shared": shared,
            "writers": overlap_writers,
            "accepted_count": sum(1 for w in overlap_writers if w["accepted"]),
            "aborted_count": sum(1 for w in overlap_writers if not w["accepted"]),
            "final_content": overlap_final_content,
        },
        "disjoint": {
            "writers": disjoint_writers,
            "accepted_count": sum(1 for w in disjoint_writers if w["accepted"]),
            "readbacks": disjoint_readbacks,
        },
    }
    return await _write_summary(
        path=MIXED_OP_CONCURRENT_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


__all__ = [
    "GOLDEN_SUMMARY",
    "STOP_SUMMARY",
    "INTERLEAVE_SUMMARY",
    "EXHAUSTION_SUMMARY",
    "PARTIAL_WRITE_SUMMARY",
    "MAINTENANCE_SUMMARY",
    "LATE_CANCEL_SUMMARY",
    "MIXED_CONFLICT_SUMMARY",
    "HEARTBEAT_LOSS_SUMMARY",
    "EXIT_IWS_DRAIN_SUMMARY",
    "ENGINE_RESTART_SUMMARY",
    "MANY_SMALL_WRITES_SUMMARY",
    "MIXED_OP_CONCURRENT_SUMMARY",
    "SUMMARY_SCHEMA",
    "BACKGROUND_IWS_LAYER_STACK_ROOT",
    "run_background_shell_golden_probe",
    "run_background_shell_stop_probe",
    "run_background_shell_interleave_probe",
    "run_background_shell_exhaustion_probe",
    "run_background_shell_partial_write_cancel_probe",
    "run_background_shell_maintenance_probe",
    "run_background_shell_late_cancel_probe",
    "run_background_mixed_fg_bg_same_path_conflict_probe",
    "run_background_heartbeat_loss_probe",
    "run_background_exit_iws_drains_agent_tasks_probe",
    "run_background_engine_restart_no_lease_leak_probe",
    "run_background_many_small_writes_probe",
    "run_background_mixed_op_concurrent_probe",
]
