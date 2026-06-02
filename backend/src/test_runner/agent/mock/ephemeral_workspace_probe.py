"""Live probes for the default ephemeral workspace behavior.

These probes keep the scenario DAG small and put the load-bearing behavior in
real sandbox tool calls. They cover the direct OCC fast path for file verbs
and the per-call overlay path for shell/search/outside-workspace calls. Each
public function writes a summary under
``/testbed/.ephemeralos/sweevo-mock/ephemeral_workspace/<mode>/summary.json``.
"""

from __future__ import annotations

import asyncio
import json
import statistics
import textwrap
import time
from collections.abc import Awaitable, Callable, Mapping
from typing import Any
from uuid import uuid4

import sandbox.api as sandbox_api
from message.events import StreamEvent
from sandbox.api.transport import DaemonSandboxTransport
from tools._framework.core.base import BaseTool
from tools._framework.core.results import ToolResult
from tools._framework.core.runtime import ExecutionMetadata
from tools.sandbox.edit_file import edit_file as edit_file_tool
from tools.sandbox.glob import glob as glob_tool
from tools.sandbox.grep import grep as grep_tool
from tools.sandbox.read_file import read_file as read_file_tool
from tools.sandbox.exec_command import exec_command as exec_command_tool
from tools.sandbox.write_file import write_file as write_file_tool


WORKSPACE_ROOT = "/testbed"
CASE_ROOT = f"{WORKSPACE_ROOT}/eph_case"
ROOT = f"{WORKSPACE_ROOT}/.ephemeralos/sweevo-mock/ephemeral_workspace"
SUMMARY_SCHEMA = "test_runner.ephemeral_workspace.v1"

ALL_VERBS_SUMMARY = f"{ROOT}/all_verbs/summary.json"
CONCURRENT_WRITES_SUMMARY = f"{ROOT}/concurrent_writes/summary.json"
POLICY_SUMMARY = f"{ROOT}/policy/summary.json"
CANCELLATION_SUMMARY = f"{ROOT}/cancellation/summary.json"
O1_DISK_SUMMARY = f"{ROOT}/o1_disk/summary.json"
SAME_PATH_CONFLICT_SUMMARY = f"{ROOT}/same_path_conflict/summary.json"
SAME_PATH_CONFLICT_SHARED_PATH = f"{ROOT}/same_path_conflict/shared.txt"
SAME_PATH_CONFLICT_FRAGMENT_DIR = f"{ROOT}/same_path_conflict/fragments"

EmitStreamEvent = Callable[[StreamEvent], Awaitable[None]]
CallTool = Callable[..., Awaitable[ToolResult]]
RecordToolCheck = Callable[[str, ToolResult], None]


def _case_subroot(mode: str) -> str:
    return f"{CASE_ROOT}/{mode}-{uuid4().hex[:8]}"


def _ignored_case_subroot(mode: str) -> str:
    return f"{ROOT}/cases/{mode}-{uuid4().hex[:8]}"


async def run_ephemeral_all_verbs_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
    sandbox_id: str,
) -> str:
    """Exercise read/write/edit/grep/glob/shell and cleanup after every call."""
    metadata.repo_root = WORKSPACE_ROOT
    before = await _layer_metrics(sandbox_id)
    records: list[dict[str, Any]] = []

    async def call(
        label: str,
        tool_obj: BaseTool,
        raw_input: dict[str, Any],
        *,
        intent: str,
        allow_error: bool = False,
    ) -> ToolResult:
        nonlocal before
        result = await _call_tool(
            label=label,
            tool_obj=tool_obj,
            raw_input=raw_input,
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=record_tool_check if not allow_error else None,
            allow_error=allow_error,
        )
        after = await _layer_metrics(sandbox_id)
        runtime = await _runtime_sample(sandbox_id)
        record = _record(
            label=label,
            tool_name=tool_obj.name,
            result=result,
            intent=intent,
            before=before,
            after=after,
            runtime=runtime,
        )
        _assert_per_call_cleanup(record)
        if intent == "read_only" and record["manifest_before"] != record["manifest_after"]:
            raise RuntimeError(f"{label} unexpectedly published: {record}")
        records.append(record)
        before = after
        return result

    case_root = _ignored_case_subroot("all-verbs")
    module_path = f"{case_root}/pkg/module.py"
    init_path = f"{case_root}/pkg/__init__.py"
    delete_path = f"{case_root}/delete_me.txt"
    opaque_old = f"{case_root}/opaque_dir/old.txt"

    await call(
        "write_ephemeral_gitignore",
        write_file_tool,
        {
            "file_path": f"{WORKSPACE_ROOT}/.gitignore",
            "content": ".ephemeralos/sweevo-mock/ephemeral_workspace/cases/\n",
        },
        intent="write_allowed",
    )
    await call(
        "write_module",
        write_file_tool,
        {
            "file_path": module_path,
            "content": "VALUE = 'alpha'\n\ndef marker():\n    return VALUE\n",
        },
        intent="write_allowed",
    )
    await call(
        "write_init",
        write_file_tool,
        {"file_path": init_path, "content": "from .module import marker\n"},
        intent="write_allowed",
    )
    read = await call(
        "read_module",
        read_file_tool,
        {"file_path": module_path, "start_line": 1, "end_line": 20},
        intent="read_only",
    )
    _assert_contains(_read_content(read), "alpha", "read_module")
    await call(
        "edit_module",
        edit_file_tool,
        {
            "file_path": module_path,
            "old_text": "VALUE = 'alpha'\n",
            "new_text": "VALUE = 'beta'\n",
            "description": "ephemeral all-verbs edit",
        },
        intent="write_allowed",
    )
    grep = await call(
        "grep_beta",
        grep_tool,
        {
            "pattern": "VALUE = 'beta'",
            "path": case_root,
            "output_mode": "files_with_matches",
        },
        intent="read_only",
    )
    _assert_contains(json.dumps(_json(grep)), "pkg/module.py", "grep_beta")
    glob = await call(
        "glob_python",
        glob_tool,
        {"pattern": "**/*.py", "path": case_root},
        intent="read_only",
    )
    _assert_contains(json.dumps(_json(glob)), "pkg/module.py", "glob_python")
    await call(
        "write_delete_target",
        write_file_tool,
        {"file_path": delete_path, "content": "delete me\n"},
        intent="write_allowed",
    )
    await call(
        "write_opaque_old",
        write_file_tool,
        {"file_path": opaque_old, "content": "old\n"},
        intent="write_allowed",
    )
    shell = await call(
        "shell_kinds",
        exec_command_tool,
        {
            "command": textwrap.dedent(
                f"""\
                python - <<'PY'
                from pathlib import Path

                case_root = Path({case_root!r})
                delete_path = Path({delete_path!r})
                try:
                    delete_path.unlink()
                except FileNotFoundError:
                    pass

                link_path = case_root / "current.py"
                try:
                    link_path.unlink()
                except FileNotFoundError:
                    pass
                link_path.symlink_to("pkg/module.py")

                opaque_dir = case_root / "opaque_dir"
                opaque_dir.mkdir(parents=True, exist_ok=True)
                (opaque_dir / ".wh..wh..opq").write_text("", encoding="utf-8")
                (opaque_dir / "new.txt").write_text("new\\n", encoding="utf-8")
                (case_root / "generated.txt").write_text("generated\\n", encoding="utf-8")
                PY
                """
            ),
            "timeout": 120,
        },
        intent="write_allowed",
    )
    _assert_required_kinds(shell)
    final_read = await call(
        "read_shell_result",
        read_file_tool,
        {"file_path": f"{case_root}/generated.txt", "start_line": 1, "end_line": 5},
        intent="read_only",
    )
    _assert_contains(_read_content(final_read), "generated", "read_shell_result")

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "all_verbs",
        "records": records,
        "read_only_publish_count": sum(
            1
            for record in records
            if record["intent"] == "read_only"
            and record["manifest_before"] != record["manifest_after"]
        ),
        "required_shell_kinds": sorted(_changed_path_kinds(shell).values()),
    }
    return await _write_summary(
        path=ALL_VERBS_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def run_ephemeral_concurrent_writes_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
    sandbox_id: str,
) -> str:
    """Launch disjoint typed writes and shell captures concurrently."""
    metadata.repo_root = WORKSPACE_ROOT
    root = _case_subroot("concurrent")
    await _call_tool(
        label="concurrent_seed",
        tool_obj=exec_command_tool,
        raw_input={"command": f"mkdir -p {root}", "timeout": 60},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )

    async def typed_write(index: int) -> ToolResult:
        return await _call_tool(
            label=f"typed_write_{index}",
            tool_obj=write_file_tool,
            raw_input={
                "file_path": f"{root}/typed-{index}.txt",
                "content": f"typed={index}\n",
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=record_tool_check,
        )

    async def shell_write(index: int) -> ToolResult:
        return await _call_tool(
            label=f"shell_write_{index}",
            tool_obj=exec_command_tool,
            raw_input={
                "command": f"printf 'shell={index}\\n' > {root}/shell-{index}.txt",
                "timeout": 60,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=record_tool_check,
        )

    typed = await asyncio.gather(*(typed_write(index) for index in range(8)))
    shells = await asyncio.gather(*(shell_write(index) for index in range(2)))

    readbacks: dict[str, str] = {}
    for index in range(8):
        read = await _call_tool(
            label=f"read_typed_{index}",
            tool_obj=read_file_tool,
            raw_input={
                "file_path": f"{root}/typed-{index}.txt",
                "start_line": 1,
                "end_line": 5,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
        )
        readbacks[f"typed-{index}.txt"] = _read_content(read)
    for index in range(2):
        read = await _call_tool(
            label=f"read_shell_{index}",
            tool_obj=read_file_tool,
            raw_input={
                "file_path": f"{root}/shell-{index}.txt",
                "start_line": 1,
                "end_line": 5,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
        )
        readbacks[f"shell-{index}.txt"] = _read_content(read)

    runtime = await _runtime_sample(sandbox_id)
    typed_sources = [_mutation_source(result) for result in typed]
    shell_sources = [_mutation_source(result) for result in shells]
    if set(typed_sources) != {"api_write"}:
        raise RuntimeError(f"typed writes did not preserve api_write: {typed_sources}")
    if set(shell_sources) != {"overlay_capture"}:
        raise RuntimeError(f"shell captures did not preserve overlay_capture: {shell_sources}")
    if runtime["command_overlay_run_dirs"] != 0:
        raise RuntimeError(f"overlay run dirs leaked after concurrency: {runtime}")

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "concurrent_writes",
        "typed_write_count": len(typed),
        "shell_write_count": len(shells),
        "typed_sources": typed_sources,
        "shell_sources": shell_sources,
        "typed_changed_paths": [_changed_paths(result) for result in typed],
        "shell_changed_paths": [_changed_paths(result) for result in shells],
        "readbacks": readbacks,
        "runtime_after": runtime,
    }
    return await _write_summary(
        path=CONCURRENT_WRITES_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def run_ephemeral_policy_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
    sandbox_id: str,
) -> str:
    """Verify command overlay outside-workspace passthrough is not OCC-published."""
    metadata.repo_root = WORKSPACE_ROOT
    hosts = await _call_tool(
        label="policy_read_hosts",
        tool_obj=exec_command_tool,
        raw_input={"command": "cat /etc/hosts", "timeout": 20},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )
    tmp_write = await _call_tool(
        label="policy_write_tmp",
        tool_obj=exec_command_tool,
        raw_input={"command": "printf 'tmp-ok\\n' > /tmp/eph-scratch.txt", "timeout": 20},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )
    tmp_probe = await sandbox_api.raw_exec(
        sandbox_id,
        "test -f /tmp/eph-scratch.txt && cat /tmp/eph-scratch.txt",
        timeout=20,
    )

    if _changed_paths(tmp_write):
        raise RuntimeError(f"/tmp write entered OCC changed_paths: {tmp_write.metadata}")

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "policy",
        "hosts_read_ok": "localhost" in _stdout(hosts) or bool(_stdout(hosts)),
        "tmp_write_changed_paths": _changed_paths(tmp_write),
        "tmp_probe_stdout": tmp_probe.stdout,
        "outside_command_timing_keys": sorted(
            set(_timings(hosts)) | set(_timings(tmp_write))
        ),
        "outside_command_has_mount_timing": (
            "command_exec.mount_workspace_s" in _timings(hosts)
            and "command_exec.mount_workspace_s" in _timings(tmp_write)
        ),
        "outside_command_has_capture_timing": (
            "command_exec.capture_upperdir_s" in _timings(hosts)
            or "command_exec.capture_upperdir_s" in _timings(tmp_write)
        ),
        "outside_command_has_public_timing": (
            "api.exec_command.dispatch_total_s" in _timings(hosts)
            and "api.exec_command.dispatch_total_s" in _timings(tmp_write)
            and "api.shell.total_s" in _timings(hosts)
            and "api.shell.total_s" in _timings(tmp_write)
        ),
    }
    return await _write_summary(
        path=POLICY_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def run_ephemeral_cancellation_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
    sandbox_id: str,
) -> str:
    """Cancel a partial workspace write and verify the next foreground call."""
    metadata.repo_root = WORKSPACE_ROOT
    root = _case_subroot("cancellation")
    partial_path = f"{root}/partial.bin"
    background_task_id = f"eph-cancel-{uuid4().hex[:8]}"
    started = time.perf_counter()
    cancelled = False
    try:
        await asyncio.wait_for(
            call_tool(
                exec_command_tool,
                {
                    "command": (
                        "python - <<'PY'\n"
                        "import os, time\n"
                        f"path = {partial_path!r}\n"
                        "os.makedirs(os.path.dirname(path), exist_ok=True)\n"
                        "with open(path, 'wb') as handle:\n"
                        "    for _ in range(200):\n"
                        "        handle.write(b'x' * 1048576)\n"
                        "        handle.flush()\n"
                        "        os.fsync(handle.fileno())\n"
                        "        time.sleep(0.05)\n"
                        "PY"
                    ),
                    "timeout": 120,
                },
                metadata,
                emit,
                background_task_id=background_task_id,
            ),
            timeout=1.0,
        )
    except asyncio.TimeoutError:
        cancelled = True
    await _wait_for_background_drain(metadata)

    partial_read = await _call_tool(
        label="cancel_read_partial",
        tool_obj=read_file_tool,
        raw_input={"file_path": partial_path, "start_line": 1, "end_line": 5},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=None,
        allow_error=True,
    )
    health_write = await _call_tool(
        label="cancel_health_write",
        tool_obj=write_file_tool,
        raw_input={"file_path": f"{root}/after_cancel.txt", "content": "ok\n"},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )
    health_read = await _call_tool(
        label="cancel_health_read",
        tool_obj=read_file_tool,
        raw_input={
            "file_path": f"{root}/after_cancel.txt",
            "start_line": 1,
            "end_line": 5,
        },
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )
    runtime = await _runtime_sample(sandbox_id)
    if not cancelled:
        raise RuntimeError("partial shell completed before cancellation deadline")
    if not partial_read.is_error:
        raise RuntimeError("cancelled shell published partial file content")
    if runtime["command_overlay_run_dirs"] != 0:
        raise RuntimeError(f"overlay run dirs leaked after cancellation: {runtime}")

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "cancellation",
        "background_task_id": background_task_id,
        "cancelled": cancelled,
        "duration_s": time.perf_counter() - started,
        "partial_read_is_error": partial_read.is_error,
        "health_write_changed_paths": _changed_paths(health_write),
        "health_read": _read_content(health_read),
        "runtime_after": runtime,
    }
    return await _write_summary(
        path=CANCELLATION_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def run_ephemeral_o1_disk_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
    sandbox_id: str,
) -> str:
    """Run 100 sequential small operations and sample runtime disk cleanup."""
    metadata.repo_root = WORKSPACE_ROOT
    root = _case_subroot("o1")
    base_path = f"{root}/base.txt"
    await _call_tool(
        label="o1_seed",
        tool_obj=write_file_tool,
        raw_input={"file_path": base_path, "content": "value=0\n"},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )
    baseline = await _layer_metrics(sandbox_id)
    records: list[dict[str, Any]] = []
    samples: list[dict[str, Any]] = []
    edit_value = 0
    mutation_count = 0
    current_metrics = baseline

    for index in range(100):
        label = f"o1_{index:03d}"
        before = current_metrics
        if index % 3 == 0:
            mutation_count += 1
            result = await _call_tool(
                label=label,
                tool_obj=write_file_tool,
                raw_input={
                    "file_path": f"{root}/items/item-{index:03d}.txt",
                    "content": f"index={index}\n",
                },
                metadata=metadata,
                emit=emit,
                call_tool=call_tool,
                record_tool_check=None,
            )
            tool_name = "write_file"
        elif index % 3 == 1:
            mutation_count += 1
            result = await _call_tool(
                label=label,
                tool_obj=edit_file_tool,
                raw_input={
                    "file_path": base_path,
                    "old_text": f"value={edit_value}\n",
                    "new_text": f"value={edit_value + 1}\n",
                    "description": "o1 sequential edit",
                },
                metadata=metadata,
                emit=emit,
                call_tool=call_tool,
                record_tool_check=None,
            )
            edit_value += 1
            tool_name = "edit_file"
        else:
            result = await _call_tool(
                label=label,
                tool_obj=read_file_tool,
                raw_input={"file_path": base_path, "start_line": 1, "end_line": 5},
                metadata=metadata,
                emit=emit,
                call_tool=call_tool,
                record_tool_check=None,
            )
            tool_name = "read_file"
        current_metrics = await _layer_metrics(sandbox_id)
        records.append(
            {
                "index": index,
                "tool_name": tool_name,
                "manifest_before": int(before.get("manifest_version") or 0),
                "manifest_after": int(current_metrics.get("manifest_version") or 0),
                "timings": _timings(result),
                "changed_paths": _changed_paths(result),
            }
        )
        if (index + 1) % 10 == 0:
            samples.append(
                {
                    "after_call": index + 1,
                    "layer_metrics": current_metrics,
                    "runtime": await _runtime_sample(sandbox_id),
                }
            )

    manifest_delta = int(current_metrics.get("manifest_version") or 0) - int(
        baseline.get("manifest_version") or 0
    )
    auto_squash_count = sum(
        1
        for record in records
        if "layer_stack.auto_squash.total_s" in (record.get("timings") or {})
    )
    if manifest_delta < mutation_count:
        raise RuntimeError(
            "manifest version did not advance at least once per mutation: "
            f"delta={manifest_delta} mutations={mutation_count}"
        )
    leaked_samples = [
        sample
        for sample in samples
        if sample["runtime"]["command_overlay_run_dirs"] != 0
    ]
    if leaked_samples:
        raise RuntimeError(f"runtime overlay dirs leaked in O(1) probe: {leaked_samples}")

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "o1_disk",
        "operation_count": 100,
        "mutation_count": mutation_count,
        "manifest_delta": manifest_delta,
        "auto_squash_count": auto_squash_count,
        "baseline_layer_metrics": baseline,
        "final_layer_metrics": current_metrics,
        "samples": samples,
        "tool_counts": _tool_counts(records),
        "warm_p95_ms": _warm_p95_by_tool(records),
    }
    return await _write_summary(
        path=O1_DISK_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def run_ephemeral_same_path_conflict_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """Race same-path writes, then retry failures against fresh reads."""
    metadata.repo_root = WORKSPACE_ROOT
    path = f"{_case_subroot('same-path')}/shared.txt"
    await _call_tool(
        label="conflict_seed",
        tool_obj=write_file_tool,
        raw_input={"file_path": path, "content": "owner=seed\n"},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )

    async def first_wave(index: int) -> ToolResult:
        return await _call_tool(
            label=f"conflict_first_{index}",
            tool_obj=write_file_tool,
            raw_input={"file_path": path, "content": f"owner=first-{index}\n"},
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
            allow_error=True,
        )

    first = await asyncio.gather(*(first_wave(index) for index in range(4)))
    first_records = [_conflict_record(index, result) for index, result in enumerate(first)]
    failed_indexes = [item["index"] for item in first_records if item["is_error"]]
    if not any(not item["is_error"] for item in first_records):
        raise RuntimeError(f"same-path first wave had no success: {first_records}")
    if not failed_indexes:
        raise RuntimeError(f"same-path first wave produced no typed conflicts: {first_records}")

    successful_labels = [
        f"first-{item['index']}" for item in first_records if not item["is_error"]
    ]
    retries: list[dict[str, Any]] = []
    for index in failed_indexes:
        fresh = await _call_tool(
            label=f"conflict_retry_read_{index}",
            tool_obj=read_file_tool,
            raw_input={"file_path": path, "start_line": 1, "end_line": 5},
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
        )
        retry_value = f"retry-{index}"
        retry = await _call_tool(
            label=f"conflict_retry_write_{index}",
            tool_obj=write_file_tool,
            raw_input={"file_path": path, "content": f"owner={retry_value}\n"},
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=record_tool_check,
        )
        successful_labels.append(retry_value)
        retries.append(
            {
                "index": index,
                "fresh_content": _read_content(fresh),
                "retry_changed_paths": _changed_paths(retry),
                "retry_source": _mutation_source(retry),
            }
        )

    final = await _call_tool(
        label="conflict_final_read",
        tool_obj=read_file_tool,
        raw_input={"file_path": path, "start_line": 1, "end_line": 5},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=None,
    )
    final_content = _read_content(final)
    expected = successful_labels[-1]
    if expected not in final_content:
        raise RuntimeError(
            f"final shared content did not match last retry {expected!r}: {final_content}"
        )

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "same_path_conflict",
        "first_wave": first_records,
        "retry_records": retries,
        "final_content": final_content,
        "last_successful_value": expected,
    }
    return await _write_summary(
        path=SAME_PATH_CONFLICT_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def run_ephemeral_same_path_conflict_seed_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """Seed the shared target before task/request fans out writer generators."""
    metadata.repo_root = WORKSPACE_ROOT
    mkdir = await _call_tool(
        label="same_path_fanout_seed_dirs",
        tool_obj=exec_command_tool,
        raw_input={
            "command": f"mkdir -p {SAME_PATH_CONFLICT_FRAGMENT_DIR}",
            "timeout": 60,
        },
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )
    if mkdir.is_error:
        raise RuntimeError(f"same-path seed mkdir failed: {mkdir.output}")
    seed = await _call_tool(
        label="same_path_fanout_seed",
        tool_obj=write_file_tool,
        raw_input={
            "file_path": SAME_PATH_CONFLICT_SHARED_PATH,
            "content": "owner=seed\n",
        },
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )
    if seed.is_error:
        raise RuntimeError(f"same-path seed write failed: {seed.output}")
    return SAME_PATH_CONFLICT_SHARED_PATH


async def run_ephemeral_same_path_conflict_writer_probe(
    *,
    index: int,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """Race one first-wave same-path edit and persist its fragment."""
    metadata.repo_root = WORKSPACE_ROOT
    first = await _call_tool(
        label=f"same_path_fanout_first_{index}",
        tool_obj=edit_file_tool,
        raw_input={
            "file_path": SAME_PATH_CONFLICT_SHARED_PATH,
            "old_text": "owner=seed\n",
            "new_text": f"owner=first-{index}\n",
            "description": (
                "same-path conflict first-wave fan-out "
                f"writer={index}"
            ),
        },
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=None,
        allow_error=True,
    )
    fragment_path = f"{SAME_PATH_CONFLICT_FRAGMENT_DIR}/first-{index}.json"
    fragment_payload = _conflict_record(index, first)
    fragment = await _call_tool(
        label=f"same_path_fanout_fragment_{index}",
        tool_obj=write_file_tool,
        raw_input={
            "file_path": fragment_path,
            "content": json.dumps(fragment_payload, indent=2, sort_keys=True) + "\n",
        },
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )
    if fragment.is_error:
        raise RuntimeError(f"same-path fragment write failed: {fragment.output}")
    return fragment_path


async def run_ephemeral_same_path_conflict_reconcile_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """Retry failed first-wave writers and write the scenario summary contract."""
    from test_runner.scenarios.sandbox.ephemeral_workspace import (
        SAME_PATH_CONFLICT_WRITER_COUNT,
    )

    metadata.repo_root = WORKSPACE_ROOT
    first_records: list[dict[str, Any]] = []
    for index in range(SAME_PATH_CONFLICT_WRITER_COUNT):
        fragment = await _call_tool(
            label=f"same_path_fanout_read_fragment_{index}",
            tool_obj=read_file_tool,
            raw_input={
                "file_path": f"{SAME_PATH_CONFLICT_FRAGMENT_DIR}/first-{index}.json",
                "start_line": 1,
                "end_line": 80,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
        )
        first_records.append(json.loads(_read_content(fragment)))

    failed_indexes = [item["index"] for item in first_records if item["is_error"]]
    if not any(not item["is_error"] for item in first_records):
        raise RuntimeError(f"same-path first wave had no success: {first_records}")
    if not failed_indexes:
        raise RuntimeError(
            f"same-path first wave produced no typed conflicts: {first_records}"
        )

    successful_labels = [
        f"first-{item['index']}" for item in first_records if not item["is_error"]
    ]
    retries: list[dict[str, Any]] = []
    for index in failed_indexes:
        fresh = await _call_tool(
            label=f"same_path_fanout_retry_read_{index}",
            tool_obj=read_file_tool,
            raw_input={
                "file_path": SAME_PATH_CONFLICT_SHARED_PATH,
                "start_line": 1,
                "end_line": 5,
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=None,
        )
        retry_value = f"retry-{index}"
        retry = await _call_tool(
            label=f"same_path_fanout_retry_write_{index}",
            tool_obj=write_file_tool,
            raw_input={
                "file_path": SAME_PATH_CONFLICT_SHARED_PATH,
                "content": f"owner={retry_value}\n",
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=record_tool_check,
        )
        successful_labels.append(retry_value)
        retries.append(
            {
                "index": index,
                "fresh_content": _read_content(fresh),
                "retry_changed_paths": _changed_paths(retry),
                "retry_source": _mutation_source(retry),
            }
        )

    final = await _call_tool(
        label="same_path_fanout_final_read",
        tool_obj=read_file_tool,
        raw_input={
            "file_path": SAME_PATH_CONFLICT_SHARED_PATH,
            "start_line": 1,
            "end_line": 5,
        },
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=None,
    )
    final_content = _read_content(final)
    expected = successful_labels[-1]
    if expected not in final_content:
        raise RuntimeError(
            f"final shared content did not match last retry {expected!r}: "
            f"{final_content}"
        )

    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "same_path_conflict",
        "first_wave": first_records,
        "retry_records": retries,
        "final_content": final_content,
        "last_successful_value": expected,
    }
    return await _write_summary(
        path=SAME_PATH_CONFLICT_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def _call_tool(
    *,
    label: str,
    tool_obj: BaseTool,
    raw_input: dict[str, Any],
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck | None,
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
        record_tool_check(f"tool.{tool_obj.name}.ephemeral_workspace.{label}", result)
    return result


async def _write_summary(
    *,
    path: str,
    payload: dict[str, Any],
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    result = await _call_tool(
        label=f"summary.{payload['mode']}",
        tool_obj=write_file_tool,
        raw_input={
            "file_path": path,
            "content": json.dumps(payload, indent=2, sort_keys=True) + "\n",
        },
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )
    if result.is_error:
        raise RuntimeError(f"ephemeral summary write failed: {result.output}")
    return path


async def _layer_metrics(sandbox_id: str) -> dict[str, Any]:
    return await DaemonSandboxTransport().call(
        sandbox_id,
        "api.layer_metrics",
        {},
        timeout=60,
    )


async def _runtime_sample(sandbox_id: str) -> dict[str, Any]:
    command = (
        "python - <<'PY'\n"
        "import json, os\n"
        "from pathlib import Path\n"
        "root = Path('/eos/mount/runtime/overlay')\n"
        "def tree_bytes(path):\n"
        "    total = 0\n"
        "    entries = 0\n"
        "    if not path.exists():\n"
        "        return 0, 0\n"
        "    for current, dirs, files in os.walk(path):\n"
        "        entries += 1\n"
        "        try:\n"
        "            total += os.lstat(current).st_blocks * 512\n"
        "        except OSError:\n"
        "            pass\n"
        "        for name in files:\n"
        "            p = os.path.join(current, name)\n"
        "            entries += 1\n"
        "            try:\n"
        "                total += os.lstat(p).st_blocks * 512\n"
        "            except OSError:\n"
        "                pass\n"
        "    return total, entries\n"
        "run_dirs = sorted(p.name for p in root.iterdir() if p.is_dir()) if root.exists() else []\n"
        "command_run_dirs = [name for name in run_dirs if not name.startswith('lsp-session-')]\n"
        "bytes_used, entries = tree_bytes(root)\n"
        "print(json.dumps({\n"
        "    'overlay_root': str(root),\n"
        "    'overlay_run_dirs': len(run_dirs),\n"
        "    'overlay_run_dir_names': run_dirs[:20],\n"
        "    'command_overlay_run_dirs': len(command_run_dirs),\n"
        "    'command_overlay_run_dir_names': command_run_dirs[:20],\n"
        "    'overlay_tree_bytes': bytes_used,\n"
        "    'overlay_tree_entries': entries,\n"
        "}, sort_keys=True))\n"
        "PY"
    )
    result = await sandbox_api.raw_exec(sandbox_id, command, timeout=30)
    if result.exit_code != 0:
        raise RuntimeError(f"runtime sample failed: {result.stderr or result.stdout}")
    return json.loads(result.stdout or "{}")


async def _wait_for_background_drain(metadata: ExecutionMetadata) -> None:
    sandbox_id = str(metadata.sandbox_id or "").strip()
    agent_id = str(metadata.agent_run_id or metadata.agent_name or "").strip()
    if not sandbox_id or not agent_id:
        return
    deadline = time.perf_counter() + 15.0
    while time.perf_counter() < deadline:
        try:
            count = await sandbox_api.inflight_count(sandbox_id, agent_id)
        except Exception:
            return
        if count <= 0:
            return
        await asyncio.sleep(0.1)


def _record(
    *,
    label: str,
    tool_name: str,
    result: ToolResult,
    intent: str,
    before: Mapping[str, Any],
    after: Mapping[str, Any],
    runtime: Mapping[str, Any],
) -> dict[str, Any]:
    return {
        "label": label,
        "tool_name": tool_name,
        "intent": intent,
        "is_error": result.is_error,
        "status": _status(result),
        "changed_paths": _changed_paths(result),
        "changed_path_kinds": _changed_path_kinds(result),
        "mutation_source": _mutation_source(result),
        "error_kind": _error_kind(result),
        "conflict_reason": str((result.metadata or {}).get("conflict_reason") or ""),
        "manifest_before": int(before.get("manifest_version") or 0),
        "manifest_after": int(after.get("manifest_version") or 0),
        "runtime_after": dict(runtime),
        "timings": _timings(result),
    }


def _assert_per_call_cleanup(record: Mapping[str, Any]) -> None:
    runtime = record.get("runtime_after")
    if (
        isinstance(runtime, Mapping)
        and int(runtime.get("command_overlay_run_dirs") or 0) != 0
    ):
        raise RuntimeError(f"per-call overlay cleanup failed: {record}")


def _assert_required_kinds(result: ToolResult) -> None:
    kinds = set(_changed_path_kinds(result).values())
    required = {"write", "delete", "symlink", "opaque_dir"}
    missing = sorted(required - kinds)
    if missing:
        raise RuntimeError(f"shell changed_path kinds missing {missing}: {result.metadata}")


def _assert_contains(content: str, needle: str, label: str) -> None:
    if needle not in content:
        raise RuntimeError(f"{label} missing {needle!r}: {content[:500]}")


def _json(result: ToolResult) -> dict[str, Any]:
    try:
        parsed = json.loads(result.output or "{}")
    except json.JSONDecodeError:
        return {}
    return parsed if isinstance(parsed, dict) else {}


def _read_content(result: ToolResult) -> str:
    content = str(_json(result).get("content") or result.output or "")
    lines: list[str] = []
    for line in content.splitlines():
        if len(line) >= 6 and line[:4].strip().isdigit() and line[4:6] == ": ":
            lines.append(line[6:])
        else:
            lines.append(line)
    return "\n".join(lines)


def _stdout(result: ToolResult) -> str:
    payload = _json(result)
    output = payload.get("output")
    if isinstance(output, dict):
        return str(output.get("stdout") or "")
    return str(payload.get("stdout") or "")


def _status(result: ToolResult) -> str:
    return str((result.metadata or {}).get("status") or "")


def _error_kind(result: ToolResult) -> str:
    return str((result.metadata or {}).get("error_kind") or "")


def _mutation_source(result: ToolResult) -> str:
    return str((result.metadata or {}).get("mutation_source") or "")


def _changed_paths(result: ToolResult) -> list[str]:
    raw = (result.metadata or {}).get("changed_paths") or []
    if not isinstance(raw, (list, tuple, set)):
        return []
    return [str(item) for item in raw if str(item or "").strip()]


def _changed_path_kinds(result: ToolResult) -> dict[str, str]:
    raw = (result.metadata or {}).get("changed_path_kinds") or {}
    if not isinstance(raw, dict):
        return {}
    return {
        str(path): str(kind)
        for path, kind in raw.items()
        if str(path or "").strip() and str(kind or "").strip()
    }


def _timings(result: ToolResult) -> dict[str, float]:
    raw = (result.metadata or {}).get("timings") or {}
    if not isinstance(raw, dict):
        return {}
    timings: dict[str, float] = {}
    for key, value in raw.items():
        try:
            timings[str(key)] = float(value)
        except (TypeError, ValueError):
            continue
    return timings


def _conflict_record(index: int, result: ToolResult) -> dict[str, Any]:
    return {
        "index": index,
        "is_error": result.is_error,
        "status": _status(result),
        "conflict_reason": str((result.metadata or {}).get("conflict_reason") or ""),
        "error_kind": _error_kind(result),
        "changed_paths": _changed_paths(result),
        "mutation_source": _mutation_source(result),
    }


def _tool_counts(records: list[Mapping[str, Any]]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for record in records:
        name = str(record.get("tool_name") or "")
        counts[name] = counts.get(name, 0) + 1
    return counts


def _warm_p95_by_tool(records: list[Mapping[str, Any]]) -> dict[str, float]:
    by_tool: dict[str, list[float]] = {}
    for record in records[10:]:
        timings = record.get("timings") or {}
        if not isinstance(timings, Mapping):
            continue
        tool_name = str(record.get("tool_name") or "")
        total = timings.get(_total_timing_key(tool_name))
        if total is None:
            total = timings.get("command_exec.total_s")
        if total is None:
            continue
        by_tool.setdefault(tool_name, []).append(float(total) * 1000)
    return {
        tool: _p95(values)
        for tool, values in by_tool.items()
        if values
    }


def _total_timing_key(tool_name: str) -> str:
    return {
        "read_file": "api.read.total_s",
        "write_file": "api.write.total_s",
        "edit_file": "api.edit.total_s",
        "grep": "api.grep.total_s",
        "glob": "api.glob.total_s",
        "shell": "api.shell.total_s",
    }.get(tool_name, "command_exec.total_s")


def _p95(values: list[float]) -> float:
    if not values:
        return 0.0
    if len(values) == 1:
        return values[0]
    return float(statistics.quantiles(values, n=20, method="inclusive")[18])


__all__ = [
    "ALL_VERBS_SUMMARY",
    "CANCELLATION_SUMMARY",
    "CONCURRENT_WRITES_SUMMARY",
    "O1_DISK_SUMMARY",
    "POLICY_SUMMARY",
    "ROOT",
    "SUMMARY_SCHEMA",
    "run_ephemeral_all_verbs_probe",
    "run_ephemeral_cancellation_probe",
    "run_ephemeral_concurrent_writes_probe",
    "run_ephemeral_o1_disk_probe",
    "run_ephemeral_policy_probe",
    "run_ephemeral_same_path_conflict_probe",
]
