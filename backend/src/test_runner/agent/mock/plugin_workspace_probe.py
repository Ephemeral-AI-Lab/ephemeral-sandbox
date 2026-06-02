"""Live probes for the 3.5 plugin/LSP sandbox tier.

Each probe drives a focused plugin contract through the mock-agent scenario
harness and writes a JSON summary under
``/testbed/.ephemeralos/sweevo-mock/plugin/<mode>/summary.json``.
"""

from __future__ import annotations

import json
import statistics
from collections.abc import Awaitable, Callable, Mapping
from typing import Any
from uuid import uuid4

import sandbox.api as sandbox_api
from message.events import StreamEvent
from plugins.catalog.lsp.tools.apply_workspace_edit import (
    apply_workspace_edit as lsp_apply_workspace_edit_tool,
)
from plugins.catalog.lsp.tools.diagnostics import diagnostics as lsp_diagnostics_tool
from plugins.catalog.lsp.tools.find_definitions import (
    find_definitions as lsp_find_definitions_tool,
)
from plugins.catalog.lsp.tools.hover import hover as lsp_hover_tool
from sandbox.api.plugin_support import (
    daemon_plugin_manifest,
    run_plugin_intent_contract_checks,
    run_plugin_setup_failure_checks,
)
from sandbox.host.daemon_client import (
    _DaemonDispatchError,
    call_daemon_api,
)
from tools._framework.core.base import BaseTool
from tools._framework.core.results import ToolResult
from tools._framework.core.runtime import ExecutionMetadata
from tools.sandbox.edit_file import edit_file as edit_file_tool
from tools.sandbox.read_file import read_file as read_file_tool
from tools.sandbox.write_file import write_file as write_file_tool


WORKSPACE_ROOT = "/testbed"
ROOT = f"{WORKSPACE_ROOT}/.ephemeralos/sweevo-mock/plugin"
SUMMARY_SCHEMA = "test_runner.plugin_workspace.v1"

READ_ONLY_LSP_REFRESH_SUMMARY = f"{ROOT}/read_only_lsp_refresh/summary.json"
WRITE_ALLOWED_PUBLISH_SUMMARY = f"{ROOT}/write_allowed_publish/summary.json"
INTENT_CONTRACT_SUMMARY = f"{ROOT}/intent_contract/summary.json"
SETUP_FAILURE_SUMMARY = f"{ROOT}/setup_failure/summary.json"
SERVICE_EVICT_SUMMARY = f"{ROOT}/service_evict/summary.json"

EmitStreamEvent = Callable[[StreamEvent], Awaitable[None]]
CallTool = Callable[..., Awaitable[ToolResult]]
RecordToolCheck = Callable[[str, ToolResult], None]


async def run_plugin_read_only_lsp_refresh_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
    sandbox_id: str,
) -> str:
    """Exercise READ_ONLY LSP refresh after a normal default-mode edit."""
    metadata.repo_root = WORKSPACE_ROOT
    case_root = _case_root("read-only")
    module_path = f"{case_root}/module.py"
    records: list[dict[str, Any]] = []

    seed = await _call_recorded_tool(
        "read_only.seed",
        write_file_tool,
        {
            "file_path": module_path,
            "content": (
                "VALUE = 1\n\n"
                "def compute(value: int) -> int:\n"
                "    return value + VALUE\n"
            ),
        },
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        sandbox_id=sandbox_id,
    )
    records.append(seed)
    warmup = await _call_recorded_tool(
        "read_only.lsp_warmup",
        lsp_diagnostics_tool,
        {"file_path": module_path, "wait_for_diagnostics": False},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        sandbox_id=sandbox_id,
    )
    records.append(warmup)

    lsp_calls = [
        (
            "hover_before",
            lsp_hover_tool,
            {"file_path": module_path, "line": 2, "character": 4},
        ),
        (
            "definitions_before",
            lsp_find_definitions_tool,
            {"file_path": module_path, "line": 3, "character": 19},
        ),
        (
            "diagnostics_before",
            lsp_diagnostics_tool,
            {"file_path": module_path, "wait_for_diagnostics": False},
        ),
    ]
    for label, tool_obj, raw_input in lsp_calls:
        records.append(
            await _call_recorded_tool(
                f"read_only.{label}",
                tool_obj,
                raw_input,
                metadata=metadata,
                emit=emit,
                call_tool=call_tool,
                record_tool_check=record_tool_check,
                sandbox_id=sandbox_id,
            )
        )

    edit = await _call_recorded_tool(
        "read_only.default_edit",
        edit_file_tool,
        {
            "file_path": module_path,
            "old_text": "    return value + VALUE\n",
            "new_text": "    return value + missing_symbol\n",
            "description": "3.5 read-only LSP refresh edit",
        },
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        sandbox_id=sandbox_id,
    )
    records.append(edit)

    after = await _call_recorded_tool(
        "read_only.diagnostics_after",
        lsp_diagnostics_tool,
        {"file_path": module_path, "wait_for_diagnostics": True},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        sandbox_id=sandbox_id,
    )
    records.append(after)
    read_after = await _call_recorded_tool(
        "read_only.normal_read_after",
        read_file_tool,
        {"file_path": module_path, "start_line": 1, "end_line": 10},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        sandbox_id=sandbox_id,
    )
    records.append(read_after)

    lsp_records = [record for record in records if record["tool_name"].startswith("lsp.")]
    diagnostics_after = _json(after["output"])
    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "read_only_lsp_refresh",
        "records": records,
        "lsp_read_only_publish_count": sum(
            1
            for record in lsp_records
            if record["manifest_before"] != record["manifest_after"]
        ),
        "lsp_overlay_publish_timing_count": sum(
            1 for record in lsp_records if _has_overlay_publish_timing(record)
        ),
        "diagnostics_after_count": len(diagnostics_after.get("diagnostics") or []),
        "diagnostics_after_text": json.dumps(diagnostics_after, sort_keys=True)[:1000],
        "read_after_contains_missing_symbol": "missing_symbol" in read_after["output"],
        "start_delta_after_edit": _timing(after, "lsp.session.start_count_delta"),
        "refresh_total_after_edit": _timing(after, "lsp.session.refresh_count_total"),
        "remount_total_after_edit": _timing(after, "lsp.session.remount_count_total"),
        "cold_lsp_warmup_ms": _timing(warmup, "lsp.total_s") * 1000.0,
        "warm_lsp_p95_ms": _p95_ms(
            _lsp_total_seconds(
                [
                    record
                    for record in lsp_records
                    if record["label"]
                    not in {"read_only.lsp_warmup", "read_only.diagnostics_after"}
                ]
            )
        ),
        "diagnostics_after_wait_ms": _timing(after, "lsp.total_s") * 1000.0,
    }
    return await _write_summary(
        path=READ_ONLY_LSP_REFRESH_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def run_plugin_write_allowed_publish_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
    sandbox_id: str,
) -> str:
    """Apply a WRITE_ALLOWED LSP WorkspaceEdit and read it via normal API."""
    metadata.repo_root = WORKSPACE_ROOT
    case_root = _case_root("write")
    target_path = f"{case_root}/target.py"
    records: list[dict[str, Any]] = []
    seed = await _call_recorded_tool(
        "write_allowed.seed",
        write_file_tool,
        {"file_path": target_path, "content": "answer = 'old'\n"},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        sandbox_id=sandbox_id,
    )
    records.append(seed)
    runtime_before = await _runtime_sample(sandbox_id)

    edit = {
        "changes": {
            f"file://{target_path}": [
                {
                    "range": {
                        "start": {"line": 0, "character": 10},
                        "end": {"line": 0, "character": 13},
                    },
                    "newText": "new",
                }
            ]
        }
    }
    applied = await _call_recorded_tool(
        "write_allowed.apply_workspace_edit",
        lsp_apply_workspace_edit_tool,
        {"edit": edit},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        sandbox_id=sandbox_id,
    )
    records.append(applied)
    read = await _call_recorded_tool(
        "write_allowed.normal_read_after",
        read_file_tool,
        {"file_path": target_path, "start_line": 1, "end_line": 5},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        sandbox_id=sandbox_id,
    )
    records.append(read)
    runtime_after = await _runtime_sample(sandbox_id)
    apply_result = _json(applied["output"])
    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "write_allowed_publish",
        "records": records,
        "apply_result": apply_result,
        "apply_changed_paths": list(apply_result.get("changed_paths") or []),
        "apply_manifest_version": apply_result.get("manifest_version"),
        "apply_overlay_timing_keys": sorted(
            key for key in applied["timings"] if _looks_like_overlay_publish_key(key)
        ),
        "normal_read_content": read["output"],
        "normal_read_has_new_value": "answer = 'new'" in read["output"],
        "runtime_before": runtime_before,
        "runtime_after": runtime_after,
        "command_overlay_run_dir_delta": (
            int(runtime_after.get("command_overlay_run_dirs") or 0)
            - int(runtime_before.get("command_overlay_run_dirs") or 0)
        ),
    }
    return await _write_summary(
        path=WRITE_ALLOWED_PUBLISH_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def run_plugin_intent_contract_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
) -> str:
    """Verify plugin intent registration and dispatch path selection."""
    metadata.repo_root = WORKSPACE_ROOT
    summary = await run_plugin_intent_contract_checks()
    summary.update({"schema": SUMMARY_SCHEMA, "mode": "intent_contract"})
    return await _write_summary(
        path=INTENT_CONTRACT_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def run_plugin_setup_failure_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
    sandbox_id: str,
) -> str:
    """Classify setup/network failure and prove retry has no stale state."""
    metadata.repo_root = WORKSPACE_ROOT
    failure, retry = await run_plugin_setup_failure_checks(
        sandbox_id,
        workspace_root=WORKSPACE_ROOT,
    )
    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "setup_failure",
        "failure": failure,
        "retry": retry,
    }
    return await _write_summary(
        path=SETUP_FAILURE_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def run_plugin_service_evict_probe(
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
    sandbox_id: str,
) -> str:
    """Keep Pyright warm across peer publishes, evict, and restart cleanly."""
    metadata.repo_root = WORKSPACE_ROOT
    case_root = _case_root("service")
    module_path = f"{case_root}/service_mod.py"
    records: list[dict[str, Any]] = []
    records.append(
        await _call_recorded_tool(
            "service.seed",
            write_file_tool,
            {
                "file_path": module_path,
                "content": "def service_value() -> int:\n    return 1\n",
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=record_tool_check,
            sandbox_id=sandbox_id,
        )
    )
    first = await _call_recorded_tool(
        "service.diagnostics_initial",
        lsp_diagnostics_tool,
        {"file_path": module_path, "wait_for_diagnostics": False},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        sandbox_id=sandbox_id,
    )
    records.append(first)

    peer_writes: list[dict[str, Any]] = []
    for index in range(5):
        peer = await _call_recorded_tool(
            f"service.peer_write_{index}",
            write_file_tool,
            {
                "file_path": f"{case_root}/peer_{index}.py",
                "content": f"PEER_{index} = {index}\n",
            },
            metadata=metadata,
            emit=emit,
            call_tool=call_tool,
            record_tool_check=record_tool_check,
            sandbox_id=sandbox_id,
        )
        records.append(peer)
        peer_writes.append(peer)

    refreshed = await _call_recorded_tool(
        "service.diagnostics_after_peer_publishes",
        lsp_diagnostics_tool,
        {"file_path": module_path, "wait_for_diagnostics": False},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        sandbox_id=sandbox_id,
    )
    records.append(refreshed)
    refresh_status = await call_daemon_api(
        sandbox_id,
        "api.plugin.status",
        {"agent_id": _agent_id(metadata), "probe_services": True},
        timeout=60,
    )
    refresh_service = _lsp_service_status(refresh_status)
    post_refresh_warm = await _call_recorded_tool(
        "service.hover_after_peer_refresh",
        lsp_hover_tool,
        {"file_path": module_path, "line": 0, "character": 4},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        sandbox_id=sandbox_id,
    )
    records.append(post_refresh_warm)
    forced_digest = f"service-evict-{uuid4().hex[:8]}"
    daemon_manifest = daemon_plugin_manifest("lsp", digest=forced_digest)
    evict_ensure = await call_daemon_api(
        sandbox_id,
        "api.plugin.ensure",
        {
            "plugin": "lsp",
            "digest": forced_digest,
            "manifest": daemon_manifest,
            "start_services": True,
            "workspace_root": WORKSPACE_ROOT,
            "agent_id": _agent_id(metadata),
        },
        timeout=60,
    )
    restarted = await _call_recorded_tool(
        "service.diagnostics_after_evict",
        lsp_diagnostics_tool,
        {"file_path": module_path, "wait_for_diagnostics": False},
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
        sandbox_id=sandbox_id,
    )
    records.append(restarted)
    summary = {
        "schema": SUMMARY_SCHEMA,
        "mode": "service_evict",
        "records": records,
        "peer_publish_count": len(peer_writes),
        "cold_initial_lsp_ms": _timing(first, "lsp.total_s") * 1000.0,
        "refresh_start_delta": _timing(refreshed, "lsp.session.start_count_delta"),
        "refresh_total": _timing(refreshed, "lsp.session.refresh_count_total"),
        "refresh_remount_total": _timing(refreshed, "lsp.session.remount_count_total"),
        "refresh_session_has_overlay_handle": bool(
            _timing(refreshed, "lsp.session.has_overlay_handle")
        ),
        "refresh_service_status": refresh_service,
        "refresh_service_state": str(refresh_service.get("state") or ""),
        "refresh_service_refresh_count": float(
            refresh_service.get("refresh_count") or 0.0
        ),
        "refresh_service_health_ok": _service_health_ok(refresh_status, refresh_service),
        "refresh_status": refresh_status,
        "refresh_lsp_ms": _timing(refreshed, "lsp.total_s") * 1000.0,
        "post_refresh_warm_lsp_ms": _timing(post_refresh_warm, "lsp.total_s")
        * 1000.0,
        "evict_ensure": evict_ensure,
        "evict_forced_digest": forced_digest,
        "post_evict_call_start_delta": _timing(
            restarted, "lsp.session.start_count_delta"
        ),
        "post_evict_call_start_total": _timing(
            restarted, "lsp.session.start_count_total"
        ),
        "post_evict_call_lsp_ms": _timing(restarted, "lsp.total_s") * 1000.0,
        "warm_lsp_p95_ms": _p95_ms(_lsp_total_seconds([post_refresh_warm])),
    }
    return await _write_summary(
        path=SERVICE_EVICT_SUMMARY,
        payload=summary,
        metadata=metadata,
        emit=emit,
        call_tool=call_tool,
        record_tool_check=record_tool_check,
    )


async def _call_recorded_tool(
    label: str,
    tool_obj: BaseTool,
    raw_input: dict[str, Any],
    *,
    metadata: ExecutionMetadata,
    emit: EmitStreamEvent,
    call_tool: CallTool,
    record_tool_check: RecordToolCheck,
    sandbox_id: str,
    allow_error: bool = False,
) -> dict[str, Any]:
    before = await _layer_metrics(sandbox_id)
    result = await _call_probe_tool(
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
    return {
        **_tool_record(label, result, tool_name=tool_obj.name),
        "manifest_before": int(before.get("manifest_version") or 0),
        "manifest_after": int(after.get("manifest_version") or 0),
    }


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
) -> ToolResult:
    result = await call_tool(
        tool_obj,
        raw_input,
        metadata,
        emit,
        allow_error=allow_error,
    )
    if record_tool_check is not None:
        record_tool_check(f"tool.{tool_obj.name}.plugin.{label}", result)
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
    result = await _call_probe_tool(
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
        raise RuntimeError(f"plugin summary write failed: {result.output}")
    return path


async def _layer_metrics(sandbox_id: str) -> dict[str, Any]:
    return await call_daemon_api(
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
        "    for current, _dirs, files in os.walk(path):\n"
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
        "command_run_dirs = [name for name in run_dirs if not name.startswith('lsp-session')]\n"
        "bytes_used, entries = tree_bytes(root)\n"
        "print(json.dumps({\n"
        "    'overlay_root': str(root),\n"
        "    'overlay_run_dirs': run_dirs,\n"
        "    'command_overlay_run_dirs': len(command_run_dirs),\n"
        "    'tree_bytes': bytes_used,\n"
        "    'tree_entries': entries,\n"
        "}, sort_keys=True))\n"
        "PY"
    )
    result = await sandbox_api.raw_exec(sandbox_id, command, timeout=30)
    if result.exit_code != 0:
        return {"error": result.stderr or result.stdout, "command_overlay_run_dirs": -1}
    return json.loads(result.stdout or "{}")


async def _daemon_error_record(
    sandbox_id: str,
    op: str,
    args: dict[str, Any],
) -> dict[str, Any]:
    try:
        response = await call_daemon_api(sandbox_id, op, args, timeout=15)
    except _DaemonDispatchError as exc:
        return {
            "raised": True,
            "kind": exc.kind,
            "message": exc.message,
            "details": exc.details,
        }
    return {"raised": False, "response": response}


def _tool_record(
    label: str,
    result: ToolResult,
    *,
    tool_name: str = "",
) -> dict[str, Any]:
    plugin = str(result.metadata.get("plugin") or "")
    op = str(result.metadata.get("op") or "")
    return {
        "label": label,
        "tool_name": tool_name or (f"{plugin}.{op}" if plugin and op else op),
        "is_error": result.is_error,
        "output": result.output,
        "metadata": dict(result.metadata or {}),
        "timings": _timings(result),
        "changed_paths": list(result.metadata.get("changed_paths") or ()),
    }


def _json(raw: str) -> dict[str, Any]:
    try:
        parsed = json.loads(raw or "{}")
    except json.JSONDecodeError:
        return {}
    return parsed if isinstance(parsed, dict) else {}


def _timings(result: ToolResult) -> dict[str, float]:
    raw = result.metadata.get("timings") if isinstance(result.metadata, dict) else {}
    if not isinstance(raw, Mapping):
        return {}
    return {
        str(key): float(value)
        for key, value in raw.items()
        if isinstance(value, int | float)
    }


def _timing(record: Mapping[str, Any], key: str) -> float:
    timings = record.get("timings")
    if not isinstance(timings, Mapping):
        return 0.0
    value = timings.get(key)
    return float(value) if isinstance(value, int | float) else 0.0


def _lsp_total_seconds(records: list[dict[str, Any]]) -> list[float]:
    values = [_timing(record, "lsp.total_s") for record in records]
    return [value for value in values if value > 0.0]


def _p95_ms(values_s: list[float]) -> float:
    if not values_s:
        return 0.0
    if len(values_s) == 1:
        return values_s[0] * 1000.0
    return statistics.quantiles(values_s, n=20, method="inclusive")[18] * 1000.0


def _has_overlay_publish_timing(record: Mapping[str, Any]) -> bool:
    timings = record.get("timings")
    if not isinstance(timings, Mapping):
        return False
    return any(_looks_like_overlay_publish_key(str(key)) for key in timings)


def _looks_like_overlay_publish_key(key: str) -> bool:
    needles = (
        "publish",
        "capture_upperdir",
        "command_exec.occ_apply_s",
    )
    return any(needle in key for needle in needles)


def _lsp_service_status(status: Mapping[str, Any]) -> dict[str, Any]:
    loaded = status.get("loaded_plugins")
    if not isinstance(loaded, list):
        return {}
    for plugin in loaded:
        if not isinstance(plugin, Mapping) or plugin.get("name") != "lsp":
            continue
        services = plugin.get("services")
        if not isinstance(services, list):
            return {}
        for service in services:
            if not isinstance(service, Mapping):
                continue
            key = service.get("key")
            if isinstance(key, Mapping) and key.get("service_id") == "runtime":
                return dict(service)
    return {}


def _service_health_ok(
    status: Mapping[str, Any],
    service: Mapping[str, Any],
) -> bool:
    key = service.get("key")
    if not isinstance(key, Mapping):
        return False
    manifest_key = str(service.get("manifest_key") or "")
    health = status.get("service_health")
    if not isinstance(health, list):
        return False
    return any(
        isinstance(item, Mapping)
        and item.get("success") is True
        and item.get("plugin") == key.get("plugin_id")
        and item.get("service_id") == key.get("service_id")
        and str(item.get("manifest_key") or "") == manifest_key
        for item in health
    )


def _case_root(mode: str) -> str:
    return f"{WORKSPACE_ROOT}/plugin_case/{mode}-{uuid4().hex[:8]}"


def _agent_id(metadata: ExecutionMetadata) -> str:
    return str(metadata.agent_run_id or metadata.agent_name or "executor").strip()


__all__ = [
    "INTENT_CONTRACT_SUMMARY",
    "READ_ONLY_LSP_REFRESH_SUMMARY",
    "SERVICE_EVICT_SUMMARY",
    "SETUP_FAILURE_SUMMARY",
    "WRITE_ALLOWED_PUBLISH_SUMMARY",
    "run_plugin_intent_contract_probe",
    "run_plugin_read_only_lsp_refresh_probe",
    "run_plugin_service_evict_probe",
    "run_plugin_setup_failure_probe",
    "run_plugin_write_allowed_publish_probe",
]
