"""Live plugin refresh strategy coverage.

This reuses the existing sandbox fixture to obtain a running Docker container,
then delegates the detailed refresh/materialization/autosquash probes to
``backend/scripts/bench_plugin_refresh_strategies.py``. The benchmark writes all
experiment state under ``/eos/plugin/*``.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest

from .._harness.sandbox_fixture import SandboxHandle
from .._harness.streaming_artifact import resolve_run_id


pytestmark = pytest.mark.asyncio

ROOT = Path(__file__).resolve().parents[5]
REFRESH_BENCH = ROOT / "backend" / "scripts" / "bench_plugin_refresh_strategies.py"
RUST_PLUGIN_BENCH = ROOT / "backend" / "scripts" / "bench_rust_daemon_plugin.py"


async def test_plugin_workspace_snapshot_refresh_strategy(
    integrated_sandbox: SandboxHandle,
) -> None:
    provider = os.environ.get("EOS_SANDBOX_PROVIDER", "docker").strip() or "docker"
    if provider != "docker":
        pytest.skip("plugin refresh strategy benchmark currently targets Docker containers")

    run_id = resolve_run_id()
    result_dir = ROOT / ".omc" / "results"
    result_dir.mkdir(parents=True, exist_ok=True)
    refresh_report = result_dir / f"plugin-refresh-strategies-{run_id}.json"
    refresh_markdown_report = result_dir / f"plugin-refresh-strategies-{run_id}.md"
    rust_report = result_dir / f"rust-daemon-plugin-generic-{run_id}.json"
    rust_markdown_report = result_dir / f"rust-daemon-plugin-generic-{run_id}.md"
    samples = os.environ.get("EOS_PLUGIN_REFRESH_SAMPLES", "1")
    auto_squash_writes = os.environ.get("EOS_PLUGIN_REFRESH_AUTO_SQUASH_WRITES", "104")

    refresh_cmd = [
        sys.executable,
        str(REFRESH_BENCH),
        "--container-id",
        integrated_sandbox.sandbox_id,
        "--samples",
        samples,
        "--auto-squash-writes",
        auto_squash_writes,
        "--report",
        str(refresh_report),
        "--markdown-report",
        str(refresh_markdown_report),
    ]
    _run_bench(
        refresh_cmd,
        timeout_s=int(os.environ.get("EOS_PLUGIN_REFRESH_TIMEOUT_S", "420")),
        label="plugin refresh benchmark",
    )

    refresh_payload = json.loads(refresh_report.read_text(encoding="utf-8"))
    assert refresh_payload["recommendation"]["winner"] == "workspace_snapshot_refresh"
    assert refresh_payload["workspace_snapshot_refresh"]["all_samples_ok"] is True
    assert refresh_payload["fs_watch_without_materialization"]["raw_workspace_stale"] is True
    assert refresh_payload["auto_squash_then_commit"]["gate_pass"] is True
    assert refresh_payload["final_metrics"]["orphan_layer_count"] == 0
    assert refresh_payload["final_metrics"]["missing_layer_count"] == 0

    rust_cmd = [
        sys.executable,
        str(RUST_PLUGIN_BENCH),
        "--container-id",
        integrated_sandbox.sandbox_id,
        "--report",
        str(rust_report),
        "--markdown-report",
        str(rust_markdown_report),
    ]
    _run_bench(
        rust_cmd,
        timeout_s=int(os.environ.get("EOS_RUST_PLUGIN_BENCH_TIMEOUT_S", "300")),
        label="Rust daemon generic plugin benchmark",
    )

    rust_payload = json.loads(rust_report.read_text(encoding="utf-8"))
    assert rust_payload["gate_pass"] is True
    assert rust_payload["ensure"]["service_processes_started"] is True
    assert "plugin.generic.ping" in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    assert (
        "plugin.generic.restart_ping"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.adapter_query"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.runtime_bridge_ping"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.runtime_bridge_apply"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.runtime_bridge_delay_ping"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.lsp_bridge_query_symbols"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.lsp_bridge_rename"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_symbols"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_workspace_symbols"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_capabilities"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_document_formatting"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_execute_command"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_completion"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_completion_resolve"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_diagnostics"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_code_actions"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_signature_help"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_hover"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_type_definition"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_declaration"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_call_hierarchy"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_document_highlight"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_prepare_rename"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_definition"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_references"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.pyright_rename"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.lsp_apply_workspace_edit"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.lsp_apply_code_action"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.lsp_format_document"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.lsp_execute_command"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.crash_probe"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.crash_recover_ping"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.hang_probe"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.hang_recover_ping"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.recover_probe"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.health_fail_ping"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.health_fail_recover_ping"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    service_health = {
        item["service_id"]: item
        for item in rust_payload["status_after_health_probe"]["service_health"]
    }
    for service_id in {
        "harness",
        "restart_harness",
        "adapter_harness",
        "runtime_bridge",
        "pyright_harness",
        "crash_harness",
        "hang_harness",
        "recover_harness",
    }:
        assert service_health[service_id]["success"] is True
    assert service_health["health_fail_harness"]["success"] is False
    assert (
        "intentional health failure"
        in service_health["health_fail_harness"]["error"]
    )
    assert (
        "plugin.generic.health_fail_ping"
        not in rust_payload["status_after_health_probe"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.health_fail_recover_ping"
        not in rust_payload["status_after_health_probe"]["connected_ppc_routes"]
    )
    health_fail_status = _service_status(
        rust_payload["status_after_health_probe"], "health_fail_harness"
    )
    assert health_fail_status["state"] == "stopped"
    assert (
        rust_payload["health_fail_recover_ping"]["from_health_recovered_service"]
        is True
    )
    assert rust_payload["health_fail_recover_ping"]["from_ppc"] is True
    assert rust_payload["health_fail_recover_ping"]["workspace_mounted"] is True
    assert rust_payload["health_fail_recover_ping"]["echo"] == (
        "after-health-fail-recover"
    )
    assert (
        "plugin.generic.health_fail_recover_ping"
        in rust_payload["status_after_health_fail_recover"]["connected_ppc_routes"]
    )
    health_fail_recover_status = _service_status(
        rust_payload["status_after_health_fail_recover"], "health_fail_harness"
    )
    assert health_fail_recover_status["state"] == "ready"
    assert health_fail_recover_status["restart_count"] >= 1
    assert "plugin.generic.apply" in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    assert (
        "plugin.generic.apply_multi"
        in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.oneshot_write"
        not in rust_payload["status_after_ensure"]["connected_ppc_routes"]
    )
    assert rust_payload["ping"]["from_ppc"] is True
    assert rust_payload["ping"]["workspace_mounted"] is True
    concurrent_ping = rust_payload["concurrent_ping"]
    assert len(concurrent_ping) == 2
    assert {item["echo"] for item in concurrent_ping} == {
        "concurrent-a",
        "concurrent-b",
    }
    assert {item["manifest_key"] for item in concurrent_ping} == {
        rust_payload["ping"]["manifest_key"],
    }
    assert all(item["success"] is True for item in concurrent_ping)
    assert all(item["from_ppc"] is True for item in concurrent_ping)
    assert all(item["workspace_mounted"] is True for item in concurrent_ping)
    assert all(item["service_id"] == "harness" for item in concurrent_ping)
    assert rust_payload["apply"]["from_self_managed"] is True
    assert rust_payload["apply"]["callback"]["success"] is True
    assert rust_payload["readback"]["content"] == "from live rust plugin\n"
    assert rust_payload["runtime_bridge_ping"]["from_runtime_bridge"] is True
    assert rust_payload["runtime_bridge_ping"]["from_ppc_service_bridge"] is True
    assert rust_payload["runtime_bridge_ping"]["workspace_mounted"] is True
    assert (
        rust_payload["runtime_bridge_ping"]["workspace_read"]["content"]
        == "from live rust plugin\n"
    )
    runtime_bridge_status = _service_status(
        rust_payload["status_after_runtime_bridge_ping"], "runtime_bridge"
    )
    assert runtime_bridge_status["state"] == "ready"
    assert runtime_bridge_status["refresh_count"] >= 1
    assert rust_payload["runtime_bridge_apply"]["from_runtime_bridge"] is True
    assert rust_payload["runtime_bridge_apply"]["from_ppc_service_bridge"] is True
    assert (
        rust_payload["runtime_bridge_apply"]["from_mounted_workspace_callback"]
        is True
    )
    assert rust_payload["runtime_bridge_apply"]["workspace_mounted"] is True
    assert rust_payload["runtime_bridge_apply"]["callback"]["success"] is True
    assert "live_plugin_runtime_bridge.txt" in rust_payload["runtime_bridge_apply"][
        "changed_paths"
    ]
    assert (
        rust_payload["runtime_bridge_readback"]["content"]
        == "from reusable ppc bridge\n"
    )
    runtime_bridge_concurrent = {
        item["echo"]: item for item in rust_payload["runtime_bridge_concurrent"]
    }
    runtime_bridge_slow = runtime_bridge_concurrent["slow-first"]
    runtime_bridge_fast = runtime_bridge_concurrent["fast-second"]
    assert runtime_bridge_slow["from_runtime_bridge"] is True
    assert runtime_bridge_fast["from_runtime_bridge"] is True
    assert runtime_bridge_slow["from_ppc_service_bridge"] is True
    assert runtime_bridge_fast["from_ppc_service_bridge"] is True
    assert runtime_bridge_slow["workspace_mounted"] is True
    assert runtime_bridge_fast["workspace_mounted"] is True
    assert runtime_bridge_slow["delay_s"] >= 0.3
    assert runtime_bridge_fast["delay_s"] == 0.0
    assert (
        runtime_bridge_fast["service_finished_at_s"]
        < runtime_bridge_slow["service_finished_at_s"]
    )
    assert runtime_bridge_fast["client_elapsed_s"] < runtime_bridge_slow["client_elapsed_s"]
    runtime_bridge_concurrent_apply = rust_payload["runtime_bridge_concurrent_apply"]
    assert len(runtime_bridge_concurrent_apply) == 2
    assert {
        path
        for item in runtime_bridge_concurrent_apply
        for path in item["changed_paths"]
    } == {
        "live_plugin_runtime_bridge_concurrent_a.txt",
        "live_plugin_runtime_bridge_concurrent_b.txt",
    }
    assert all(item["from_runtime_bridge"] is True for item in runtime_bridge_concurrent_apply)
    assert all(
        item["from_ppc_service_bridge"] is True
        for item in runtime_bridge_concurrent_apply
    )
    assert all(
        item["from_mounted_workspace_callback"] is True
        for item in runtime_bridge_concurrent_apply
    )
    assert all(item["workspace_mounted"] is True for item in runtime_bridge_concurrent_apply)
    assert all(item["callback"]["success"] is True for item in runtime_bridge_concurrent_apply)
    assert (
        rust_payload["runtime_bridge_concurrent_readback_a"]["content"]
        == "from concurrent runtime bridge a\n"
    )
    assert (
        rust_payload["runtime_bridge_concurrent_readback_b"]["content"]
        == "from concurrent runtime bridge b\n"
    )
    assert rust_payload["lsp_bridge_seed"]["success"] is True
    assert rust_payload["lsp_bridge_query_symbols"]["from_lsp_importlib_bridge"] is True
    assert rust_payload["lsp_bridge_query_symbols"]["from_ppc_service_bridge"] is True
    assert rust_payload["lsp_bridge_query_symbols"]["workspace_mounted"] is True
    assert (
        rust_payload["lsp_bridge_query_symbols"]["lsp"]["protocol"]
        == "lsp-python-importlib"
    )
    assert (
        rust_payload["lsp_bridge_query_symbols"]["lsp"]["server"]
        == "plugins.catalog.lsp.runtime.server"
    )
    assert (
        "bridge_total"
        in rust_payload["lsp_bridge_query_symbols"]["lsp"]["symbol_names"]
    )
    assert rust_payload["lsp_bridge_rename"]["from_lsp_importlib_bridge"] is True
    assert rust_payload["lsp_bridge_rename"]["from_ppc_service_bridge"] is True
    assert (
        rust_payload["lsp_bridge_rename"]["from_mounted_workspace_callback"]
        is True
    )
    assert rust_payload["lsp_bridge_rename"]["workspace_mounted"] is True
    assert (
        rust_payload["lsp_bridge_rename"]["lsp"]["protocol"]
        == "lsp-python-importlib"
    )
    assert rust_payload["lsp_bridge_rename"]["lsp"]["new_name"] == "bridge_total"
    assert rust_payload["lsp_bridge_rename"]["lsp"]["apply"]["success"] is True
    assert (
        "live_plugin_lsp_bridge.py"
        in rust_payload["lsp_bridge_rename"]["changed_paths"]
    )
    assert (
        rust_payload["lsp_bridge_rename_readback"]["content"]
        == "def bridge_total() -> int:\n    return 7\n\nRESULT = bridge_total()\n"
    )
    assert rust_payload["apply_multi"]["from_self_managed"] is True
    assert rust_payload["apply_multi"]["callback_count"] == 2
    assert len(rust_payload["apply_multi"]["callbacks"]) == 2
    assert all(
        callback["success"] is True
        for callback in rust_payload["apply_multi"]["callbacks"]
    )
    assert {
        "live_plugin_multi_a.txt",
        "live_plugin_multi_b.txt",
    }.issubset(set(rust_payload["apply_multi"]["changed_paths"]))
    assert (
        rust_payload["multi_readback_a"]["content"]
        == "from live rust plugin multi a\n"
    )
    assert (
        rust_payload["multi_readback_b"]["content"]
        == "from live rust plugin multi b\n"
    )
    assert rust_payload["shell_publish"]["exit_code"] == 0
    assert rust_payload["shell_publish"]["status"] in {"ok", "committed"}
    assert rust_payload["shell_readback"]["content"] == "from live rust shell publish\n"
    assert rust_payload["shell_refresh_ping"]["from_ppc"] is True
    assert rust_payload["shell_refresh_ping"]["workspace_mounted"] is True
    assert (
        rust_payload["shell_refresh_ping"]["workspace_read"]["content"]
        == "from live rust shell publish\n"
    )
    shell_refresh_status = _service_status(
        rust_payload["status_after_shell_refresh"],
        "harness",
    )
    assert shell_refresh_status["state"] == "ready"
    assert shell_refresh_status["refresh_count"] >= 1
    assert rust_payload["refresh_ping"]["from_ppc"] is True
    assert rust_payload["refresh_ping"]["workspace_mounted"] is True
    assert (
        rust_payload["refresh_ping"]["workspace_read"]["content"]
        == "from live rust plugin\n"
    )
    harness_status = _service_status(rust_payload["status_after_refresh"], "harness")
    assert harness_status["state"] == "ready"
    assert harness_status["refresh_count"] >= 1
    assert rust_payload["adapter_query"]["from_package_adapter"] is True
    assert rust_payload["adapter_query"]["workspace_mounted"] is True
    assert rust_payload["adapter_query"]["package"]["protocol"] == "line-json-v1"
    assert rust_payload["adapter_query"]["package"]["cached"] is True
    assert (
        rust_payload["adapter_query"]["package"]["content"]
        == "from live rust plugin\n"
    )
    adapter_status = _service_status(
        rust_payload["status_after_adapter"], "adapter_harness"
    )
    assert adapter_status["state"] == "ready"
    assert adapter_status["refresh_count"] >= 1
    co_shared_refresh = rust_payload["co_shared_refresh"]
    assert co_shared_refresh["first_service_id"] == "harness"
    assert co_shared_refresh["second_service_id"] == "adapter_harness"
    assert co_shared_refresh["first_state"] == "ready"
    assert co_shared_refresh["second_state"] == "ready"
    assert co_shared_refresh["same_manifest_key"] is True
    assert co_shared_refresh["first_refresh_count"] >= 1
    assert co_shared_refresh["second_refresh_count"] >= 1
    assert co_shared_refresh["first_restart_count"] == 0
    assert co_shared_refresh["second_restart_count"] == 0
    assert rust_payload["pyright_seed"]["success"] is True
    assert rust_payload["pyright_symbols"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_symbols"]["workspace_mounted"] is True
    assert rust_payload["pyright_symbols"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert "live_value" in rust_payload["pyright_symbols"]["lsp"]["symbol_names"]
    assert rust_payload["pyright_workspace_symbols"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_workspace_symbols"]["workspace_mounted"] is True
    assert rust_payload["pyright_workspace_symbols"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert rust_payload["pyright_workspace_symbols"]["lsp"]["symbol_count"] >= 1
    assert (
        "live_value"
        in rust_payload["pyright_workspace_symbols"]["lsp"]["symbol_names"]
    )
    assert (
        "live_plugin_pyright.py"
        in rust_payload["pyright_workspace_symbols"]["lsp"]["symbol_paths"]
    )
    assert rust_payload["pyright_capabilities"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_capabilities"]["workspace_mounted"] is True
    assert rust_payload["pyright_capabilities"]["lsp"]["protocol"] == "lsp-jsonrpc"
    capability_supports = rust_payload["pyright_capabilities"]["lsp"]["supports"]
    assert capability_supports["completion"] is True
    assert capability_supports["completion_resolve"] is True
    assert capability_supports["hover"] is True
    assert capability_supports["signature_help"] is True
    assert capability_supports["definition"] is True
    assert capability_supports["declaration"] is True
    assert capability_supports["type_definition"] is True
    assert capability_supports["document_highlight"] is True
    assert capability_supports["document_symbol"] is True
    assert capability_supports["workspace_symbol"] is True
    assert capability_supports["references"] is True
    assert capability_supports["rename"] is True
    assert capability_supports["code_action"] is True
    assert capability_supports["document_formatting"] is False
    assert capability_supports["document_range_formatting"] is False
    assert capability_supports["execute_command_provider"] is True
    assert capability_supports["execute_command"] is False
    assert (
        rust_payload["pyright_capabilities"]["lsp"]["raw"]["executeCommandProvider"][
            "commands"
        ]
        == []
    )
    code_action_provider = rust_payload["pyright_capabilities"]["lsp"]["raw"][
        "codeActionProvider"
    ]
    assert "source.organizeImports" in code_action_provider["codeActionKinds"]
    assert capability_supports["call_hierarchy"] is True
    assert rust_payload["pyright_document_formatting"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_document_formatting"]["workspace_mounted"] is True
    assert rust_payload["pyright_document_formatting"]["success"] is False
    formatting_lsp = rust_payload["pyright_document_formatting"]["lsp"]
    assert formatting_lsp["protocol"] == "lsp-jsonrpc"
    assert formatting_lsp["path"] == "live_plugin_pyright.py"
    assert formatting_lsp["method"] == "textDocument/formatting"
    assert formatting_lsp["capability"] == "documentFormattingProvider"
    assert formatting_lsp["supported"] is False
    assert formatting_lsp["unsupported"] is True
    assert formatting_lsp["edit_count"] == 0
    assert rust_payload["pyright_execute_command"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_execute_command"]["workspace_mounted"] is True
    assert rust_payload["pyright_execute_command"]["success"] is False
    execute_lsp = rust_payload["pyright_execute_command"]["lsp"]
    assert execute_lsp["protocol"] == "lsp-jsonrpc"
    assert execute_lsp["method"] == "workspace/executeCommand"
    assert execute_lsp["capability"] == "executeCommandProvider.commands"
    assert execute_lsp["supported"] is False
    assert execute_lsp["unsupported"] is True
    assert execute_lsp["commands"] == []
    assert rust_payload["pyright_hover"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_hover"]["workspace_mounted"] is True
    assert rust_payload["pyright_hover"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert "live_value" in rust_payload["pyright_hover"]["lsp"]["hover_text"]
    assert "int" in rust_payload["pyright_hover"]["lsp"]["hover_text"]
    assert rust_payload["pyright_type_seed"]["success"] is True
    assert rust_payload["pyright_type_definition"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_type_definition"]["workspace_mounted"] is True
    assert rust_payload["pyright_type_definition"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert rust_payload["pyright_type_definition"]["lsp"]["path"] == "live_plugin_type.py"
    assert rust_payload["pyright_type_definition"]["lsp"]["position"]["line"] == 4
    assert rust_payload["pyright_type_definition"]["lsp"]["position"]["character"] == 11
    assert rust_payload["pyright_type_definition"]["lsp"]["type_definition_count"] >= 1
    assert any(
        location["path"] == "live_plugin_type.py"
        and location["range"]["start"]["line"] == 0
        for location in rust_payload["pyright_type_definition"]["lsp"]["locations"]
    )
    assert rust_payload["pyright_declaration"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_declaration"]["workspace_mounted"] is True
    assert rust_payload["pyright_declaration"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert rust_payload["pyright_declaration"]["lsp"]["path"] == "live_plugin_pyright.py"
    assert rust_payload["pyright_declaration"]["lsp"]["position"]["line"] == 3
    assert rust_payload["pyright_declaration"]["lsp"]["position"]["character"] == 12
    assert rust_payload["pyright_declaration"]["lsp"]["declaration_count"] >= 1
    assert any(
        location["path"] == "live_plugin_pyright.py"
        and location["range"]["start"]["line"] == 0
        for location in rust_payload["pyright_declaration"]["lsp"]["locations"]
    )
    assert rust_payload["pyright_call_hierarchy_seed"]["success"] is True
    assert rust_payload["pyright_call_hierarchy"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_call_hierarchy"]["workspace_mounted"] is True
    assert rust_payload["pyright_call_hierarchy"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert (
        rust_payload["pyright_call_hierarchy"]["lsp"]["path"]
        == "live_plugin_call_hierarchy.py"
    )
    assert rust_payload["pyright_call_hierarchy"]["lsp"]["position"]["line"] == 0
    assert (
        rust_payload["pyright_call_hierarchy"]["lsp"]["position"]["character"]
        == len("def live_ca")
    )
    assert rust_payload["pyright_call_hierarchy"]["lsp"]["item_count"] >= 1
    assert (
        "live_callee"
        in rust_payload["pyright_call_hierarchy"]["lsp"]["item_names"]
    )
    assert rust_payload["pyright_call_hierarchy"]["lsp"]["incoming_count"] >= 1
    assert (
        "live_caller"
        in rust_payload["pyright_call_hierarchy"]["lsp"]["incoming_names"]
    )
    assert rust_payload["pyright_call_hierarchy_outgoing"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_call_hierarchy_outgoing"]["workspace_mounted"] is True
    assert (
        rust_payload["pyright_call_hierarchy_outgoing"]["lsp"]["protocol"]
        == "lsp-jsonrpc"
    )
    assert (
        rust_payload["pyright_call_hierarchy_outgoing"]["lsp"]["path"]
        == "live_plugin_call_hierarchy.py"
    )
    assert rust_payload["pyright_call_hierarchy_outgoing"]["lsp"]["position"]["line"] == 3
    assert (
        rust_payload["pyright_call_hierarchy_outgoing"]["lsp"]["position"]["character"]
        == len("def live_ca")
    )
    assert rust_payload["pyright_call_hierarchy_outgoing"]["lsp"]["item_count"] >= 1
    assert (
        "live_caller"
        in rust_payload["pyright_call_hierarchy_outgoing"]["lsp"]["item_names"]
    )
    assert rust_payload["pyright_call_hierarchy_outgoing"]["lsp"]["outgoing_count"] >= 1
    assert (
        "live_callee"
        in rust_payload["pyright_call_hierarchy_outgoing"]["lsp"]["outgoing_names"]
    )
    assert rust_payload["pyright_document_highlight"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_document_highlight"]["workspace_mounted"] is True
    assert (
        rust_payload["pyright_document_highlight"]["lsp"]["protocol"]
        == "lsp-jsonrpc"
    )
    assert rust_payload["pyright_document_highlight"]["lsp"]["highlight_count"] >= 2
    highlight_lines = {
        highlight["range"]["start"]["line"]
        for highlight in rust_payload["pyright_document_highlight"]["lsp"]["highlights"]
        if highlight["path"] == "live_plugin_pyright.py"
    }
    assert {0, 3}.issubset(highlight_lines)
    assert rust_payload["pyright_prepare_rename"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_prepare_rename"]["workspace_mounted"] is True
    assert rust_payload["pyright_prepare_rename"]["lsp"]["protocol"] == "lsp-jsonrpc"
    prepare_range = rust_payload["pyright_prepare_rename"]["lsp"]["range"]
    assert prepare_range["start"]["line"] == 3
    assert prepare_range["start"]["character"] == 9
    assert prepare_range["end"]["character"] == 19
    assert rust_payload["pyright_definition"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_definition"]["workspace_mounted"] is True
    assert rust_payload["pyright_definition"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert rust_payload["pyright_definition"]["lsp"]["definition_count"] >= 1
    assert any(
        location["path"] == "live_plugin_pyright.py"
        and location["range"]["start"]["line"] == 0
        for location in rust_payload["pyright_definition"]["lsp"]["locations"]
    )
    assert rust_payload["pyright_references"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_references"]["workspace_mounted"] is True
    assert rust_payload["pyright_references"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert rust_payload["pyright_references"]["lsp"]["reference_count"] >= 2
    reference_lines = {
        location["range"]["start"]["line"]
        for location in rust_payload["pyright_references"]["lsp"]["locations"]
        if location["path"] == "live_plugin_pyright.py"
    }
    assert {0, 3}.issubset(reference_lines)
    assert rust_payload["lsp_apply_workspace_edit_seed"]["success"] is True
    assert rust_payload["lsp_apply_workspace_edit"]["from_lsp_workspace_edit"] is True
    assert rust_payload["lsp_apply_workspace_edit"]["from_self_managed"] is True
    assert rust_payload["lsp_apply_workspace_edit"]["workspace_mounted"] is True
    assert rust_payload["lsp_apply_workspace_edit"]["callback"]["success"] is True
    assert (
        "live_plugin_apply_workspace_edit.py"
        in rust_payload["lsp_apply_workspace_edit"]["changed_paths"]
    )
    assert (
        rust_payload["lsp_apply_workspace_edit_readback"]["content"]
        == "alpha\nedited\n"
    )
    assert rust_payload["lsp_apply_code_action_seed"]["success"] is True
    assert rust_payload["lsp_apply_code_action"]["from_lsp_code_action"] is True
    assert rust_payload["lsp_apply_code_action"]["from_self_managed"] is True
    assert rust_payload["lsp_apply_code_action"]["workspace_mounted"] is True
    assert rust_payload["lsp_apply_code_action"]["action_title"] == "Replace first line"
    assert rust_payload["lsp_apply_code_action"]["action_kind"] == "quickfix"
    assert rust_payload["lsp_apply_code_action"]["callback"]["success"] is True
    assert (
        "live_plugin_apply_code_action.py"
        in rust_payload["lsp_apply_code_action"]["changed_paths"]
    )
    assert (
        rust_payload["lsp_apply_code_action_readback"]["content"]
        == "after\nunchanged\n"
    )
    assert rust_payload["lsp_format_seed"]["success"] is True
    assert rust_payload["lsp_format_document"]["from_lsp_formatting"] is True
    assert rust_payload["lsp_format_document"]["from_self_managed"] is True
    assert rust_payload["lsp_format_document"]["workspace_mounted"] is True
    assert rust_payload["lsp_format_document"]["method"] == "textDocument/formatting"
    assert rust_payload["lsp_format_document"]["edit_count"] >= 1
    assert rust_payload["lsp_format_document"]["callback"]["success"] is True
    assert (
        "live_plugin_format.py"
        in rust_payload["lsp_format_document"]["changed_paths"]
    )
    assert (
        rust_payload["lsp_format_readback"]["content"]
        == "def format_me() -> int:\n    return 1\n"
    )
    assert rust_payload["lsp_execute_command_seed"]["success"] is True
    assert rust_payload["lsp_execute_command"]["from_lsp_execute_command"] is True
    assert rust_payload["lsp_execute_command"]["from_self_managed"] is True
    assert rust_payload["lsp_execute_command"]["workspace_mounted"] is True
    assert rust_payload["lsp_execute_command"]["method"] == "workspace/executeCommand"
    assert rust_payload["lsp_execute_command"]["command"] == "generic.applyWorkspaceEdit"
    assert rust_payload["lsp_execute_command"]["supported"] is True
    assert rust_payload["lsp_execute_command"]["unsupported"] is False
    assert rust_payload["lsp_execute_command"]["callback"]["success"] is True
    assert (
        "live_plugin_execute_command.py"
        in rust_payload["lsp_execute_command"]["changed_paths"]
    )
    assert (
        rust_payload["lsp_execute_command_readback"]["content"]
        == "value = 'after'\n"
    )
    pyright_status = _service_status(
        rust_payload["status_after_pyright"], "pyright_harness"
    )
    assert pyright_status["state"] == "ready"
    assert pyright_status["refresh_count"] >= 1
    assert rust_payload["pyright_completion_seed"]["success"] is True
    assert rust_payload["pyright_completion"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_completion"]["workspace_mounted"] is True
    assert rust_payload["pyright_completion"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert rust_payload["pyright_completion"]["lsp"]["path"] == "live_plugin_completion.py"
    assert rust_payload["pyright_completion"]["lsp"]["position"]["line"] == 3
    assert rust_payload["pyright_completion"]["lsp"]["position"]["character"] == 14
    assert "live_value" in rust_payload["pyright_completion"]["lsp"]["matching_labels"]
    assert rust_payload["pyright_completion_resolve"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_completion_resolve"]["workspace_mounted"] is True
    assert rust_payload["pyright_completion_resolve"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert (
        rust_payload["pyright_completion_resolve"]["lsp"]["path"]
        == "live_plugin_completion.py"
    )
    assert rust_payload["pyright_completion_resolve"]["lsp"]["position"]["line"] == 3
    assert rust_payload["pyright_completion_resolve"]["lsp"]["position"]["character"] == 14
    assert rust_payload["pyright_completion_resolve"]["lsp"]["request_label"] == "live_value"
    assert rust_payload["pyright_completion_resolve"]["lsp"]["resolved_label"] == "live_value"
    assert rust_payload["pyright_diagnostics_seed"]["success"] is True
    assert rust_payload["pyright_diagnostics"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_diagnostics"]["workspace_mounted"] is True
    assert rust_payload["pyright_diagnostics"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert rust_payload["pyright_diagnostics"]["lsp"]["path"] == "live_plugin_diagnostics.py"
    assert rust_payload["pyright_diagnostics"]["lsp"]["position"]["line"] == 0
    assert rust_payload["pyright_diagnostics"]["lsp"]["position"]["character"] == len("value: Li")
    assert rust_payload["pyright_diagnostics"]["lsp"]["diagnostic_count"] >= 1
    diagnostic_messages = rust_payload["pyright_diagnostics"]["lsp"]["diagnostic_messages"]
    assert any("List" in message for message in diagnostic_messages)
    assert any(
        code == "reportUndefinedVariable"
        for code in rust_payload["pyright_diagnostics"]["lsp"]["diagnostic_codes"]
    )
    assert rust_payload["pyright_code_action_seed"]["success"] is True
    assert rust_payload["pyright_code_actions"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_code_actions"]["workspace_mounted"] is True
    assert rust_payload["pyright_code_actions"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert (
        rust_payload["pyright_code_actions"]["lsp"]["path"]
        == "live_plugin_code_actions.py"
    )
    assert rust_payload["pyright_code_actions"]["lsp"]["position"]["line"] == 0
    assert rust_payload["pyright_code_actions"]["lsp"]["position"]["character"] == 0
    assert (
        "source.organizeImports"
        in rust_payload["pyright_code_actions"]["lsp"]["only"]
    )
    assert isinstance(rust_payload["pyright_code_actions"]["lsp"]["actions"], list)
    assert rust_payload["pyright_code_actions"]["lsp"]["action_count"] >= 0
    assert (
        rust_payload["pyright_code_actions"]["lsp"]["action_count"] == 0
        or "source.organizeImports"
        in rust_payload["pyright_code_actions"]["lsp"]["action_kinds"]
    )
    assert rust_payload["pyright_signature_seed"]["success"] is True
    assert rust_payload["pyright_signature_help"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_signature_help"]["workspace_mounted"] is True
    assert rust_payload["pyright_signature_help"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert rust_payload["pyright_signature_help"]["lsp"]["path"] == "live_plugin_signature.py"
    assert rust_payload["pyright_signature_help"]["lsp"]["position"]["line"] == 3
    assert rust_payload["pyright_signature_help"]["lsp"]["position"]["character"] == 28
    assert rust_payload["pyright_signature_help"]["lsp"]["signature_count"] >= 1
    assert rust_payload["pyright_signature_help"]["lsp"]["active_parameter"] == 1
    signature_labels = rust_payload["pyright_signature_help"]["lsp"]["labels"]
    assert any(
        "left" in label and "right" in label
        for label in signature_labels
    )
    assert rust_payload["pyright_rename"]["from_pyright_adapter"] is True
    assert rust_payload["pyright_rename"]["from_self_managed"] is True
    assert rust_payload["pyright_rename"]["workspace_mounted"] is True
    assert rust_payload["pyright_rename"]["callback"]["success"] is True
    assert (
        "live_plugin_pyright.py"
        in rust_payload["pyright_rename"]["changed_paths"]
    )
    assert rust_payload["pyright_rename"]["lsp"]["protocol"] == "lsp-jsonrpc"
    assert rust_payload["pyright_rename"]["lsp"]["new_name"] == "live_total"
    assert (
        rust_payload["pyright_rename_readback"]["content"]
        == "def live_total() -> int:\n    return 42\n\nRESULT = live_total()\n"
    )
    assert rust_payload["restart_ping"]["from_ppc"] is True
    assert rust_payload["restart_ping"]["from_restart_service"] is True
    assert rust_payload["restart_ping"]["workspace_mounted"] is True
    assert (
        rust_payload["restart_ping"]["workspace_read"]["content"]
        == "from live rust plugin\n"
    )
    restart_status = _service_status(
        rust_payload["status_after_restart"], "restart_harness"
    )
    assert restart_status["state"] == "ready"
    assert restart_status["restart_count"] >= 1
    assert restart_status["refresh_count"] == 0
    assert rust_payload["oneshot"]["success"] is True
    assert rust_payload["oneshot"]["plugin_overlay"]["worker_exit_code"] == 0
    assert rust_payload["oneshot"]["plugin_result"]["worker"] == "oneshot_overlay"
    assert (
        rust_payload["oneshot_readback"]["content"]
        == "from live rust oneshot plugin\n"
    )
    assert rust_payload["crash_probe"]["expected_failure"] is True
    assert (
        "plugin.generic.crash_probe"
        not in rust_payload["status_after_crash"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.crash_recover_ping"
        not in rust_payload["status_after_crash"]["connected_ppc_routes"]
    )
    crash_status = _service_status(rust_payload["status_after_crash"], "crash_harness")
    assert crash_status["state"] == "stopped"
    assert rust_payload["crash_recover_ping"]["from_crash_recovered_service"] is True
    assert rust_payload["crash_recover_ping"]["from_ppc"] is True
    assert rust_payload["crash_recover_ping"]["workspace_mounted"] is True
    assert rust_payload["crash_recover_ping"]["echo"] == "after-crash-recover"
    assert (
        "plugin.generic.crash_recover_ping"
        in rust_payload["status_after_crash_recover"]["connected_ppc_routes"]
    )
    crash_recover_status = _service_status(
        rust_payload["status_after_crash_recover"], "crash_harness"
    )
    assert crash_recover_status["state"] == "ready"
    assert crash_recover_status["restart_count"] >= 1
    assert rust_payload["hang_probe"]["expected_failure"] is True
    assert (
        "plugin.generic.hang_probe"
        not in rust_payload["status_after_hang"]["connected_ppc_routes"]
    )
    assert (
        "plugin.generic.hang_recover_ping"
        not in rust_payload["status_after_hang"]["connected_ppc_routes"]
    )
    hang_status = _service_status(rust_payload["status_after_hang"], "hang_harness")
    assert hang_status["state"] == "stopped"
    assert rust_payload["hang_recover_ping"]["from_timeout_recovered_service"] is True
    assert rust_payload["hang_recover_ping"]["from_ppc"] is True
    assert rust_payload["hang_recover_ping"]["workspace_mounted"] is True
    assert rust_payload["hang_recover_ping"]["echo"] == "after-timeout-recover"
    assert (
        "plugin.generic.hang_recover_ping"
        in rust_payload["status_after_hang_recover"]["connected_ppc_routes"]
    )
    hang_recover_status = _service_status(
        rust_payload["status_after_hang_recover"],
        "hang_harness",
    )
    assert hang_recover_status["state"] == "ready"
    assert hang_recover_status["restart_count"] >= 1
    assert rust_payload["recover_probe_first"]["expected_failure"] is True
    assert (
        "plugin.generic.recover_probe"
        not in rust_payload["status_after_recover_failure"]["connected_ppc_routes"]
    )
    recover_failed_status = _service_status(
        rust_payload["status_after_recover_failure"], "recover_harness"
    )
    assert recover_failed_status["state"] == "stopped"
    assert rust_payload["recover_probe_second"]["from_recovered_service"] is True
    assert rust_payload["recover_probe_second"]["workspace_mounted"] is True
    assert (
        "plugin.generic.recover_probe"
        in rust_payload["status_after_recover"]["connected_ppc_routes"]
    )
    recover_status = _service_status(rust_payload["status_after_recover"], "recover_harness")
    assert recover_status["state"] == "ready"
    assert recover_status["restart_count"] >= 1
    isolated_gate = rust_payload["isolated_plugin_gate"]
    assert isolated_gate["gate_pass"] is True
    assert isolated_gate["enter"]["success"] is True
    assert _is_forbidden_in_isolated_workspace(isolated_gate["plugin_status"])
    assert _is_forbidden_in_isolated_workspace(isolated_gate["plugin_dispatch"])
    assert isolated_gate["exit"]["success"] is True
    assert isolated_gate["status_after_exit"]["open"] is False
    assert rust_payload["final_metrics"]["active_leases"] >= 1
    assert rust_payload["final_metrics"]["orphan_layer_count"] == 0
    assert rust_payload["final_metrics"]["missing_layer_count"] == 0
    assert rust_payload["post_cleanup_metrics"]["active_leases"] == 0
    assert rust_payload["post_cleanup_metrics"]["orphan_layer_count"] == 0
    assert rust_payload["post_cleanup_metrics"]["missing_layer_count"] == 0
    assert rust_payload["processes_before_cleanup"]["count"] >= 1
    assert rust_payload["processes_after_cleanup"]["count"] == 0
    assert rust_payload["status_after_cleanup"]["connected_ppc_routes"] == []
    assert rust_payload["status_after_cleanup"]["connected_ppc_services"] == []
    assert rust_payload["status_after_cleanup"]["running_service_processes"] == []


def _service_status(status_payload: dict[str, Any], service_id: str) -> dict[str, Any]:
    for plugin in status_payload.get("loaded_plugins", []):
        if not isinstance(plugin, dict):
            continue
        for service in plugin.get("services", []):
            if (
                isinstance(service, dict)
                and isinstance(service.get("key"), dict)
                and service["key"].get("service_id") == service_id
            ):
                return service
    raise AssertionError(f"missing service status for {service_id}")


def _is_forbidden_in_isolated_workspace(response: dict[str, Any]) -> bool:
    error = response.get("error")
    if isinstance(error, dict) and error.get("kind") == "forbidden_in_isolated_workspace":
        return True
    return "forbidden_in_isolated_workspace" in json.dumps(response, sort_keys=True)


def _run_bench(cmd: list[str], *, timeout_s: int, label: str) -> None:
    completed = subprocess.run(
        cmd,
        cwd=ROOT,
        text=True,
        capture_output=True,
        timeout=timeout_s,
        check=False,
    )
    assert completed.returncode == 0, (
        f"{label} failed\n"
        f"cmd={' '.join(cmd)}\n"
        f"stdout={completed.stdout[-4000:]}\n"
        f"stderr={completed.stderr[-4000:]}"
    )
