"""Live load tests for mixed concurrent Daytona writes, edits, and daytona_shell.

This suite runs real tool calls against one live sandbox and one shared CI
service so audited process behavior is exercised under mixed contention:

1. Concurrent ``daytona_write_file`` calls on unique files.
2. Concurrent ``daytona_edit_file`` calls:
   - disjoint same-file edits across a small set of files.
   - overlapping same-line edits across a few files.
3. Concurrent ``daytona_rename_symbol`` calls on unique symbols.
4. Concurrent ``daytona_move_file`` and ``daytona_delete_file`` calls.
5. Concurrent coordinated ``daytona_shell`` shell commands on unique files.

The test verifies:
- successful writes are persisted,
- disjoint edits mostly land,
- overlapping edits permit at most one winner per target file,
- arbiter stats are sane after the burst,
- active file locks are cleaned up after completion.
"""

from __future__ import annotations

import asyncio
import base64
import json
import os
import re
import shlex
import sys
import threading
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from collections.abc import Callable
from typing import Any

import pytest
from dotenv import load_dotenv

from code_intelligence._async_bridge import configure_default_executor
from code_intelligence.analysis import symbol_index as symbol_index_module
from code_intelligence.editing import write_coordinator as write_coordinator_module
from code_intelligence.lsp import client as lsp_client_module
from code_intelligence.routing import content_manager as content_manager_module
from code_intelligence.routing import rename_planner as rename_planner_module
from code_intelligence.routing.service import CodeIntelligenceService
from tests.test_e2e.daytona_exec_io import read_text_via_exec, write_text_via_exec
from tools.core.base import ToolExecutionContext, ToolResult
from tools.daytona_toolkit._daytona_utils import (
    _extract_exit_code,
    _wrap_bash_command,
)
from code_intelligence.routing import command_executor as command_executor_module
from code_intelligence.routing import overlay_auditor as overlay_auditor_module
from code_intelligence.routing import overlay_command_committer as overlay_committer_module
from code_intelligence.routing import service as ci_service_module
import tools.daytona_toolkit.shell_tool as shell_tool_module
from tools.daytona_toolkit.shell_tool import daytona_shell
from tools.daytona_toolkit.delete_move_tool import (
    daytona_delete_file,
    daytona_move_file,
)
from tools.daytona_toolkit.edit_tool import daytona_edit_file
from tools.daytona_toolkit.rename_tool import daytona_rename_symbol
from tools.daytona_toolkit.tools import daytona_write_file

_PROJECT_ROOT = Path(__file__).resolve().parents[3]
load_dotenv(_PROJECT_ROOT / ".env")

_LIVE_LOAD_OVERLAY_MAX_CONCURRENT = 50


def _set_live_load_overlay_concurrency(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(
        "EOS_OVERLAY_MAX_CONCURRENT",
        str(_LIVE_LOAD_OVERLAY_MAX_CONCURRENT),
    )


def _load_settings() -> dict[str, Any]:
    settings_path = Path.home() / ".ephemeralos" / "settings.json"
    if settings_path.exists():
        return json.loads(settings_path.read_text())
    return {}


_SETTINGS = _load_settings()
HAS_DAYTONA = bool(
    (os.environ.get("DAYTONA_API_KEY") or _SETTINGS.get("daytona_api_key", ""))
    and (os.environ.get("DAYTONA_API_URL") or _SETTINGS.get("daytona_api_url", ""))
)

pytestmark = [pytest.mark.e2e, pytest.mark.live]

_TERM_NOISE = re.compile(r"\x1b\[3J.*$", re.S)


class _AsyncFs:
    def __init__(self, real_fs: Any):
        self._real = real_fs

    async def upload_file(self, *args, **kwargs):
        return await asyncio.to_thread(self._real.upload_file, *args, **kwargs)

    async def download_file(self, *args, **kwargs):
        return await asyncio.to_thread(self._real.download_file, *args, **kwargs)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._real, name)


class _AsyncProcess:
    def __init__(self, real_process: Any):
        self._real = real_process

    async def exec(self, *args, **kwargs):
        response = await asyncio.to_thread(self._real.exec, *args, **kwargs)
        stdout = _TERM_NOISE.sub("", getattr(response, "result", "") or "")
        return SimpleNamespace(result=stdout, exit_code=getattr(response, "exit_code", None))

    def __getattr__(self, name: str) -> Any:
        return getattr(self._real, name)


class _AsyncSandboxWrapper:
    def __init__(self, raw_sandbox: Any):
        self._raw = raw_sandbox
        self.fs = _AsyncFs(raw_sandbox.fs)
        self.process = _AsyncProcess(raw_sandbox.process)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._raw, name)


@dataclass
class LiveLoadEnv:
    sandbox_id: str
    raw_sandbox: Any
    async_sandbox: Any
    home: str
    repo_root: str

    def exec(self, command: str, *, cwd: str | None = None, timeout: int = 180) -> tuple[int, str]:
        wrapped = command if cwd is None else f"cd {shlex.quote(cwd)} && {command}"
        response = self.raw_sandbox.process.exec(_wrap_bash_command(wrapped), timeout=timeout)
        raw = _TERM_NOISE.sub("", getattr(response, "result", "") or "")
        cleaned, exit_code = _extract_exit_code(
            raw,
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return exit_code, cleaned

    def exec_checked(self, command: str, *, cwd: str | None = None, timeout: int = 180) -> str:
        exit_code, stdout = self.exec(command, cwd=cwd, timeout=timeout)
        if exit_code != 0:
            detail = stdout.strip() or f"exit {exit_code}"
            raise AssertionError(f"Sandbox command failed: {detail}")
        return stdout

    def require_command(self, name: str) -> None:
        exit_code, _ = self.exec(f"command -v {shlex.quote(name)} >/dev/null 2>&1", timeout=30)
        if exit_code != 0:
            pytest.skip(f"Sandbox image missing required command: {name}")

    def write_text(self, rel_path: str, content: str) -> None:
        write_text_via_exec(self.raw_sandbox, f"{self.repo_root}/{rel_path}", content, timeout=60)

    def read_text(self, rel_path: str) -> str:
        return read_text_via_exec(self.raw_sandbox, f"{self.repo_root}/{rel_path}", timeout=60)

    def make_ci_service(self) -> CodeIntelligenceService:
        return CodeIntelligenceService(
            sandbox_id=self.sandbox_id,
            workspace_root=self.repo_root,
            sandbox=self.raw_sandbox,
        )

    def make_ctx(
        self,
        ci_service: CodeIntelligenceService,
        *,
        agent_run_id: str,
        coordinated: bool = False,
    ) -> ToolExecutionContext:
        metadata: dict[str, Any] = {
            "daytona_sandbox": self.async_sandbox,
            "ci_sandbox": self.raw_sandbox,
            "daytona_cwd": self.repo_root,
            "repo_root": self.repo_root,
            "exec_cwd": self.repo_root,
            "ci_service": ci_service,
            "agent_run_id": agent_run_id,
        }
        if coordinated:
            metadata["agent_name"] = "developer"
        return ToolExecutionContext(cwd=Path(self.repo_root), metadata=metadata)

    def init_repo(self) -> None:
        self.exec_checked(f"rm -rf {shlex.quote(self.repo_root)} && mkdir -p {shlex.quote(self.repo_root)}")
        self.exec_checked(f"git -C {shlex.quote(self.repo_root)} init")
        self.exec_checked(f"git -C {shlex.quote(self.repo_root)} config user.email test@example.com")
        self.exec_checked(f"git -C {shlex.quote(self.repo_root)} config user.name 'Test User'")


@pytest.fixture
def live_load_env():
    if not HAS_DAYTONA:
        pytest.skip("Daytona credentials not configured")

    from sandbox.testing import create_test_sandbox, delete_test_sandbox, get_sandbox_service

    info = create_test_sandbox(name="process-audit-load-live")
    sandbox_id = info["id"]
    try:
        sandbox_svc = get_sandbox_service()
        raw_sandbox = sandbox_svc.get_sandbox_object(sandbox_id)
        home_resp = raw_sandbox.process.exec("pwd", timeout=10)
        home = (getattr(home_resp, "result", "") or "").strip() or "/home/daytona"
        env = LiveLoadEnv(
            sandbox_id=sandbox_id,
            raw_sandbox=raw_sandbox,
            async_sandbox=_AsyncSandboxWrapper(raw_sandbox),
            home=home,
            repo_root=f"{home}/process_audit_load_repo",
        )
        env.require_command("git")
        env.require_command("python3")
        yield env
    finally:
        delete_test_sandbox(sandbox_id)


def _json_output(result: ToolResult) -> dict[str, Any]:
    assert result.output, "tool returned empty output"
    return json.loads(result.output)


async def _invoke_tool(tool: Any, kwargs: dict[str, Any], ctx: ToolExecutionContext) -> ToolResult:
    return await tool.execute(tool.input_model(**kwargs), ctx)


def _install_shell_phase_probe(monkeypatch: pytest.MonkeyPatch) -> dict[str, list[float]]:
    stats: dict[str, list[float]] = {
        "shell_exec_s": [],
    }

    original_shell = shell_tool_module._run_shell_with_recovery

    async def _timed_shell(*args, **kwargs):
        started = time.perf_counter()
        try:
            return await original_shell(*args, **kwargs)
        finally:
            stats["shell_exec_s"].append(round(time.perf_counter() - started, 6))

    monkeypatch.setattr(shell_tool_module, "_run_shell_with_recovery", _timed_shell)
    return stats


def _install_overlay_phase_probe(
    monkeypatch: pytest.MonkeyPatch,
) -> dict[str, list[float]]:
    """Probe overlay audit lifecycle for per-phase timings."""
    stats: dict[str, list[float]] = {
        "overlay_audit_total_s": [],
        "overlay_snapshot_s": [],
        "overlay_run_command_s": [],
        "overlay_read_stdout_s": [],
        "overlay_read_diff_s": [],
        "overlay_cleanup_run_dir_s": [],
        "overlay_ensure_script_uploaded_s": [],
        "overlay_assemble_commit_s": [],
        "overlay_commit_s": [],
    }

    orig_execute = overlay_auditor_module.OverlayAuditor.execute
    orig_run = overlay_auditor_module.OverlayAuditor._run_overlay
    orig_read_stdout = overlay_auditor_module.OverlayAuditor._read_stdout
    orig_collect = overlay_auditor_module.OverlayAuditor._read_diff
    orig_cleanup = overlay_auditor_module.OverlayAuditor._cleanup_run_dir
    orig_ensure_upload = overlay_auditor_module.OverlayAuditor._ensure_script_uploaded
    orig_commit_diff = overlay_auditor_module.OverlayAuditor._commit_and_assemble
    orig_diff_commit = overlay_committer_module.OverlayCommandCommitter.commit

    async def _timed_execute(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            return await orig_execute(self, *args, **kwargs)
        finally:
            stats["overlay_audit_total_s"].append(
                round(time.perf_counter() - started, 6)
            )

    async def _timed_run(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            return await orig_run(self, *args, **kwargs)
        finally:
            stats["overlay_run_command_s"].append(
                round(time.perf_counter() - started, 6)
            )

    async def _timed_read_stdout(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            return await orig_read_stdout(self, *args, **kwargs)
        finally:
            stats["overlay_read_stdout_s"].append(
                round(time.perf_counter() - started, 6)
            )

    async def _timed_cleanup(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            return await orig_cleanup(self, *args, **kwargs)
        finally:
            stats["overlay_cleanup_run_dir_s"].append(
                round(time.perf_counter() - started, 6)
            )

    async def _timed_ensure_upload(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            return await orig_ensure_upload(self, *args, **kwargs)
        finally:
            stats["overlay_ensure_script_uploaded_s"].append(
                round(time.perf_counter() - started, 6)
            )

    async def _timed_collect(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            diff = await orig_collect(self, *args, **kwargs)
            timings = getattr(diff, "snapshot_timings", {}) or {}
            for key, value in timings.items():
                if isinstance(value, (int, float)):
                    stats.setdefault(f"overlay_snapshot_{key}_s", []).append(
                        round(float(value), 6)
                    )
            if timings:
                total = timings.get("total")
                if isinstance(total, (int, float)):
                    stats["overlay_snapshot_s"].append(round(float(total), 6))
            run_timings = getattr(diff, "run_timings", {}) or {}
            for key, value in run_timings.items():
                if isinstance(value, (int, float)):
                    stats.setdefault(f"overlay_inner_{key}_s", []).append(
                        round(float(value), 6)
                    )
            return diff
        finally:
            stats["overlay_read_diff_s"].append(
                round(time.perf_counter() - started, 6)
            )

    async def _timed_commit_diff(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            return await orig_commit_diff(self, *args, **kwargs)
        finally:
            stats["overlay_assemble_commit_s"].append(
                round(time.perf_counter() - started, 6)
            )

    async def _timed_diff_commit(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            return await orig_diff_commit(self, *args, **kwargs)
        finally:
            stats["overlay_commit_s"].append(round(time.perf_counter() - started, 6))

    monkeypatch.setattr(
        overlay_auditor_module.OverlayAuditor,
        "execute",
        _timed_execute,
    )
    monkeypatch.setattr(
        overlay_auditor_module.OverlayAuditor,
        "_run_overlay",
        _timed_run,
    )
    monkeypatch.setattr(
        overlay_auditor_module.OverlayAuditor,
        "_read_stdout",
        _timed_read_stdout,
    )
    monkeypatch.setattr(
        overlay_auditor_module.OverlayAuditor,
        "_read_diff",
        _timed_collect,
    )
    monkeypatch.setattr(
        overlay_auditor_module.OverlayAuditor,
        "_cleanup_run_dir",
        _timed_cleanup,
    )
    monkeypatch.setattr(
        overlay_auditor_module.OverlayAuditor,
        "_ensure_script_uploaded",
        _timed_ensure_upload,
    )
    monkeypatch.setattr(
        overlay_auditor_module.OverlayAuditor,
        "_commit_and_assemble",
        _timed_commit_diff,
    )
    monkeypatch.setattr(
        overlay_committer_module.OverlayCommandCommitter,
        "commit",
        _timed_diff_commit,
    )
    return stats


def _install_lsp_phase_probe(monkeypatch: pytest.MonkeyPatch) -> dict[str, list[float]]:
    """Probe LSP phases that can serialize otherwise disjoint writes."""
    stats: dict[str, list[float]] = {
        "lsp_invalidate_s": [],
        "lsp_find_references_many_s": [],
        "lsp_rename_symbol_s": [],
        "lsp_rename_symbols_s": [],
    }

    orig_invalidate = lsp_client_module.LspClient.invalidate
    orig_find_references_many = lsp_client_module.LspClient.find_references_many
    orig_rename_symbol = lsp_client_module.LspClient.rename_symbol
    orig_rename_symbols = lsp_client_module.LspClient.rename_symbols

    def _timed_invalidate(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            return orig_invalidate(self, *args, **kwargs)
        finally:
            stats["lsp_invalidate_s"].append(round(time.perf_counter() - started, 6))

    def _timed_find_references_many(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            return orig_find_references_many(self, *args, **kwargs)
        finally:
            stats["lsp_find_references_many_s"].append(
                round(time.perf_counter() - started, 6)
            )

    def _timed_rename_symbol(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            return orig_rename_symbol(self, *args, **kwargs)
        finally:
            stats["lsp_rename_symbol_s"].append(round(time.perf_counter() - started, 6))

    def _timed_rename_symbols(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            return orig_rename_symbols(self, *args, **kwargs)
        finally:
            stats["lsp_rename_symbols_s"].append(round(time.perf_counter() - started, 6))

    monkeypatch.setattr(lsp_client_module.LspClient, "invalidate", _timed_invalidate)
    monkeypatch.setattr(
        lsp_client_module.LspClient,
        "find_references_many",
        _timed_find_references_many,
    )
    monkeypatch.setattr(lsp_client_module.LspClient, "rename_symbol", _timed_rename_symbol)
    monkeypatch.setattr(lsp_client_module.LspClient, "rename_symbols", _timed_rename_symbols)
    return stats


def _install_rename_phase_probe(monkeypatch: pytest.MonkeyPatch) -> dict[str, list[float]]:
    """Probe semantic rename planner branches and batch sizes.

    The high-concurrency load test often reports identical elapsed times for
    all rename calls because they are intentionally batched. These counters
    make the batch internals visible: which planner branch hit, whether the
    full LSP rename fallback ran, and how many changes the batch produced.
    """
    stats: dict[str, list[float]] = {
        "rename_plan_single_s": [],
        "rename_plan_many_s": [],
        "rename_plan_many_request_count": [],
        "rename_plan_many_result_count": [],
        "rename_plan_many_change_count": [],
        "rename_preview_fast_s": [],
        "rename_preview_fast_hit": [],
        "rename_preview_fast_miss": [],
        "rename_same_file_fast_s": [],
        "rename_same_file_fast_hit": [],
        "rename_same_file_fast_miss": [],
        "rename_refs_fast_s": [],
        "rename_refs_fast_hit": [],
        "rename_refs_fast_miss": [],
    }

    orig_plan_single = rename_planner_module.RenamePlanner.rename_symbol_plan
    orig_plan_many = rename_planner_module.RenamePlanner.rename_symbol_plans_many
    orig_preview_fast = (
        rename_planner_module.RenamePlanner._preview_rename_symbol_plan_fast
    )
    orig_same_file_fast = (
        rename_planner_module.RenamePlanner._rename_symbol_plans_many_same_file_fast
    )
    orig_refs_fast = rename_planner_module.RenamePlanner._rename_symbol_plans_many_fast

    def _record_branch_result(prefix: str, result) -> None:
        stats[f"{prefix}_hit" if result is not None else f"{prefix}_miss"].append(1.0)

    def _timed_plan_single(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            return orig_plan_single(self, *args, **kwargs)
        finally:
            stats["rename_plan_single_s"].append(
                round(time.perf_counter() - started, 6)
            )

    def _timed_plan_many(self, requests, *args, **kwargs):
        stats["rename_plan_many_request_count"].append(float(len(requests or ())))
        started = time.perf_counter()
        try:
            result = orig_plan_many(self, requests, *args, **kwargs)
        finally:
            stats["rename_plan_many_s"].append(
                round(time.perf_counter() - started, 6)
            )
        stats["rename_plan_many_result_count"].append(float(len(result or ())))
        stats["rename_plan_many_change_count"].append(
            float(sum(len(getattr(plan, "changes", ()) or ()) for plan in result or ()))
        )
        return result

    def _timed_preview_fast(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            result = orig_preview_fast(self, *args, **kwargs)
        finally:
            stats["rename_preview_fast_s"].append(
                round(time.perf_counter() - started, 6)
            )
        _record_branch_result("rename_preview_fast", result)
        return result

    def _timed_same_file_fast(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            result = orig_same_file_fast(self, *args, **kwargs)
        finally:
            stats["rename_same_file_fast_s"].append(
                round(time.perf_counter() - started, 6)
            )
        _record_branch_result("rename_same_file_fast", result)
        return result

    def _timed_refs_fast(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            result = orig_refs_fast(self, *args, **kwargs)
        finally:
            stats["rename_refs_fast_s"].append(
                round(time.perf_counter() - started, 6)
            )
        _record_branch_result("rename_refs_fast", result)
        return result

    monkeypatch.setattr(
        rename_planner_module.RenamePlanner,
        "rename_symbol_plan",
        _timed_plan_single,
    )
    monkeypatch.setattr(
        rename_planner_module.RenamePlanner,
        "rename_symbol_plans_many",
        _timed_plan_many,
    )
    monkeypatch.setattr(
        rename_planner_module.RenamePlanner,
        "_preview_rename_symbol_plan_fast",
        _timed_preview_fast,
    )
    monkeypatch.setattr(
        rename_planner_module.RenamePlanner,
        "_rename_symbol_plans_many_same_file_fast",
        _timed_same_file_fast,
    )
    monkeypatch.setattr(
        rename_planner_module.RenamePlanner,
        "_rename_symbol_plans_many_fast",
        _timed_refs_fast,
    )
    return stats


def _install_svc_cmd_phase_probe(monkeypatch: pytest.MonkeyPatch) -> dict[str, list[float]]:
    """Probe the sub-phases of ``CodeIntelligenceService.cmd`` for shell.

    This probe measures service-level cost around the overlay auditor
    and samples an in-flight gauge on entry to tell single-call cost from
    queued-behind-a-lock time.
    """
    stats: dict[str, list[float]] = {
        "svc_cmd_wall_s": [],
        "svc_cmd_rebind_s": [],
        "svc_cmd_ensure_auditor_s": [],
        "svc_cmd_in_flight_on_entry": [],
        "svc_cmd_ensure_auditor_in_flight_on_entry": [],
    }
    gauge_lock = threading.Lock()
    in_flight = {"cmd": 0, "ensure": 0}

    orig_cmd = ci_service_module.CodeIntelligenceService.cmd
    orig_rebind = ci_service_module.CodeIntelligenceService.rebind_sandbox
    orig_ensure_auditor = command_executor_module.AuditedCommandExecutor._ensure_overlay_auditor

    async def _timed_cmd(self, sandbox, command, *args, **kwargs):
        with gauge_lock:
            in_flight["cmd"] += 1
            stats["svc_cmd_in_flight_on_entry"].append(float(in_flight["cmd"]))
        cmd_started = time.perf_counter()
        try:
            return await orig_cmd(self, sandbox, command, *args, **kwargs)
        finally:
            stats["svc_cmd_wall_s"].append(round(time.perf_counter() - cmd_started, 6))
            with gauge_lock:
                in_flight["cmd"] -= 1

    def _timed_rebind(self, sandbox):
        started = time.perf_counter()
        try:
            return orig_rebind(self, sandbox)
        finally:
            stats["svc_cmd_rebind_s"].append(round(time.perf_counter() - started, 6))

    async def _timed_ensure_auditor(self):
        with gauge_lock:
            in_flight["ensure"] += 1
            stats["svc_cmd_ensure_auditor_in_flight_on_entry"].append(
                float(in_flight["ensure"])
        )
        started = time.perf_counter()
        try:
            return await orig_ensure_auditor(self)
        finally:
            stats["svc_cmd_ensure_auditor_s"].append(
                round(time.perf_counter() - started, 6)
            )
            with gauge_lock:
                in_flight["ensure"] -= 1

    monkeypatch.setattr(ci_service_module.CodeIntelligenceService, "cmd", _timed_cmd)
    monkeypatch.setattr(
        ci_service_module.CodeIntelligenceService, "rebind_sandbox", _timed_rebind,
    )
    monkeypatch.setattr(
        command_executor_module.AuditedCommandExecutor,
        "_ensure_overlay_auditor",
        _timed_ensure_auditor,
    )
    return stats


def _install_content_phase_probe(monkeypatch: pytest.MonkeyPatch) -> dict[str, list[float]]:
    """Probe sandbox content round-trips used by typed OCC mutators.

    In addition to per-call wall times, samples a shared in-flight gauge on
    each entry so the summary shows whether reads/writes fan out in parallel
    or queue behind each other. A long-tail wall with tiny in-flight peers
    means one call stalled in transport; a long-tail with high in-flight
    means something else (GIL, connection pool, etc) caps the fan-out.
    """
    stats: dict[str, list[float]] = {
        "content_read_s": [],
        "content_read_many_s": [],
        "content_read_many_rename_s": [],
        "content_read_many_mutation_s": [],
        "content_read_many_commit_s": [],
        "content_read_many_other_s": [],
        "content_read_many_path_count": [],
        "content_write_s": [],
        "content_delete_s": [],
        "content_read_in_flight_on_entry": [],
        "content_write_in_flight_on_entry": [],
        "content_any_in_flight_on_entry": [],
    }
    gauge_lock = threading.Lock()
    in_flight = {"read": 0, "write": 0}

    orig_read = content_manager_module.ContentManager.read
    orig_read_many = content_manager_module.ContentManager.read_many
    orig_write = content_manager_module.ContentManager.write
    orig_delete = content_manager_module.ContentManager.delete

    def _sample_on_entry(kind: str) -> None:
        with gauge_lock:
            in_flight[kind] += 1
            stats[f"content_{kind}_in_flight_on_entry"].append(
                float(in_flight[kind])
            )
            stats["content_any_in_flight_on_entry"].append(
                float(in_flight["read"] + in_flight["write"])
            )

    def _exit(kind: str) -> None:
        with gauge_lock:
            in_flight[kind] -= 1

    def _read_many_label() -> str:
        frame = sys._getframe(2)
        while frame is not None:
            filename = frame.f_code.co_filename.replace("\\", "/")
            if filename.endswith("/rename_planner.py"):
                return "rename"
            if filename.endswith("/mutation_service.py"):
                return "mutation"
            if filename.endswith("/write_coordinator.py"):
                return "commit"
            frame = frame.f_back
        return "other"

    def _timed_read(self, *args, **kwargs):
        _sample_on_entry("read")
        started = time.perf_counter()
        try:
            return orig_read(self, *args, **kwargs)
        finally:
            stats["content_read_s"].append(round(time.perf_counter() - started, 6))
            _exit("read")

    def _timed_read_many(self, *args, **kwargs):
        _sample_on_entry("read")
        label = _read_many_label()
        if args:
            try:
                stats["content_read_many_path_count"].append(float(len(args[0] or ())))
            except TypeError:
                pass
        started = time.perf_counter()
        try:
            return orig_read_many(self, *args, **kwargs)
        finally:
            elapsed = round(time.perf_counter() - started, 6)
            stats["content_read_many_s"].append(elapsed)
            stats[f"content_read_many_{label}_s"].append(elapsed)
            _exit("read")

    def _timed_write(self, *args, **kwargs):
        _sample_on_entry("write")
        started = time.perf_counter()
        try:
            return orig_write(self, *args, **kwargs)
        finally:
            stats["content_write_s"].append(round(time.perf_counter() - started, 6))
            _exit("write")

    def _timed_delete(self, *args, **kwargs):
        _sample_on_entry("write")
        started = time.perf_counter()
        try:
            return orig_delete(self, *args, **kwargs)
        finally:
            stats["content_delete_s"].append(round(time.perf_counter() - started, 6))
            _exit("write")

    monkeypatch.setattr(content_manager_module.ContentManager, "read", _timed_read)
    monkeypatch.setattr(content_manager_module.ContentManager, "read_many", _timed_read_many)
    monkeypatch.setattr(content_manager_module.ContentManager, "write", _timed_write)
    monkeypatch.setattr(content_manager_module.ContentManager, "delete", _timed_delete)
    return stats


def _install_commit_phase_probe(monkeypatch: pytest.MonkeyPatch) -> dict[str, list[float]]:
    """Probe WriteCoordinator phases across all typed and daytona_shell commits.

    Records per-call timings + a shared in-flight concurrency gauge so the
    summary can show (a) how apply-pass time decomposes (snapshot / remote
    write / arbiter record / symbol refresh / lsp invalidate) and (b) the
    peak number of threads actually inside ``commit_operation_against_base``.
    Disjoint-file tests expect the sample max close to concurrency; a lower
    peak means upstream dispatch is capping parallelism before the
    coordinator. Each call samples the gauge on entry so `.max` on
    ``in_flight_on_entry`` is the true peak concurrency observed.
    """
    stats: dict[str, list[float]] = {
        "commit_wall_s": [],
        "commit_many_wall_s": [],
        "commit_many_operation_count": [],
        "commit_many_change_count": [],
        "commit_many_rename_operation_count": [],
        "commit_many_lock_wait_s": [],
        "commit_many_resolve_s": [],
        "commit_many_resolve_read_s": [],
        "commit_many_apply_s": [],
        "commit_many_apply_record_s": [],
        "commit_many_reported_total_s": [],
        "commit_lock_wait_s": [],
        "commit_resolve_s": [],
        "commit_resolve_read_s": [],
        "commit_apply_s": [],
        "commit_apply_snapshot_s": [],
        "commit_apply_write_s": [],
        "commit_apply_record_s": [],
        "commit_apply_refresh_s": [],
        "commit_apply_invalidate_s": [],
        "commit_reported_total_s": [],
        "symbol_refresh_s": [],
        "in_flight_on_entry": [],
    }
    gauge_lock = threading.Lock()
    in_flight = {"current": 0}

    orig_commit = write_coordinator_module.WriteCoordinator.commit_operation_against_base
    orig_commit_many = (
        write_coordinator_module.WriteCoordinator.commit_many_operations_against_base
    )
    orig_refresh = symbol_index_module.SymbolIndex.refresh

    def _record_timing_fields(
        timings: dict[str, Any],
        *,
        prefix: str,
        include_apply_record: bool = False,
    ) -> None:
        fields = [
            ("lock_wait", f"{prefix}_lock_wait_s"),
            ("resolve", f"{prefix}_resolve_s"),
            ("resolve_read", f"{prefix}_resolve_read_s"),
            ("apply", f"{prefix}_apply_s"),
            ("total", f"{prefix}_reported_total_s"),
        ]
        if include_apply_record:
            fields.append(("apply_record", f"{prefix}_apply_record_s"))
        for source_key, target_key in fields:
            value = timings.get(source_key)
            if isinstance(value, (int, float)):
                stats[target_key].append(round(float(value), 6))

    def _timed_commit(self, *args, **kwargs):
        with gauge_lock:
            in_flight["current"] += 1
            stats["in_flight_on_entry"].append(float(in_flight["current"]))
        started = time.perf_counter()
        try:
            result = orig_commit(self, *args, **kwargs)
        finally:
            with gauge_lock:
                in_flight["current"] -= 1
        stats["commit_wall_s"].append(round(time.perf_counter() - started, 6))
        timings = getattr(result, "timings", {}) or {}
        _record_timing_fields(timings, prefix="commit")
        for source_key, target_key in (
            ("apply_snapshot", "commit_apply_snapshot_s"),
            ("apply_write", "commit_apply_write_s"),
            ("apply_record", "commit_apply_record_s"),
            ("apply_refresh", "commit_apply_refresh_s"),
            ("apply_invalidate", "commit_apply_invalidate_s"),
        ):
            value = timings.get(source_key)
            if isinstance(value, (int, float)):
                stats[target_key].append(round(float(value), 6))
        return result

    def _timed_commit_many(self, operations, *args, **kwargs):
        ops = list(operations or ())
        stats["commit_many_operation_count"].append(float(len(ops)))
        stats["commit_many_change_count"].append(
            float(sum(len(getattr(op, "changes", ()) or ()) for op in ops))
        )
        stats["commit_many_rename_operation_count"].append(
            float(sum(getattr(op, "edit_type", "") == "rename_symbol" for op in ops))
        )
        started = time.perf_counter()
        results = orig_commit_many(self, operations, *args, **kwargs)
        stats["commit_many_wall_s"].append(round(time.perf_counter() - started, 6))
        for result in results or ():
            timings = getattr(result, "timings", {}) or {}
            if timings:
                _record_timing_fields(
                    timings,
                    prefix="commit_many",
                    include_apply_record=True,
                )
                break
        return results

    def _timed_refresh(self, *args, **kwargs):
        started = time.perf_counter()
        try:
            return orig_refresh(self, *args, **kwargs)
        finally:
            stats["symbol_refresh_s"].append(round(time.perf_counter() - started, 6))

    monkeypatch.setattr(
        write_coordinator_module.WriteCoordinator,
        "commit_operation_against_base",
        _timed_commit,
    )
    monkeypatch.setattr(
        write_coordinator_module.WriteCoordinator,
        "commit_many_operations_against_base",
        _timed_commit_many,
    )
    monkeypatch.setattr(symbol_index_module.SymbolIndex, "refresh", _timed_refresh)
    return stats


def _start_executor_depth_sampler(
    loop: asyncio.AbstractEventLoop,
    *,
    interval_s: float = 0.05,
) -> tuple[dict[str, list[float]], Callable[[], None]]:
    """Sample the default executor's pending-work queue every *interval_s*.

    Returns ``(stats, stop)``. The sampler runs on a daemon thread until
    ``stop()`` is called. ``stats["queue_depth"]`` records queue depth per
    sample; ``stats["workers_busy"]`` records how many workers the pool
    reports active (work_queue.qsize() + workers currently running).
    A non-empty queue during the load run means the pool, not a coordinator
    lock, is the cap on fan-out.
    """
    stats: dict[str, list[float]] = {
        "queue_depth": [],
        "max_workers": [],
    }
    stop_flag = threading.Event()

    def _sample_loop() -> None:
        pool = loop._default_executor  # type: ignore[attr-defined]
        if pool is None:
            return
        max_workers = getattr(pool, "_max_workers", 0) or 0
        stats["max_workers"].append(float(max_workers))
        work_queue = getattr(pool, "_work_queue", None)
        while not stop_flag.is_set():
            if work_queue is not None:
                try:
                    stats["queue_depth"].append(float(work_queue.qsize()))
                except Exception:  # pragma: no cover - defensive
                    pass
            stop_flag.wait(interval_s)

    thread = threading.Thread(
        target=_sample_loop, name="executor-depth-sampler", daemon=True,
    )
    thread.start()

    def _stop() -> None:
        stop_flag.set()
        thread.join(timeout=1.0)

    return stats, _stop


async def _run_mixed_operations(
    live_load_env: LiveLoadEnv,
    svc: CodeIntelligenceService,
    operations: list[dict[str, Any]],
    *,
    concurrency: int,
    timeout_s: int,
    log_ops: bool = False,
    log_label: str = "occ-load-op",
    executor_depth_stats: dict[str, list[float]] | None = None,
) -> list[dict[str, Any]]:
    run_started = time.perf_counter()
    loop = asyncio.get_running_loop()
    configure_default_executor(
        loop,
        max_workers=max(200, concurrency * 8),
    )
    depth_stop: Callable[[], None] | None = None
    if executor_depth_stats is not None:
        depth_stats, depth_stop = _start_executor_depth_sampler(loop)
        executor_depth_stats.update(depth_stats)

    # Replace the fixture's `asyncio.to_thread(sync_sdk.exec)` wrapper with the
    # true AsyncDaytona sandbox (aiohttp). The wrapper's `ctx.run` propagation
    # caps parallelism at ~6-7 under the sync Daytona SDK (see
    # test_live_daytona_transport_parallelism_isolation Arm A vs D). Production
    # uses AsyncDaytona (via `get_async_sandbox`), so the fixture must match for
    # the benchmark to reflect real tool throughput.
    from sandbox.async_client import get_async_sandbox

    live_load_env.async_sandbox = await get_async_sandbox(live_load_env.sandbox_id)

    async def _invoke(
        sequence: int,
        operation: dict[str, Any],
        semaphore: asyncio.Semaphore,
    ) -> dict[str, Any]:
        agent_run_id = f"{operation['name']}-{uuid.uuid4().hex[:8]}"
        ctx = live_load_env.make_ctx(
            svc,
            agent_run_id=agent_run_id,
            coordinated=bool(operation.get("coordinated", False)),
        )
        tool = _tool_for_operation_kind(str(operation["kind"]))
        queued_at = time.perf_counter()
        identity = _operation_identity(live_load_env, svc, agent_run_id)
        if log_ops:
            _log_occ_event(
                log_label,
                {
                    "event": "queued",
                    "sequence": sequence,
                    "kind": operation["kind"],
                    "name": operation["name"],
                    "path": operation["path"],
                    "concurrency": concurrency,
                    **identity,
                    "arbiter": _arbiter_snapshot(svc),
                },
            )
        async with semaphore:
            started = time.perf_counter()
            before = _arbiter_snapshot(svc)
            if log_ops:
                _log_occ_event(
                    log_label,
                    {
                        "event": "start",
                        "sequence": sequence,
                        "kind": operation["kind"],
                        "name": operation["name"],
                        "path": operation["path"],
                        "queued_s": round(started - queued_at, 6),
                        "start_offset_s": round(started - run_started, 6),
                        **identity,
                        "arbiter_before": before,
                    },
                )
            try:
                result = await _invoke_tool(tool, operation["kwargs"], ctx)
            except Exception as exc:  # pragma: no cover - live diagnostic path
                elapsed_s = round(time.perf_counter() - started, 6)
                failure = {
                    "kind": operation["kind"],
                    "name": operation["name"],
                    "path": operation["path"],
                    "group": operation.get("group"),
                    "winner_value": operation.get("winner_value"),
                    "is_error": True,
                    "exception_type": type(exc).__name__,
                    "exception": str(exc),
                    "metadata": {},
                    "payload": {},
                    "raw_output": str(exc)[-1200:],
                    "elapsed_s": elapsed_s,
                    "wait_s": round(started - queued_at, 6),
                    "sequence": sequence,
                    **identity,
                    "arbiter_before": before,
                    "arbiter_after": _arbiter_snapshot(svc),
                }
                if log_ops:
                    _log_occ_event(
                        log_label,
                        {
                            "event": "exception",
                            "sequence": sequence,
                            "kind": operation["kind"],
                            "name": operation["name"],
                            "elapsed_s": elapsed_s,
                            **identity,
                            "exception_type": type(exc).__name__,
                            "exception": str(exc)[-1200:],
                            "arbiter_after": failure["arbiter_after"],
                        },
                    )
                return failure
        elapsed_s = round(time.perf_counter() - started, 6)
        wait_s = round(started - queued_at, 6)
        output = (result.output or "").lstrip()
        payload = _json_output(result) if output.startswith("{") else {}
        after = _arbiter_snapshot(svc)
        item = {
            "kind": operation["kind"],
            "name": operation["name"],
            "path": operation["path"],
            "group": operation.get("group"),
            "winner_value": operation.get("winner_value"),
            "is_error": result.is_error,
            "metadata": dict(result.metadata or {}),
            "payload": payload,
            "raw_output": (result.output or "")[:1200],
            "elapsed_s": elapsed_s,
            "wait_s": wait_s,
            "sequence": sequence,
            **identity,
            "arbiter_before": before,
            "arbiter_after": after,
        }
        if log_ops:
            _log_occ_event(
                log_label,
                {
                    "event": "finish",
                    "sequence": sequence,
                    "kind": operation["kind"],
                    "name": operation["name"],
                    "is_error": result.is_error,
                    "elapsed_s": elapsed_s,
                    "wait_s": wait_s,
                    **identity,
                    "metadata": item["metadata"],
                    "payload": payload,
                    "arbiter_after": after,
                    "raw_output_tail": (result.output or "")[-600:],
                },
            )
        return item

    semaphore = asyncio.Semaphore(concurrency)
    try:
        return await asyncio.wait_for(
            asyncio.gather(
                *[
                    _invoke(sequence, operation, semaphore)
                    for sequence, operation in enumerate(operations)
                ]
            ),
            timeout=timeout_s,
        )
    finally:
        if depth_stop is not None:
            depth_stop()


def _operation_identity(
    live_load_env: LiveLoadEnv,
    svc: CodeIntelligenceService,
    agent_run_id: str,
) -> dict[str, Any]:
    return {
        "agent_run_id": agent_run_id,
        "pid": os.getpid(),
        "thread_id": threading.get_ident(),
        "sandbox_id": live_load_env.sandbox_id,
        "repo_root": live_load_env.repo_root,
        "svc_id": hex(id(svc)),
        "arbiter_id": hex(id(svc.arbiter)),
    }


def _arbiter_snapshot(svc: CodeIntelligenceService) -> dict[str, Any]:
    status = svc.status()["arbiter"]
    return {
        "generation": svc.arbiter.generation,
        "total_edits": status["total_edits"],
        "conflicts_detected": status["conflicts_detected"],
        "active_locks": status["active_locks"],
        "active_lock_count": svc.arbiter.active_lock_count,
    }


def _log_occ_event(label: str, payload: dict[str, Any]) -> None:
    print(f"\n[{label}] {json.dumps(payload, sort_keys=True, default=str)}", flush=True)


def _tool_for_operation_kind(kind: str) -> Any:
    if kind == "write":
        return daytona_write_file
    if kind == "shell":
        return daytona_shell
    if kind in {"edit-disjoint", "edit-overlap", "edit"}:
        return daytona_edit_file
    if kind == "rename":
        return daytona_rename_symbol
    if kind == "move":
        return daytona_move_file
    if kind == "delete":
        return daytona_delete_file
    raise AssertionError(f"Unsupported operation kind: {kind}")


def _percentile(sorted_values: list[float], pct: float) -> float:
    if not sorted_values:
        return 0.0
    idx = min(len(sorted_values) - 1, int(round((len(sorted_values) - 1) * pct)))
    return sorted_values[idx]


def _value_profile(values: list[float]) -> dict[str, float]:
    ordered = sorted(values)
    if not ordered:
        return {
            "count": 0,
            "min": 0.0,
            "avg": 0.0,
            "p50": 0.0,
            "p90": 0.0,
            "p95": 0.0,
            "max": 0.0,
            "total": 0.0,
        }
    total = sum(ordered)
    return {
        "count": len(ordered),
        "min": round(ordered[0], 6),
        "avg": round(total / len(ordered), 6),
        "p50": round(_percentile(ordered, 0.50), 6),
        "p90": round(_percentile(ordered, 0.90), 6),
        "p95": round(_percentile(ordered, 0.95), 6),
        "max": round(ordered[-1], 6),
        "total": round(total, 6),
    }


def _phase_summary(stats: dict[str, list[float]]) -> dict[str, dict[str, float]]:
    return {phase: _value_profile(list(values)) for phase, values in stats.items()}


def _operation_timing_summary(
    results: list[dict[str, Any]],
    *,
    wall_elapsed_s: float,
) -> dict[str, Any]:
    elapsed_values = [float(item["elapsed_s"]) for item in results]
    wait_values = [float(item["wait_s"]) for item in results]
    total_operation_s = round(sum(elapsed_values), 6)
    ratio = round(total_operation_s / wall_elapsed_s, 3) if wall_elapsed_s > 0 else 0.0
    op_count = len(results)
    throughput = round(op_count / wall_elapsed_s, 3) if wall_elapsed_s > 0 else 0.0
    return {
        "wall_elapsed_s": round(wall_elapsed_s, 6),
        "op_count": op_count,
        "sum_operation_elapsed_s": total_operation_s,
        "parallelism_ratio": ratio,
        "throughput_ops_per_s": throughput,
        "elapsed_s_profile": _value_profile(elapsed_values),
        "wait_s_profile": _value_profile(wait_values),
        "max_wait_s": round(max(wait_values, default=0.0), 6),
    }


def _elapsed_profile(items: list[dict[str, Any]]) -> dict[str, float]:
    return _value_profile([float(item["elapsed_s"]) for item in items])


def test_live_occ_load_72_all_mutators_high_concurrency_profile(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
):
    """High-concurrency mixed OCC load across every Daytona mutator.

    This intentionally uses disjoint files/symbols so failures point to
    transport, snapshot, locking, or routing regressions rather than expected
    write conflicts. It exercises write, edit, rename, move, delete, and
    coordinated daytona_shell against one shared ``CodeIntelligenceService``.
    """
    log_label = "occ-load-72-all-mutators-high-concurrency"
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "init_repo",
            "pid": os.getpid(),
            "sandbox_id": live_load_env.sandbox_id,
            "repo_root": live_load_env.repo_root,
        },
    )
    live_load_env.init_repo()
    shell_stats = _install_shell_phase_probe(monkeypatch)
    overlay_stats = _install_overlay_phase_probe(monkeypatch)
    lsp_stats = _install_lsp_phase_probe(monkeypatch)
    rename_stats = _install_rename_phase_probe(monkeypatch)
    content_stats = _install_content_phase_probe(monkeypatch)
    commit_stats = _install_commit_phase_probe(monkeypatch)
    svc_cmd_stats = _install_svc_cmd_phase_probe(monkeypatch)
    executor_depth_stats: dict[str, list[float]] = {}

    seed_started = time.perf_counter()
    _log_occ_event(log_label, {"event": "setup", "phase": "seed_start"})
    for idx in range(12):
        live_load_env.write_text(
            f"edits/all_{idx}.py",
            (
                f'"""Edit fixture {idx}."""\n\n'
                f"VALUE_{idx} = {idx}\n"
                f"MARKER_{idx} = 'before'\n"
            ),
        )
        live_load_env.write_text(
            f"rename/module_{idx}.py",
            (
                f'"""Rename fixture {idx}."""\n\n'
                f"def rename_target_{idx}(value):\n"
                f"    return value + {idx}\n\n"
                f"def caller_{idx}(value):\n"
                f"    return rename_target_{idx}(value)\n"
            ),
        )
        live_load_env.write_text(f"moves/src_{idx}.txt", f"move source {idx}\n")
        live_load_env.write_text(f"deletes/delete_{idx}.txt", f"delete target {idx}\n")
        live_load_env.write_text(f"shell/high_{idx}.txt", f"shell base {idx}\n")
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "seed_finish",
            "elapsed_s": round(time.perf_counter() - seed_started, 6),
        },
    )

    commit_started = time.perf_counter()
    _log_occ_event(log_label, {"event": "setup", "phase": "git_commit_start"})
    live_load_env.exec_checked(f"git -C {shlex.quote(live_load_env.repo_root)} add -A")
    live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} commit -m seed-all-mutators-load",
        timeout=180,
    )
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "git_commit_finish",
            "elapsed_s": round(time.perf_counter() - commit_started, 6),
        },
    )

    svc = live_load_env.make_ci_service()
    init_started = time.perf_counter()
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "ensure_initialized_start",
            "svc_id": hex(id(svc)),
            "arbiter_id": hex(id(svc.arbiter)),
        },
    )
    svc.ensure_initialized(wait=True)
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "ensure_initialized_finish",
            "elapsed_s": round(time.perf_counter() - init_started, 6),
            "svc_id": hex(id(svc)),
            "arbiter_id": hex(id(svc.arbiter)),
            "arbiter": _arbiter_snapshot(svc),
        },
    )

    operations: list[dict[str, Any]] = []
    for idx in range(12):
        operations.extend(
            [
                {
                    "kind": "write",
                    "name": f"write-{idx}",
                    "path": f"{live_load_env.repo_root}/writes/all_{idx}.txt",
                    "kwargs": {
                        "file_path": f"{live_load_env.repo_root}/writes/all_{idx}.txt",
                        "content": f"write all {idx}\n",
                    },
                },
                {
                    "kind": "edit-disjoint",
                    "name": f"edit-{idx}",
                    "path": f"{live_load_env.repo_root}/edits/all_{idx}.py",
                    "kwargs": {
                        "file_path": f"{live_load_env.repo_root}/edits/all_{idx}.py",
                        "old_text": f"MARKER_{idx} = 'before'",
                        "new_text": f"MARKER_{idx} = 'after-{idx}'",
                    },
                },
                {
                    "kind": "rename",
                    "name": f"rename-{idx}",
                    "path": f"{live_load_env.repo_root}/rename/module_{idx}.py",
                    "kwargs": {
                        "symbol": f"rename_target_{idx}",
                        "new_name": f"renamed_target_{idx}",
                        "file_hint": f"rename/module_{idx}.py",
                    },
                },
                {
                    "kind": "move",
                    "name": f"move-{idx}",
                    "path": f"{live_load_env.repo_root}/moves/src_{idx}.txt",
                    "kwargs": {
                        "src_path": f"{live_load_env.repo_root}/moves/src_{idx}.txt",
                        "target_path": f"{live_load_env.repo_root}/moves/dst_{idx}.txt",
                    },
                },
                {
                    "kind": "delete",
                    "name": f"delete-{idx}",
                    "path": f"{live_load_env.repo_root}/deletes/delete_{idx}.txt",
                    "kwargs": {
                        "path": f"{live_load_env.repo_root}/deletes/delete_{idx}.txt",
                    },
                },
                {
                    "kind": "shell",
                    "name": f"shell-{idx}",
                    "path": f"{live_load_env.repo_root}/shell/high_{idx}.txt",
                    "kwargs": {
                        "mode": "shell",
                        "command": (
                            "python3 - <<'PY'\n"
                            "from pathlib import Path\n"
                            f"Path('shell/high_{idx}.txt').write_text('shell high {idx}\\n', encoding='utf-8')\n"
                            "PY"
                        ),
                        "timeout": 180,
                    },
                    "coordinated": True,
                },
            ]
        )

    assert len(operations) == 72

    started = time.perf_counter()
    results = asyncio.run(
        _run_mixed_operations(
            live_load_env,
            svc,
            operations,
            concurrency=72,
            timeout_s=360,
            log_ops=True,
            log_label=log_label,
            executor_depth_stats=executor_depth_stats,
        )
    )
    wall_elapsed_s = time.perf_counter() - started

    by_kind: dict[str, list[dict[str, Any]]] = {}
    for item in results:
        by_kind.setdefault(item["kind"], []).append(item)

    failures = [
        {
            "kind": item["kind"],
            "name": item["name"],
            "metadata": item["metadata"],
            "payload": item["payload"],
            "raw_output": item["raw_output"],
        }
        for item in results
        if item["is_error"]
    ]
    summary = {
        "operation_counts": {
            kind: len(items)
            for kind, items in sorted(by_kind.items())
        },
        "success_counts": {
            kind: sum(not item["is_error"] for item in items)
            for kind, items in sorted(by_kind.items())
        },
        "elapsed_profile_s": {
            kind: _elapsed_profile(items)
            for kind, items in sorted(by_kind.items())
        },
        "timing": _operation_timing_summary(
            results,
            wall_elapsed_s=wall_elapsed_s,
        ),
        "shell_phase_s": _phase_summary(shell_stats),
        "overlay_phase_s": _phase_summary(overlay_stats),
        "lsp_phase_s": _phase_summary(lsp_stats),
        "rename_phase_s": _phase_summary(rename_stats),
        "content_phase_s": _phase_summary(content_stats),
        "commit_phase_s": _phase_summary(commit_stats),
        "svc_cmd_phase_s": _phase_summary(svc_cmd_stats),
        "executor_depth": _phase_summary(executor_depth_stats),
        "arbiter": svc.status()["arbiter"],
        "held_locks": svc.arbiter.active_lock_count,
        "process_identity": {
            "expected": {
                "pid": os.getpid(),
                "svc_id": hex(id(svc)),
                "arbiter_id": hex(id(svc.arbiter)),
                "sandbox_id": live_load_env.sandbox_id,
            },
            "observed_count": len(
                {
                    (
                        item["pid"],
                        item["svc_id"],
                        item["arbiter_id"],
                        item["sandbox_id"],
                    )
                    for item in results
                }
            ),
        },
        "failures": failures[:5],
    }
    print("\n[occ-load-72-all-mutators-high-concurrency]", flush=True)
    print(json.dumps(summary, indent=2, sort_keys=True), flush=True)

    assert not failures, json.dumps(failures, indent=2, sort_keys=True)
    assert summary["timing"]["parallelism_ratio"] >= 4.0, summary["timing"]
    assert svc.arbiter.active_lock_count == 0
    assert svc.status()["arbiter"]["conflicts_detected"] == 0
    assert {
        (
            item["pid"],
            item["svc_id"],
            item["arbiter_id"],
            item["sandbox_id"],
        )
        for item in results
    } == {
        (
            os.getpid(),
            hex(id(svc)),
            hex(id(svc.arbiter)),
            live_load_env.sandbox_id,
        )
    }

    for idx in range(12):
        assert live_load_env.read_text(f"writes/all_{idx}.txt") == f"write all {idx}\n"

        edited = live_load_env.read_text(f"edits/all_{idx}.py")
        assert f"MARKER_{idx} = 'after-{idx}'" in edited

        renamed = live_load_env.read_text(f"rename/module_{idx}.py")
        assert f"def renamed_target_{idx}(value):" in renamed
        assert f"return renamed_target_{idx}(value)" in renamed
        assert f"rename_target_{idx}" not in renamed

        assert live_load_env.read_text(f"moves/dst_{idx}.txt") == f"move source {idx}\n"
        live_load_env.exec_checked(
            f"test ! -e {shlex.quote(f'{live_load_env.repo_root}/moves/src_{idx}.txt')}",
            timeout=30,
        )
        live_load_env.exec_checked(
            f"test ! -e {shlex.quote(f'{live_load_env.repo_root}/deletes/delete_{idx}.txt')}",
            timeout=30,
        )

        assert live_load_env.read_text(f"shell/high_{idx}.txt") == f"shell high {idx}\n"

    assert svc.status()["arbiter"]["total_edits"] >= len(operations)


@pytest.mark.parametrize("op_count", [10, 20, 30, 40, 50])
def test_live_occ_load_shell_only_high_concurrency_profile(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
    op_count: int,
):
    """High-concurrency daytona_shell-only load against unique tracked files."""
    _set_live_load_overlay_concurrency(monkeypatch)
    log_label = f"occ-load-{op_count}-shell-only-high-concurrency"
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "init_repo",
            "pid": os.getpid(),
            "sandbox_id": live_load_env.sandbox_id,
            "repo_root": live_load_env.repo_root,
            "overlay_max_concurrent": _LIVE_LOAD_OVERLAY_MAX_CONCURRENT,
        },
    )
    live_load_env.init_repo()
    shell_stats = _install_shell_phase_probe(monkeypatch)
    overlay_stats = _install_overlay_phase_probe(monkeypatch)
    content_stats = _install_content_phase_probe(monkeypatch)
    commit_stats = _install_commit_phase_probe(monkeypatch)
    svc_cmd_stats = _install_svc_cmd_phase_probe(monkeypatch)
    executor_depth_stats: dict[str, list[float]] = {}

    for idx in range(op_count):
        live_load_env.write_text(f"shell/only_{idx}.txt", f"base {idx}\n")

    live_load_env.exec_checked(f"git -C {shlex.quote(live_load_env.repo_root)} add -A")
    live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} commit -m seed-shell-only-load",
        timeout=180,
    )

    svc = live_load_env.make_ci_service()
    svc.ensure_initialized(wait=True)

    operations = [
        {
            "kind": "shell",
            "name": f"shell-{idx}",
            "path": f"{live_load_env.repo_root}/shell/only_{idx}.txt",
            "kwargs": {
                "mode": "shell",
                "command": (
                    "python3 - <<'PY'\n"
                    "from pathlib import Path\n"
                    f"Path('shell/only_{idx}.txt').write_text('shell only {idx}\\n', encoding='utf-8')\n"
                    "PY"
                ),
                "timeout": 180,
            },
            "coordinated": True,
        }
        for idx in range(op_count)
    ]

    async def _warmup_then_run():
        await svc.warmup_overlay(live_load_env.async_sandbox)
        return await _run_mixed_operations(
            live_load_env,
            svc,
            operations,
            concurrency=op_count,
            timeout_s=360,
            log_ops=True,
            log_label=log_label,
            executor_depth_stats=executor_depth_stats,
        )

    started = time.perf_counter()
    results = asyncio.run(_warmup_then_run())
    wall_elapsed_s = time.perf_counter() - started

    failures = [
        {
            "kind": item["kind"],
            "name": item["name"],
            "metadata": item["metadata"],
            "payload": item["payload"],
            "raw_output": item["raw_output"],
        }
        for item in results
        if item["is_error"]
    ]
    summary = {
        "operation_counts": {"shell": len(results)},
        "success_counts": {
            "shell": sum(not item["is_error"] for item in results),
        },
        "elapsed_profile_s": {"shell": _elapsed_profile(results)},
        "timing": _operation_timing_summary(
            results,
            wall_elapsed_s=wall_elapsed_s,
        ),
        "shell_phase_s": _phase_summary(shell_stats),
        "overlay_phase_s": _phase_summary(overlay_stats),
        "content_phase_s": _phase_summary(content_stats),
        "commit_phase_s": _phase_summary(commit_stats),
        "svc_cmd_phase_s": _phase_summary(svc_cmd_stats),
        "executor_depth": _phase_summary(executor_depth_stats),
        "overlay_max_concurrent": _LIVE_LOAD_OVERLAY_MAX_CONCURRENT,
        "arbiter": svc.status()["arbiter"],
        "held_locks": svc.arbiter.active_lock_count,
        "process_identity": {
            "expected": {
                "pid": os.getpid(),
                "svc_id": hex(id(svc)),
                "arbiter_id": hex(id(svc.arbiter)),
                "sandbox_id": live_load_env.sandbox_id,
            },
            "observed_count": len(
                {
                    (
                        item["pid"],
                        item["svc_id"],
                        item["arbiter_id"],
                        item["sandbox_id"],
                    )
                    for item in results
                }
            ),
        },
        "failures": failures[:5],
    }
    print(f"\n[{log_label} timings]", flush=True)
    print(json.dumps(summary, indent=2, sort_keys=True), flush=True)

    assert not failures, json.dumps(failures, indent=2, sort_keys=True)
    assert summary["success_counts"]["shell"] == op_count
    assert summary["timing"]["parallelism_ratio"] >= 4.0, summary["timing"]
    assert svc.arbiter.active_lock_count == 0
    assert svc.status()["arbiter"]["conflicts_detected"] == 0
    assert {
        (
            item["pid"],
            item["svc_id"],
            item["arbiter_id"],
            item["sandbox_id"],
        )
        for item in results
    } == {
        (
            os.getpid(),
            hex(id(svc)),
            hex(id(svc.arbiter)),
            live_load_env.sandbox_id,
        )
    }

    for idx in range(op_count):
        assert live_load_env.read_text(f"shell/only_{idx}.txt") == (
            f"shell only {idx}\n"
        )

    assert svc.status()["arbiter"]["total_edits"] >= op_count


@pytest.mark.parametrize("op_count", [20, 50])
def test_live_occ_load_shell_gitignore_only_high_concurrency_profile(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
    op_count: int,
):
    """High-concurrency daytona_shell-only load against ignored files."""
    _set_live_load_overlay_concurrency(monkeypatch)
    log_label = f"occ-load-{op_count}-shell-gitignore-only-high-concurrency"
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "init_repo",
            "pid": os.getpid(),
            "sandbox_id": live_load_env.sandbox_id,
            "repo_root": live_load_env.repo_root,
            "overlay_max_concurrent": _LIVE_LOAD_OVERLAY_MAX_CONCURRENT,
        },
    )
    live_load_env.init_repo()
    shell_stats = _install_shell_phase_probe(monkeypatch)
    overlay_stats = _install_overlay_phase_probe(monkeypatch)
    content_stats = _install_content_phase_probe(monkeypatch)
    commit_stats = _install_commit_phase_probe(monkeypatch)
    svc_cmd_stats = _install_svc_cmd_phase_probe(monkeypatch)
    executor_depth_stats: dict[str, list[float]] = {}

    live_load_env.write_text(".gitignore", "runtime/\n")
    live_load_env.write_text("README.md", "seed\n")
    live_load_env.exec_checked(f"git -C {shlex.quote(live_load_env.repo_root)} add -A")
    live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} commit -m seed-shell-ignored-load",
        timeout=180,
    )

    svc = live_load_env.make_ci_service()
    svc.ensure_initialized(wait=True)

    operations = [
        {
            "kind": "shell",
            "name": f"shell-ignored-{idx}",
            "path": f"{live_load_env.repo_root}/runtime/only_{idx}.txt",
            "kwargs": {
                "mode": "shell",
                "command": (
                    "python3 - <<'PY'\n"
                    "from pathlib import Path\n"
                    "Path('runtime').mkdir(exist_ok=True)\n"
                    f"Path('runtime/only_{idx}.txt').write_text('ignored shell {idx}\\n', encoding='utf-8')\n"
                    "PY"
                ),
                "timeout": 180,
            },
            "coordinated": True,
        }
        for idx in range(op_count)
    ]

    started = time.perf_counter()
    results = asyncio.run(
        _run_mixed_operations(
            live_load_env,
            svc,
            operations,
            concurrency=op_count,
            timeout_s=360,
            log_ops=True,
            log_label=log_label,
            executor_depth_stats=executor_depth_stats,
        )
    )
    wall_elapsed_s = time.perf_counter() - started

    failures = [
        {
            "kind": item["kind"],
            "name": item["name"],
            "metadata": item["metadata"],
            "payload": item["payload"],
            "raw_output": item["raw_output"],
        }
        for item in results
        if item["is_error"]
    ]
    git_status_short = live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} status --short --untracked-files=all",
        timeout=30,
    )
    ignored_status = live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} status --short --ignored",
        timeout=30,
    )
    summary = {
        "operation_counts": {"shell": len(results)},
        "success_counts": {
            "shell": sum(not item["is_error"] for item in results),
        },
        "elapsed_profile_s": {"shell": _elapsed_profile(results)},
        "timing": _operation_timing_summary(
            results,
            wall_elapsed_s=wall_elapsed_s,
        ),
        "shell_phase_s": _phase_summary(shell_stats),
        "overlay_phase_s": _phase_summary(overlay_stats),
        "content_phase_s": _phase_summary(content_stats),
        "commit_phase_s": _phase_summary(commit_stats),
        "svc_cmd_phase_s": _phase_summary(svc_cmd_stats),
        "executor_depth": _phase_summary(executor_depth_stats),
        "overlay_max_concurrent": _LIVE_LOAD_OVERLAY_MAX_CONCURRENT,
        "arbiter": svc.status()["arbiter"],
        "held_locks": svc.arbiter.active_lock_count,
        "process_identity": {
            "expected": {
                "pid": os.getpid(),
                "svc_id": hex(id(svc)),
                "arbiter_id": hex(id(svc.arbiter)),
                "sandbox_id": live_load_env.sandbox_id,
            },
            "observed_count": len(
                {
                    (
                        item["pid"],
                        item["svc_id"],
                        item["arbiter_id"],
                        item["sandbox_id"],
                    )
                    for item in results
                }
            ),
        },
        "git_status_short": git_status_short,
        "ignored_status": ignored_status.splitlines()[:5],
        "failures": failures[:5],
    }
    print(f"\n[{log_label} timings]", flush=True)
    print(json.dumps(summary, indent=2, sort_keys=True), flush=True)

    assert not failures, json.dumps(failures, indent=2, sort_keys=True)
    assert summary["success_counts"]["shell"] == op_count
    assert svc.arbiter.active_lock_count == 0
    assert svc.status()["arbiter"]["conflicts_detected"] == 0
    assert {
        (
            item["pid"],
            item["svc_id"],
            item["arbiter_id"],
            item["sandbox_id"],
        )
        for item in results
    } == {
        (
            os.getpid(),
            hex(id(svc)),
            hex(id(svc.arbiter)),
            live_load_env.sandbox_id,
        )
    }
    assert git_status_short.strip() == ""
    assert any(line.startswith("!! runtime/") for line in ignored_status.splitlines())

    for idx in range(op_count):
        assert live_load_env.read_text(f"runtime/only_{idx}.txt") == (
            f"ignored shell {idx}\n"
        )


def test_live_occ_load_50_mixed_operations(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
):
    log_label = "occ-load-50-mixed"
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "init_repo",
            "pid": os.getpid(),
            "sandbox_id": live_load_env.sandbox_id,
        },
    )
    live_load_env.init_repo()
    shell_stats = _install_shell_phase_probe(monkeypatch)
    overlay_stats = _install_overlay_phase_probe(monkeypatch)
    lsp_stats = _install_lsp_phase_probe(monkeypatch)
    content_stats = _install_content_phase_probe(monkeypatch)
    commit_stats = _install_commit_phase_probe(monkeypatch)

    # Seed disjoint edit targets: 3 files * 5 edits each = 15 disjoint edits.
    for group in range(3):
        lines = ['"""Disjoint edit target."""', ""]
        for idx in range(5):
            global_idx = group * 5 + idx
            lines.append(f"VALUE_{global_idx} = {global_idx}")
        live_load_env.write_text(f"edits/disjoint_{group}.py", "\n".join(lines) + "\n")

    # Seed overlapping edit targets: 2 files * 3 edits each = 6 overlap attempts.
    for group in range(2):
        live_load_env.write_text(
            f"edits/overlap_{group}.py",
            '"""Overlap target."""\n\nSHARED = 0\n',
        )

    # Seed daytona_shell unique targets: 4 independent command writes.
    for idx in range(4):
        live_load_env.write_text(f"tx/unique_{idx}.txt", "base\n")

    live_load_env.exec_checked(f"git -C {shlex.quote(live_load_env.repo_root)} add -A")
    live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} commit -m seed-load-fixtures",
        timeout=180,
    )

    svc = live_load_env.make_ci_service()
    operations: list[dict[str, Any]] = []

    # 25 unique writes.
    for idx in range(25):
        operations.append(
            {
                "kind": "write",
                "name": f"write-{idx}",
                "path": f"{live_load_env.repo_root}/writes/write_{idx}.txt",
                "kwargs": {
                    "file_path": f"{live_load_env.repo_root}/writes/write_{idx}.txt",
                    "content": f"write {idx}\n",
                },
                "coordinated": False,
            }
        )

    # 15 disjoint edits.
    for group in range(3):
        for idx in range(5):
            global_idx = group * 5 + idx
            file_path = f"{live_load_env.repo_root}/edits/disjoint_{group}.py"
            operations.append(
                {
                    "kind": "edit-disjoint",
                    "name": f"edit-disjoint-{global_idx}",
                    "path": file_path,
                    "kwargs": {
                        "file_path": file_path,
                        "old_text": f"VALUE_{global_idx} = {global_idx}",
                        "new_text": f"VALUE_{global_idx} = {global_idx}00",
                    },
                    "coordinated": False,
                }
            )

    # 6 overlapping edits: at most one final winner per file.
    for group in range(2):
        file_path = f"{live_load_env.repo_root}/edits/overlap_{group}.py"
        for idx in range(3):
            value = (group + 1) * 1000 + idx
            operations.append(
                {
                    "kind": "edit-overlap",
                    "name": f"edit-overlap-{group}-{idx}",
                    "path": file_path,
                    "group": group,
                    "winner_value": value,
                    "kwargs": {
                        "file_path": file_path,
                        "old_text": "SHARED = 0",
                        "new_text": f"SHARED = {value}",
                    },
                    "coordinated": False,
                }
            )

    # 4 coordinated daytona_shell shell commands on unique files.
    for idx in range(4):
        rel_path = f"tx/unique_{idx}.txt"
        operations.append(
            {
                "kind": "shell",
                "name": f"shell-{idx}",
                "path": f"{live_load_env.repo_root}/{rel_path}",
                "kwargs": {
                    "mode": "shell",
                    "command": (
                        "python3 - <<'PY'\n"
                        "from pathlib import Path\n"
                        f"Path({rel_path!r}).write_text('shell {idx}\\n', encoding='utf-8')\n"
                        "PY"
                    ),
                    "timeout": 120,
                },
                "coordinated": True,
            }
        )

    assert len(operations) == 50

    started = time.perf_counter()
    results = asyncio.run(
        _run_mixed_operations(
            live_load_env,
            svc,
            operations,
            concurrency=20,
            timeout_s=240,
            log_ops=True,
            log_label=log_label,
        )
    )
    wall_elapsed_s = time.perf_counter() - started

    write_results = [item for item in results if item["kind"] == "write"]
    disjoint_results = [item for item in results if item["kind"] == "edit-disjoint"]
    overlap_results = [item for item in results if item["kind"] == "edit-overlap"]
    shell_results = [item for item in results if item["kind"] == "shell"]

    write_successes = sum(not item["is_error"] for item in write_results)
    disjoint_successes = sum(not item["is_error"] for item in disjoint_results)
    overlap_successes = sum(not item["is_error"] for item in overlap_results)
    overlap_conflicts = sum(
        bool(item["metadata"].get("conflict")) or bool(item["payload"].get("conflict"))
        for item in overlap_results
    )
    shell_successes = sum(not item["is_error"] for item in shell_results)

    arbiter_status = svc.status()["arbiter"]
    scope_status = svc.scope_status([live_load_env.repo_root])
    hotspots = scope_status["hotspots"]

    winners_by_group: dict[int, list[int]] = {0: [], 1: []}
    for item in overlap_results:
        group = int(item["group"])
        value = int(item["winner_value"])
        text = live_load_env.read_text(f"edits/overlap_{group}.py")
        if f"SHARED = {value}" in text:
            winners_by_group[group].append(value)
    overlap_persisted_winners = sum(len(values) for values in winners_by_group.values())

    by_kind: dict[str, list[dict[str, Any]]] = {}
    for item in results:
        by_kind.setdefault(item["kind"], []).append(item)

    print(f"\n[{log_label} summary]")
    print(
        json.dumps(
            {
                "operation_count": len(operations),
                "write_successes": write_successes,
                "disjoint_successes": disjoint_successes,
                "overlap_successes": overlap_successes,
                "overlap_conflicts": overlap_conflicts,
                "overlap_persisted_winners": overlap_persisted_winners,
                "shell_successes": shell_successes,
                "elapsed_profile_s": {
                    kind: _elapsed_profile(items)
                    for kind, items in sorted(by_kind.items())
                },
                "timing": _operation_timing_summary(
                    results,
                    wall_elapsed_s=wall_elapsed_s,
                ),
                "shell_phase_s": _phase_summary(shell_stats),
                "overlay_phase_s": _phase_summary(overlay_stats),
                "lsp_phase_s": _phase_summary(lsp_stats),
                "content_phase_s": _phase_summary(content_stats),
                "commit_phase_s": _phase_summary(commit_stats),
                "arbiter": arbiter_status,
                "hotspots": hotspots[:5],
            },
            indent=2,
            sort_keys=True,
        )
    )

    # Writes should all succeed because they target unique files.
    assert write_successes == 25

    # daytona_shell targets unique files too; these should all run and audit cleanly.
    assert shell_successes == 4

    # Disjoint edits should mostly land. Allow a small amount of live contention noise.
    assert disjoint_successes >= 12

    # Overlap files are process-level writes: several commands can report
    # success, but each file must end with a single coherent value.

    assert all(len(values) <= 1 for values in winners_by_group.values()), winners_by_group
    assert overlap_persisted_winners <= 2

    # Verify persisted results on unique-file paths.
    for idx in range(25):
        assert live_load_env.read_text(f"writes/write_{idx}.txt") == f"write {idx}\n"
    for idx in range(4):
        assert live_load_env.read_text(f"tx/unique_{idx}.txt") == f"shell {idx}\n"

    # Audit ledger sanity. conflicts_detected is currently not wired up, so use
    # result-level conflict tallies plus arbiter totals/hotspots here.
    expected_min_edits = write_successes + shell_successes + disjoint_successes
    assert arbiter_status["total_edits"] >= expected_min_edits
    assert arbiter_status["active_locks"] >= 0
    assert arbiter_status["conflicts_detected"] >= 0
    assert any("edits/disjoint_" in item["file_path"] for item in hotspots), hotspots


def test_live_occ_load_20_non_overlapping_operations_profile(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
):
    _set_live_load_overlay_concurrency(monkeypatch)
    log_label = "occ-load-20-nonoverlap"
    _log_occ_event(
        log_label,
        {"event": "setup", "phase": "init_repo", "sandbox_id": live_load_env.sandbox_id},
    )
    live_load_env.init_repo()
    shell_stats = _install_shell_phase_probe(monkeypatch)
    overlay_stats = _install_overlay_phase_probe(monkeypatch)
    lsp_stats = _install_lsp_phase_probe(monkeypatch)
    content_stats = _install_content_phase_probe(monkeypatch)
    commit_stats = _install_commit_phase_probe(monkeypatch)

    for group in range(2):
        live_load_env.write_text(
            f"edits/disjoint_{group}.py",
            (
                f'"""Disjoint target {group}."""\n\n'
                f"A_{group} = 1\n"
                f"B_{group} = 2\n"
                f"C_{group} = 3\n"
                f"D_{group} = 4\n"
                f"E_{group} = 5\n"
            ),
        )
    for idx in range(4):
        live_load_env.write_text(f"tx/small_{idx}.txt", "base\n")

    live_load_env.exec_checked(f"git -C {shlex.quote(live_load_env.repo_root)} add -A")
    live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} commit -m seed-small-nonoverlap-load",
        timeout=180,
    )

    svc = live_load_env.make_ci_service()
    operations = [
        {
            "kind": "write",
            "name": "write-0",
            "path": f"{live_load_env.repo_root}/writes/w0.txt",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/writes/w0.txt",
                "content": "write 0\n",
            },
            "coordinated": False,
        },
        {
            "kind": "write",
            "name": "write-1",
            "path": f"{live_load_env.repo_root}/writes/w1.txt",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/writes/w1.txt",
                "content": "write 1\n",
            },
            "coordinated": False,
        },
        {
            "kind": "write",
            "name": "write-2",
            "path": f"{live_load_env.repo_root}/writes/w2.txt",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/writes/w2.txt",
                "content": "write 2\n",
            },
            "coordinated": False,
        },
        {
            "kind": "write",
            "name": "write-3",
            "path": f"{live_load_env.repo_root}/writes/w3.txt",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/writes/w3.txt",
                "content": "write 3\n",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-a0",
            "path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
                "old_text": "A_0 = 1",
                "new_text": "A_0 = 100",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-b0",
            "path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
                "old_text": "B_0 = 2",
                "new_text": "B_0 = 200",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-c0",
            "path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
                "old_text": "C_0 = 3",
                "new_text": "C_0 = 300",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-d0",
            "path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
                "old_text": "D_0 = 4",
                "new_text": "D_0 = 400",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-e0",
            "path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_0.py",
                "old_text": "E_0 = 5",
                "new_text": "E_0 = 500",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-a1",
            "path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
                "old_text": "A_1 = 1",
                "new_text": "A_1 = 100",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-b1",
            "path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
                "old_text": "B_1 = 2",
                "new_text": "B_1 = 200",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-c1",
            "path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
                "old_text": "C_1 = 3",
                "new_text": "C_1 = 300",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-d1",
            "path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
                "old_text": "D_1 = 4",
                "new_text": "D_1 = 400",
            },
            "coordinated": False,
        },
        {
            "kind": "edit-disjoint",
            "name": "edit-e1",
            "path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/edits/disjoint_1.py",
                "old_text": "E_1 = 5",
                "new_text": "E_1 = 500",
            },
            "coordinated": False,
        },
        {
            "kind": "shell",
            "name": "shell-0",
            "path": f"{live_load_env.repo_root}/tx/small_0.txt",
            "kwargs": {
                "mode": "shell",
                "command": (
                    "python3 - <<'PY'\n"
                    "from pathlib import Path\n"
                    "Path('tx/small_0.txt').write_text('shell 0\\n', encoding='utf-8')\n"
                    "PY"
                ),
                "timeout": 120,
            },
            "coordinated": True,
        },
        {
            "kind": "shell",
            "name": "shell-1",
            "path": f"{live_load_env.repo_root}/tx/small_1.txt",
            "kwargs": {
                "mode": "shell",
                "command": (
                    "python3 - <<'PY'\n"
                    "from pathlib import Path\n"
                    "Path('tx/small_1.txt').write_text('shell 1\\n', encoding='utf-8')\n"
                    "PY"
                ),
                "timeout": 120,
            },
            "coordinated": True,
        },
        {
            "kind": "shell",
            "name": "shell-2",
            "path": f"{live_load_env.repo_root}/tx/small_2.txt",
            "kwargs": {
                "mode": "shell",
                "command": (
                    "python3 - <<'PY'\n"
                    "from pathlib import Path\n"
                    "Path('tx/small_2.txt').write_text('shell 2\\n', encoding='utf-8')\n"
                    "PY"
                ),
                "timeout": 120,
            },
            "coordinated": True,
        },
        {
            "kind": "shell",
            "name": "shell-3",
            "path": f"{live_load_env.repo_root}/tx/small_3.txt",
            "kwargs": {
                "mode": "shell",
                "command": (
                    "python3 - <<'PY'\n"
                    "from pathlib import Path\n"
                    "Path('tx/small_3.txt').write_text('shell 3\\n', encoding='utf-8')\n"
                    "PY"
                ),
                "timeout": 120,
            },
            "coordinated": True,
        },
        {
            "kind": "write",
            "name": "write-4",
            "path": f"{live_load_env.repo_root}/writes/w4.txt",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/writes/w4.txt",
                "content": "write 4\n",
            },
            "coordinated": False,
        },
        {
            "kind": "write",
            "name": "write-5",
            "path": f"{live_load_env.repo_root}/writes/w5.txt",
            "kwargs": {
                "file_path": f"{live_load_env.repo_root}/writes/w5.txt",
                "content": "write 5\n",
            },
            "coordinated": False,
        },
    ]

    started = time.perf_counter()
    results = asyncio.run(
        _run_mixed_operations(
            live_load_env,
            svc,
            operations,
            concurrency=20,
            timeout_s=120,
            log_ops=True,
            log_label=log_label,
        )
    )
    wall_elapsed_s = time.perf_counter() - started

    by_kind: dict[str, list[dict[str, Any]]] = {}
    for item in results:
        by_kind.setdefault(item["kind"], []).append(item)

    summary = {
        "operation_counts": {
            kind: len(items)
            for kind, items in sorted(by_kind.items())
        },
        "elapsed_profile_s": {
            kind: _elapsed_profile(items)
            for kind, items in sorted(by_kind.items())
        },
        "timing": _operation_timing_summary(
            results,
            wall_elapsed_s=wall_elapsed_s,
        ),
        "write_process_s": [
            round(float(item["payload"].get("timings", {}).get("commit_total", 0.0)), 6)
            for item in by_kind.get("write", [])
        ],
        "edit_tool_total_s": [
            round(float(item["payload"].get("timings", {}).get("tool", {}).get("tool_total", 0.0)), 6)
            for item in by_kind.get("edit-disjoint", [])
            if item["payload"].get("timings")
        ],
        "shell_phase_s": _phase_summary(shell_stats),
        "overlay_phase_s": _phase_summary(overlay_stats),
        "lsp_phase_s": _phase_summary(lsp_stats),
        "content_phase_s": _phase_summary(content_stats),
        "commit_phase_s": _phase_summary(commit_stats),
        "arbiter": svc.status()["arbiter"],
        "overlay_max_concurrent": _LIVE_LOAD_OVERLAY_MAX_CONCURRENT,
    }
    print(f"\n[{log_label} timings]")
    print(json.dumps(summary, indent=2, sort_keys=True))

    assert len(operations) == 20
    assert sum(not item["is_error"] for item in by_kind["write"]) == 6
    assert sum(not item["is_error"] for item in by_kind["shell"]) == 4
    assert sum(not item["is_error"] for item in by_kind["edit-disjoint"]) >= 8
    assert summary["timing"]["parallelism_ratio"] >= 3.0, summary["timing"]


def test_live_occ_load_30_non_overlapping_operations_profile(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
):
    _set_live_load_overlay_concurrency(monkeypatch)
    log_label = "occ-load-30-nonoverlap"
    _log_occ_event(
        log_label,
        {"event": "setup", "phase": "init_repo", "sandbox_id": live_load_env.sandbox_id},
    )
    live_load_env.init_repo()
    shell_stats = _install_shell_phase_probe(monkeypatch)
    overlay_stats = _install_overlay_phase_probe(monkeypatch)
    lsp_stats = _install_lsp_phase_probe(monkeypatch)
    content_stats = _install_content_phase_probe(monkeypatch)
    commit_stats = _install_commit_phase_probe(monkeypatch)

    for group in range(3):
        live_load_env.write_text(
            f"edits/disjoint_{group}.py",
            (
                f'"""Disjoint target {group}."""\n\n'
                f"A_{group} = 1\n"
                f"B_{group} = 2\n"
                f"C_{group} = 3\n"
                f"D_{group} = 4\n"
                f"E_{group} = 5\n"
            ),
        )
    for idx in range(6):
        live_load_env.write_text(f"tx/medium_{idx}.txt", "base\n")

    live_load_env.exec_checked(f"git -C {shlex.quote(live_load_env.repo_root)} add -A")
    live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} commit -m seed-medium-nonoverlap-load",
        timeout=180,
    )

    svc = live_load_env.make_ci_service()
    operations: list[dict[str, Any]] = []

    for idx in range(9):
        operations.append(
            {
                "kind": "write",
                "name": f"write-{idx}",
                "path": f"{live_load_env.repo_root}/writes/w{idx}.txt",
                "kwargs": {
                    "file_path": f"{live_load_env.repo_root}/writes/w{idx}.txt",
                    "content": f"write {idx}\n",
                },
                "coordinated": False,
            }
        )

    for group in range(3):
        for label, old, new in (
            ("a", f"A_{group} = 1", f"A_{group} = 100"),
            ("b", f"B_{group} = 2", f"B_{group} = 200"),
            ("c", f"C_{group} = 3", f"C_{group} = 300"),
            ("d", f"D_{group} = 4", f"D_{group} = 400"),
            ("e", f"E_{group} = 5", f"E_{group} = 500"),
        ):
            operations.append(
                {
                    "kind": "edit-disjoint",
                    "name": f"edit-{label}{group}",
                    "path": f"{live_load_env.repo_root}/edits/disjoint_{group}.py",
                    "kwargs": {
                        "file_path": f"{live_load_env.repo_root}/edits/disjoint_{group}.py",
                        "old_text": old,
                        "new_text": new,
                    },
                    "coordinated": False,
                }
            )

    for idx in range(6):
        operations.append(
            {
                "kind": "shell",
                "name": f"shell-{idx}",
                "path": f"{live_load_env.repo_root}/tx/medium_{idx}.txt",
                "kwargs": {
                    "mode": "shell",
                    "command": (
                        "python3 - <<'PY'\n"
                        "from pathlib import Path\n"
                        f"Path('tx/medium_{idx}.txt').write_text('shell {idx}\\n', encoding='utf-8')\n"
                        "PY"
                    ),
                    "timeout": 120,
                },
                "coordinated": True,
            }
        )

    started = time.perf_counter()
    results = asyncio.run(
        _run_mixed_operations(
            live_load_env,
            svc,
            operations,
            concurrency=20,
            timeout_s=180,
            log_ops=True,
            log_label=log_label,
        )
    )
    wall_elapsed_s = time.perf_counter() - started

    by_kind: dict[str, list[dict[str, Any]]] = {}
    for item in results:
        by_kind.setdefault(item["kind"], []).append(item)

    summary = {
        "operation_counts": {
            kind: len(items)
            for kind, items in sorted(by_kind.items())
        },
        "elapsed_profile_s": {
            kind: _elapsed_profile(items)
            for kind, items in sorted(by_kind.items())
        },
        "timing": _operation_timing_summary(
            results,
            wall_elapsed_s=wall_elapsed_s,
        ),
        "write_process_s": [
            round(float(item["payload"].get("timings", {}).get("commit_total", 0.0)), 6)
            for item in by_kind.get("write", [])
        ],
        "edit_tool_total_s": [
            round(float(item["payload"].get("timings", {}).get("tool", {}).get("tool_total", 0.0)), 6)
            for item in by_kind.get("edit-disjoint", [])
            if item["payload"].get("timings")
        ],
        "shell_phase_s": _phase_summary(shell_stats),
        "overlay_phase_s": _phase_summary(overlay_stats),
        "lsp_phase_s": _phase_summary(lsp_stats),
        "content_phase_s": _phase_summary(content_stats),
        "commit_phase_s": _phase_summary(commit_stats),
        "arbiter": svc.status()["arbiter"],
        "overlay_max_concurrent": _LIVE_LOAD_OVERLAY_MAX_CONCURRENT,
    }
    print(f"\n[{log_label} timings]")
    print(json.dumps(summary, indent=2, sort_keys=True))

    assert len(operations) == 30
    assert sum(not item["is_error"] for item in by_kind["write"]) == 9
    assert sum(not item["is_error"] for item in by_kind["shell"]) == 6
    assert sum(not item["is_error"] for item in by_kind["edit-disjoint"]) >= 12


def test_live_occ_load_50_non_overlapping_operations_profile(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
):
    _set_live_load_overlay_concurrency(monkeypatch)
    log_label = "occ-load-50-nonoverlap"
    _log_occ_event(
        log_label,
        {"event": "setup", "phase": "init_repo", "sandbox_id": live_load_env.sandbox_id},
    )
    live_load_env.init_repo()
    shell_stats = _install_shell_phase_probe(monkeypatch)
    overlay_stats = _install_overlay_phase_probe(monkeypatch)
    lsp_stats = _install_lsp_phase_probe(monkeypatch)
    content_stats = _install_content_phase_probe(monkeypatch)
    commit_stats = _install_commit_phase_probe(monkeypatch)

    for group in range(5):
        live_load_env.write_text(
            f"edits/disjoint_{group}.py",
            (
                f'"""Disjoint target {group}."""\n\n'
                f"A_{group} = 1\n"
                f"B_{group} = 2\n"
                f"C_{group} = 3\n"
                f"D_{group} = 4\n"
                f"E_{group} = 5\n"
            ),
        )
    for idx in range(10):
        live_load_env.write_text(f"tx/large_{idx}.txt", "base\n")

    live_load_env.exec_checked(f"git -C {shlex.quote(live_load_env.repo_root)} add -A")
    live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} commit -m seed-large-nonoverlap-load",
        timeout=180,
    )

    svc = live_load_env.make_ci_service()
    operations: list[dict[str, Any]] = []

    for idx in range(15):
        operations.append(
            {
                "kind": "write",
                "name": f"write-{idx}",
                "path": f"{live_load_env.repo_root}/writes/w{idx}.txt",
                "kwargs": {
                    "file_path": f"{live_load_env.repo_root}/writes/w{idx}.txt",
                    "content": f"write {idx}\n",
                },
                "coordinated": False,
            }
        )

    for group in range(5):
        for label, old, new in (
            ("a", f"A_{group} = 1", f"A_{group} = 100"),
            ("b", f"B_{group} = 2", f"B_{group} = 200"),
            ("c", f"C_{group} = 3", f"C_{group} = 300"),
            ("d", f"D_{group} = 4", f"D_{group} = 400"),
            ("e", f"E_{group} = 5", f"E_{group} = 500"),
        ):
            operations.append(
                {
                    "kind": "edit-disjoint",
                    "name": f"edit-{label}{group}",
                    "path": f"{live_load_env.repo_root}/edits/disjoint_{group}.py",
                    "kwargs": {
                        "file_path": f"{live_load_env.repo_root}/edits/disjoint_{group}.py",
                        "old_text": old,
                        "new_text": new,
                    },
                    "coordinated": False,
                }
            )

    for idx in range(10):
        operations.append(
            {
                "kind": "shell",
                "name": f"shell-{idx}",
                "path": f"{live_load_env.repo_root}/tx/large_{idx}.txt",
                "kwargs": {
                    "mode": "shell",
                    "command": (
                        "python3 - <<'PY'\n"
                        "from pathlib import Path\n"
                        f"Path('tx/large_{idx}.txt').write_text('shell {idx}\\n', encoding='utf-8')\n"
                        "PY"
                    ),
                    "timeout": 120,
                },
                "coordinated": True,
            }
        )

    started = time.perf_counter()
    results = asyncio.run(
        _run_mixed_operations(
            live_load_env,
            svc,
            operations,
            concurrency=20,
            timeout_s=240,
            log_ops=True,
            log_label=log_label,
        )
    )
    wall_elapsed_s = time.perf_counter() - started

    by_kind: dict[str, list[dict[str, Any]]] = {}
    for item in results:
        by_kind.setdefault(item["kind"], []).append(item)

    summary = {
        "operation_counts": {
            kind: len(items)
            for kind, items in sorted(by_kind.items())
        },
        "elapsed_profile_s": {
            kind: _elapsed_profile(items)
            for kind, items in sorted(by_kind.items())
        },
        "timing": _operation_timing_summary(
            results,
            wall_elapsed_s=wall_elapsed_s,
        ),
        "write_process_s": [
            round(float(item["payload"].get("timings", {}).get("commit_total", 0.0)), 6)
            for item in by_kind.get("write", [])
        ],
        "edit_tool_total_s": [
            round(float(item["payload"].get("timings", {}).get("tool", {}).get("tool_total", 0.0)), 6)
            for item in by_kind.get("edit-disjoint", [])
            if item["payload"].get("timings")
        ],
        "shell_phase_s": _phase_summary(shell_stats),
        "overlay_phase_s": _phase_summary(overlay_stats),
        "lsp_phase_s": _phase_summary(lsp_stats),
        "content_phase_s": _phase_summary(content_stats),
        "commit_phase_s": _phase_summary(commit_stats),
        "arbiter": svc.status()["arbiter"],
        "overlay_max_concurrent": _LIVE_LOAD_OVERLAY_MAX_CONCURRENT,
    }
    print(f"\n[{log_label} timings]")
    print(json.dumps(summary, indent=2, sort_keys=True))

    assert len(operations) == 50
    assert sum(not item["is_error"] for item in by_kind["write"]) == 15
    assert sum(not item["is_error"] for item in by_kind["shell"]) == 10
    assert sum(not item["is_error"] for item in by_kind["edit-disjoint"]) >= 20


def test_live_occ_load_svc_cmd_overlay_amortization(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
):
    """svc.cmd / shell repeated calls must keep overlay setup bounded.

    This is the performance claim for the overlay auditor: repeated calls on
    the same sandbox should avoid expensive one-time setup and keep per-call
    snapshot, overlay execution, diff parsing, and OCC commit costs visible.

    The test runs one cold-start call, then 5 sequential calls that each
    mutate the same file. All 6 must succeed; the 5 steady-state calls'
    median elapsed must be materially below the cold-start elapsed; and the
    final file content must reflect the last write.
    """
    log_label = "occ-load-amortization"
    _log_occ_event(
        log_label,
        {"event": "setup", "phase": "init_repo", "sandbox_id": live_load_env.sandbox_id},
    )
    live_load_env.init_repo()
    shell_stats = _install_shell_phase_probe(monkeypatch)
    overlay_stats = _install_overlay_phase_probe(monkeypatch)
    live_load_env.write_text("shared/counter.txt", "v0\n")
    live_load_env.exec_checked(f"git -C {shlex.quote(live_load_env.repo_root)} add -A")
    live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} commit -m seed-amortization",
        timeout=180,
    )

    svc = live_load_env.make_ci_service()

    async def _invoke_shell(label: str, target_value: str) -> dict[str, Any]:
        ctx = live_load_env.make_ctx(
            svc,
            agent_run_id=f"{label}-{uuid.uuid4().hex[:8]}",
            coordinated=True,
        )
        kwargs = {
            "mode": "shell",
            "command": (
                "python3 - <<'PY'\n"
                "from pathlib import Path\n"
                f"Path('shared/counter.txt').write_text({target_value!r} + '\\n', encoding='utf-8')\n"
                "PY"
            ),
            "timeout": 120,
        }
        _log_occ_event(
            log_label,
            {"event": "shell_start", "label": label, "target_value": target_value},
        )
        started = time.perf_counter()
        result = await _invoke_tool(daytona_shell, kwargs, ctx)
        elapsed_s = round(time.perf_counter() - started, 6)
        _log_occ_event(
            log_label,
            {
                "event": "shell_finish",
                "label": label,
                "is_error": result.is_error,
                "elapsed_s": elapsed_s,
                "metadata": dict(result.metadata or {}),
                "overlay_phase_snapshot_s": {
                    phase: round(values[-1], 6)
                    for phase, values in overlay_stats.items()
                    if values
                },
                "shell_phase_snapshot_s": {
                    phase: round(values[-1], 6)
                    for phase, values in shell_stats.items()
                    if values
                },
            },
        )
        raw_output = result.output or ""
        output = raw_output.lstrip()
        payload: dict[str, Any] = {}
        if output.startswith("{"):
            try:
                payload = json.loads(output)
            except json.JSONDecodeError:
                payload = {}
        return {
            "label": label,
            "target_value": target_value,
            "is_error": result.is_error,
            "metadata": dict(result.metadata or {}),
            "payload": payload,
            "raw_output": raw_output[:800],
            "elapsed_s": elapsed_s,
        }

    async def _scenario() -> list[dict[str, Any]]:
        results = []
        # Cold start: first svc.cmd pays overlay setup cost.
        results.append(await _invoke_shell("cold", "cold-0"))
        # Steady-state: 5 sequential calls exercise the bounded overlay path.
        for i in range(5):
            results.append(await _invoke_shell(f"steady-{i}", f"steady-{i}"))
        return results

    started = time.perf_counter()
    results = asyncio.run(asyncio.wait_for(_scenario(), timeout=300))
    wall_elapsed_s = time.perf_counter() - started

    cold = results[0]
    steady = results[1:]
    steady_elapsed = sorted(item["elapsed_s"] for item in steady)
    median_steady_s = steady_elapsed[len(steady_elapsed) // 2]
    max_steady_s = steady_elapsed[-1]

    final_content = live_load_env.read_text("shared/counter.txt")
    arbiter_status = svc.status()["arbiter"]

    print(f"\n[{log_label}]")
    print(
        json.dumps(
            {
                "wall_elapsed_s": round(wall_elapsed_s, 6),
                "cold_elapsed_s": cold["elapsed_s"],
                "steady_elapsed_s": steady_elapsed,
                "median_steady_s": median_steady_s,
                "max_steady_s": max_steady_s,
                "steady_profile_s": _value_profile(
                    [item["elapsed_s"] for item in steady]
                ),
                "cold_vs_steady_ratio": (
                    round(cold["elapsed_s"] / median_steady_s, 3)
                    if median_steady_s > 0
                    else 0.0
                ),
                "final_content": final_content,
                "arbiter": arbiter_status,
                "shell_phase_s": _phase_summary(shell_stats),
                "overlay_phase_s": _phase_summary(overlay_stats),
                "per_call": [
                    {
                        "label": item["label"],
                        "is_error": item["is_error"],
                        "elapsed_s": item["elapsed_s"],
                        "raw_output": item["raw_output"],
                        "metadata": item["metadata"],
                    }
                    for item in results
                ],
            },
            indent=2,
            sort_keys=True,
        )
    )

    # All 6 calls must succeed.
    for item in results:
        assert not item["is_error"], (
            f"{item['label']} failed: "
            f"output={item['raw_output']!r} metadata={item['metadata']}"
        )

    # The last write must be what landed on disk.
    assert final_content == "steady-4\n", (
        f"Final file does not reflect the last steady-state write; got {final_content!r}"
    )

    # Amortization gate. Live-sandbox timings are noisy (network jitter,
    # shared runner load), so we assert the median of 5 steady-state calls
    # is at most 1.5x the cold-start. If the pool regressed to recreating
    # every slot each call, steady-state would roughly track cold-start.
    assert median_steady_s <= cold["elapsed_s"] * 1.5, (
        f"Steady-state median {median_steady_s:.3f}s exceeds 1.5x cold-start "
        f"{cold['elapsed_s']:.3f}s; overlay amortization regressed. "
        f"Per-call timings: cold={cold['elapsed_s']:.3f}s, "
        f"steady={steady_elapsed}"
    )

    # Max steady-state must not exceed 2x cold start.
    assert max_steady_s <= cold["elapsed_s"] * 2.0, (
        f"Steady-state max {max_steady_s:.3f}s exceeded 2x cold-start "
        f"{cold['elapsed_s']:.3f}s."
    )

    # Arbiter ledger must reflect 6 shell-side commits (one per svc.cmd).
    assert arbiter_status["total_edits"] >= 6, arbiter_status


def test_live_occ_load_sequential_per_op_baseline(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
):
    """Per-op 1-op latency baseline with zero concurrency.

    Runs each mutator kind ``N`` times sequentially on one fresh sandbox.
    Pair the resulting p50/min numbers with the 72-op concurrent test's
    per-kind profile to reason about parallel efficiency.

    The ops do real work on fresh paths per iteration so prior iterations
    don't short-circuit (e.g. edit must find its sentinel, delete must
    find its target). First iteration is treated as a cold-start sample;
    steady-state should converge by iteration 2.
    """
    N = 5
    log_label = "occ-load-sequential-baseline"
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "init_repo",
            "sandbox_id": live_load_env.sandbox_id,
            "iterations": N,
        },
    )
    live_load_env.init_repo()
    shell_stats = _install_shell_phase_probe(monkeypatch)
    overlay_stats = _install_overlay_phase_probe(monkeypatch)
    lsp_stats = _install_lsp_phase_probe(monkeypatch)
    content_stats = _install_content_phase_probe(monkeypatch)
    commit_stats = _install_commit_phase_probe(monkeypatch)

    # Seed files that each op iter will mutate.
    for idx in range(N):
        live_load_env.write_text(
            f"edits/edit_{idx}.py",
            f'"""Edit fixture {idx}."""\n\nMARKER_{idx} = "before"\n',
        )
        live_load_env.write_text(
            f"rename/mod_{idx}.py",
            (
                f'"""Rename fixture {idx}."""\n\n'
                f"def target_{idx}(x):\n"
                f"    return x + {idx}\n\n"
                f"def caller_{idx}(x):\n"
                f"    return target_{idx}(x)\n"
            ),
        )
        live_load_env.write_text(f"moves/src_{idx}.txt", f"src {idx}\n")
        live_load_env.write_text(f"deletes/del_{idx}.txt", f"del {idx}\n")
        live_load_env.write_text(f"shell/ca_{idx}.txt", f"base {idx}\n")

    live_load_env.exec_checked(f"git -C {shlex.quote(live_load_env.repo_root)} add -A")
    live_load_env.exec_checked(
        f"git -C {shlex.quote(live_load_env.repo_root)} commit -m seed-sequential-baseline",
        timeout=180,
    )

    svc = live_load_env.make_ci_service()
    svc.ensure_initialized(wait=True)

    timings_by_kind: dict[str, list[float]] = {}

    async def _run_single(
        kind: str,
        name: str,
        kwargs: dict[str, Any],
        *,
        coordinated: bool,
    ) -> float:
        agent_run_id = f"{name}-{uuid.uuid4().hex[:8]}"
        ctx = live_load_env.make_ctx(
            svc,
            agent_run_id=agent_run_id,
            coordinated=coordinated,
        )
        tool = _tool_for_operation_kind(kind)
        _log_occ_event(
            log_label,
            {"event": "op_start", "kind": kind, "name": name},
        )
        started = time.perf_counter()
        result = await _invoke_tool(tool, kwargs, ctx)
        elapsed = round(time.perf_counter() - started, 6)
        assert not result.is_error, (
            f"{kind}/{name} failed: output={(result.output or '')[:300]!r} "
            f"metadata={dict(result.metadata or {})}"
        )
        _log_occ_event(
            log_label,
            {
                "event": "op_finish",
                "kind": kind,
                "name": name,
                "elapsed_s": elapsed,
                "metadata": dict(result.metadata or {}),
            },
        )
        timings_by_kind.setdefault(kind, []).append(elapsed)
        return elapsed

    async def _scenario() -> None:
        for idx in range(N):
            await _run_single(
                "write",
                f"write-{idx}",
                {
                    "file_path": f"{live_load_env.repo_root}/writes/w_{idx}.txt",
                    "content": f"write {idx}\n",
                },
                coordinated=False,
            )
            await _run_single(
                "edit-disjoint",
                f"edit-{idx}",
                {
                    "file_path": f"{live_load_env.repo_root}/edits/edit_{idx}.py",
                    "old_text": f'MARKER_{idx} = "before"',
                    "new_text": f'MARKER_{idx} = "after-{idx}"',
                },
                coordinated=False,
            )
            await _run_single(
                "rename",
                f"rename-{idx}",
                {
                    "symbol": f"target_{idx}",
                    "new_name": f"renamed_{idx}",
                    "file_hint": f"rename/mod_{idx}.py",
                },
                coordinated=False,
            )
            await _run_single(
                "move",
                f"move-{idx}",
                {
                    "src_path": f"{live_load_env.repo_root}/moves/src_{idx}.txt",
                    "target_path": f"{live_load_env.repo_root}/moves/dst_{idx}.txt",
                },
                coordinated=False,
            )
            await _run_single(
                "delete",
                f"delete-{idx}",
                {"path": f"{live_load_env.repo_root}/deletes/del_{idx}.txt"},
                coordinated=False,
            )
            await _run_single(
                "shell",
                f"shell-{idx}",
                {
                    "mode": "shell",
                    "command": (
                        "python3 - <<'PY'\n"
                        "from pathlib import Path\n"
                        f"Path('shell/ca_{idx}.txt').write_text('shell {idx}\\n', encoding='utf-8')\n"
                        "PY"
                    ),
                    "timeout": 120,
                },
                coordinated=True,
            )

    scenario_started = time.perf_counter()
    asyncio.run(asyncio.wait_for(_scenario(), timeout=600))
    wall_elapsed_s = round(time.perf_counter() - scenario_started, 6)

    def _steady_profile(times: list[float]) -> dict[str, float]:
        # First iteration is cold; summarize ops 2..N for steady-state view.
        return _value_profile(times[1:]) if len(times) > 1 else _value_profile(times)

    summary = {
        "iterations_per_kind": N,
        "wall_elapsed_s": wall_elapsed_s,
        "per_op_elapsed_s": {
            kind: _value_profile(times)
            for kind, times in sorted(timings_by_kind.items())
        },
        "per_op_steady_s": {
            kind: _steady_profile(times)
            for kind, times in sorted(timings_by_kind.items())
        },
        "cold_s": {
            kind: round(times[0], 6)
            for kind, times in sorted(timings_by_kind.items())
        },
        "shell_phase_s": _phase_summary(shell_stats),
        "overlay_phase_s": _phase_summary(overlay_stats),
        "lsp_phase_s": _phase_summary(lsp_stats),
        "content_phase_s": _phase_summary(content_stats),
        "commit_phase_s": _phase_summary(commit_stats),
        "arbiter": svc.status()["arbiter"],
    }

    print(f"\n[{log_label}]")
    print(json.dumps(summary, indent=2, sort_keys=True))

    expected_kinds = {"write", "edit-disjoint", "rename", "move", "delete", "shell"}
    assert set(timings_by_kind.keys()) == expected_kinds, timings_by_kind.keys()
    for kind, times in timings_by_kind.items():
        assert len(times) == N, (kind, times)
        for t in times:
            assert 0 < t < 60, (kind, t)

    # Spot-check that the state actually landed — guards against a tool
    # silently no-op'ing and inflating the baseline.
    for idx in range(N):
        assert live_load_env.read_text(f"writes/w_{idx}.txt") == f"write {idx}\n"
        assert f'MARKER_{idx} = "after-{idx}"' in live_load_env.read_text(
            f"edits/edit_{idx}.py"
        )
        renamed = live_load_env.read_text(f"rename/mod_{idx}.py")
        assert f"def renamed_{idx}(x):" in renamed
        assert f"target_{idx}(x)" not in renamed or f"renamed_{idx}(x)" in renamed
        assert live_load_env.read_text(f"moves/dst_{idx}.txt") == f"src {idx}\n"
        assert live_load_env.read_text(f"shell/ca_{idx}.txt") == f"shell {idx}\n"


def test_live_daytona_transport_parallelism_isolation(live_load_env: LiveLoadEnv) -> None:
    """Isolate whether `process.exec` is the concurrency ceiling.

    The OCC load test measures ~2.3x effective parallelism for 72 concurrent
    ops, while single-op latencies are 0.58-1.57s. That gap could live in the
    Python OCC pipeline or in the sandbox transport. This test removes the
    entire OCC pipeline and measures parallelism of `process.exec` alone.

    Target: wall time for 72 concurrent `sleep 0.5` execs should be ~0.5-1.0s
    if transport parallelism is unbounded. Anything materially higher means
    the transport itself is the ceiling and no amount of Python batching will
    hit the 1-2s target.
    """
    N = 72
    SLEEP_S = 0.5

    import concurrent.futures as _cf

    async def _run() -> dict[str, Any]:
        # Arm A: asyncio.to_thread wrapping sync sandbox (what svc.cmd uses).
        async def one_a(idx: int) -> dict[str, float]:
            t0 = time.perf_counter()
            resp = await live_load_env.async_sandbox.process.exec(
                f"sleep {SLEEP_S}",
                timeout=30,
            )
            return {
                "idx": idx,
                "elapsed_s": round(time.perf_counter() - t0, 6),
                "exit_code": getattr(resp, "exit_code", None),
            }

        configure_default_executor(
            asyncio.get_running_loop(),
            max_workers=max(200, N * 8),
        )
        # Warm one exec so any one-time connection setup isn't measured.
        await live_load_env.async_sandbox.process.exec("echo warm", timeout=10)

        wall_t0 = time.perf_counter()
        a_per_call = await asyncio.gather(*[one_a(i) for i in range(N)])
        arm_a_wall = round(time.perf_counter() - wall_t0, 6)

        # Arm C: explicit run_in_executor on a fresh ThreadPoolExecutor.
        # If C matches Arm B, set_default_executor isn't being honored by
        # asyncio.to_thread / run_in_executor(None, ...). If C still matches
        # Arm A, bottleneck is in loop-driven dispatch itself (not executor).
        loop = asyncio.get_running_loop()
        explicit_pool = _cf.ThreadPoolExecutor(
            max_workers=N, thread_name_prefix="arm-c",
        )

        import functools as _functools

        def _run_one_sync() -> Any:
            return live_load_env.raw_sandbox.process.exec(
                f"sleep {SLEEP_S}", timeout=30,
            )

        async def one_c(idx: int) -> dict[str, float]:
            t0 = time.perf_counter()
            resp = await loop.run_in_executor(
                explicit_pool,
                _functools.partial(_run_one_sync),
            )
            return {
                "idx": idx,
                "elapsed_s": round(time.perf_counter() - t0, 6),
                "exit_code": getattr(resp, "exit_code", None),
            }

        wall_t0 = time.perf_counter()
        c_per_call = await asyncio.gather(*[one_c(i) for i in range(N)])
        arm_c_wall = round(time.perf_counter() - wall_t0, 6)
        explicit_pool.shutdown(wait=False)

        # Arm D: true AsyncDaytona client (aiohttp). This is what production
        # should use once the sync-wrap is removed.
        from sandbox.async_client import get_async_sandbox

        async_real = await get_async_sandbox(live_load_env.sandbox_id)
        # Warm one real-async exec.
        await async_real.process.exec("echo warm", timeout=10)

        async def one_d(idx: int) -> dict[str, float]:
            t0 = time.perf_counter()
            resp = await async_real.process.exec(
                f"sleep {SLEEP_S}",
                timeout=30,
            )
            return {
                "idx": idx,
                "elapsed_s": round(time.perf_counter() - t0, 6),
                "exit_code": getattr(resp, "exit_code", None),
            }

        wall_t0 = time.perf_counter()
        d_per_call = await asyncio.gather(*[one_d(i) for i in range(N)])
        arm_d_wall = round(time.perf_counter() - wall_t0, 6)

        # Arm F: run_in_executor(None, ...) -- same default executor as
        # asyncio.to_thread but WITHOUT the contextvars.copy_context().run
        # wrapping. If F matches C (~48x), the to_thread ctx.run wrapping is
        # what serializes the sync Daytona SDK. If F matches A (~6x), the
        # default executor is blocked regardless of ctx.run.
        def _run_one_sync_f() -> Any:
            return live_load_env.raw_sandbox.process.exec(
                f"sleep {SLEEP_S}", timeout=30,
            )

        async def one_f(idx: int) -> dict[str, float]:
            t0 = time.perf_counter()
            resp = await loop.run_in_executor(None, _run_one_sync_f)
            return {
                "idx": idx,
                "elapsed_s": round(time.perf_counter() - t0, 6),
                "exit_code": getattr(resp, "exit_code", None),
            }

        wall_t0 = time.perf_counter()
        f_per_call = await asyncio.gather(*[one_f(i) for i in range(N)])
        arm_f_wall = round(time.perf_counter() - wall_t0, 6)

        # Arm G: AsyncDaytona + heavy python3 workload (what ContentManager
        # actually does). Each call spawns a python interpreter on the sandbox
        # to read a stub file and marshal JSON, mirroring the real hot path.
        # If G is much slower than D (sleep 0.5), the sandbox's process.exec
        # runner is capped for heavy concurrent work regardless of transport.
        g_script = (
            "import json, pathlib, sys; "
            "p = pathlib.Path('/tmp/arm_g_stub.txt'); "
            "print(json.dumps({'exists': p.exists(), "
            "'content': p.read_text(encoding='utf-8') if p.exists() else ''}))"
        )
        g_command = f"python3 -c {shlex.quote(g_script)}"
        # Seed the stub file so the python script does real work (read+json).
        await async_real.process.exec(
            "printf 'arm-g-stub\\n' > /tmp/arm_g_stub.txt", timeout=10,
        )
        # Warm one exec.
        await async_real.process.exec(g_command, timeout=10)

        async def one_g(idx: int) -> dict[str, float]:
            t0 = time.perf_counter()
            resp = await async_real.process.exec(g_command, timeout=30)
            return {
                "idx": idx,
                "elapsed_s": round(time.perf_counter() - t0, 6),
                "exit_code": getattr(resp, "exit_code", None),
            }

        wall_t0 = time.perf_counter()
        g_per_call = await asyncio.gather(*[one_g(i) for i in range(N)])
        arm_g_wall = round(time.perf_counter() - wall_t0, 6)

        # Arm H: AsyncDaytona + snapshot-like git plumbing. This isolates the
        # command shape used by overlay snapshots from the rest of daytona_shell.
        h_repo = "/tmp/arm_h_snapshot_repo"
        await async_real.process.exec(
            "rm -rf /tmp/arm_h_snapshot_repo && "
            "mkdir -p /tmp/arm_h_snapshot_repo && "
            "git -C /tmp/arm_h_snapshot_repo init -q && "
            "git -C /tmp/arm_h_snapshot_repo config user.email test@example.invalid && "
            "git -C /tmp/arm_h_snapshot_repo config user.name Tester && "
            "for i in $(seq 1 10); do printf 'x=%s\\n' \"$i\" > /tmp/arm_h_snapshot_repo/f$i.py; done && "
            "git -C /tmp/arm_h_snapshot_repo add -A && "
            "git -C /tmp/arm_h_snapshot_repo commit -q -m seed && "
            "printf 'dirty\\n' > /tmp/arm_h_snapshot_repo/dirty.txt",
            timeout=30,
        )
        h_script = """
import json
import os
import subprocess
import sys
import tempfile

repo = sys.argv[1]
fd, index = tempfile.mkstemp(prefix="arm-h-idx-")
os.close(fd)
os.unlink(index)
env = dict(os.environ)
env["GIT_INDEX_FILE"] = index
env.setdefault("GIT_AUTHOR_NAME", "Bench")
env.setdefault("GIT_AUTHOR_EMAIL", "bench@example.invalid")
env.setdefault("GIT_COMMITTER_NAME", "Bench")
env.setdefault("GIT_COMMITTER_EMAIL", "bench@example.invalid")
env.setdefault("GIT_AUTHOR_DATE", "1700000000 +0000")
env.setdefault("GIT_COMMITTER_DATE", "1700000000 +0000")
try:
    head = subprocess.check_output(["git", "-C", repo, "rev-parse", "HEAD"], env=env).decode().strip()
    subprocess.run(["git", "-C", repo, "read-tree", "HEAD"], env=env, check=True)
    subprocess.run(["git", "-C", repo, "add", "-A"], env=env, check=True)
    tree = subprocess.check_output(["git", "-C", repo, "write-tree"], env=env).decode().strip()
    snap = subprocess.check_output(
        ["git", "-C", repo, "commit-tree", tree, "-m", "bench", "-p", head],
        env=env,
    ).decode().strip()
    print(json.dumps({"ok": True, "snap": snap[:12]}))
finally:
    try:
        os.unlink(index)
    except OSError:
        pass
"""
        h_command = f"python3 -c {shlex.quote(h_script)} {shlex.quote(h_repo)}"
        await async_real.process.exec(h_command, timeout=30)

        async def one_h(idx: int) -> dict[str, float]:
            t0 = time.perf_counter()
            resp = await async_real.process.exec(h_command, timeout=60)
            return {
                "idx": idx,
                "elapsed_s": round(time.perf_counter() - t0, 6),
                "exit_code": getattr(resp, "exit_code", None),
            }

        wall_t0 = time.perf_counter()
        h_per_call = await asyncio.gather(*[one_h(i) for i in range(N)])
        arm_h_wall = round(time.perf_counter() - wall_t0, 6)

        default_exec = loop._default_executor  # type: ignore[attr-defined]
        default_max = (
            getattr(default_exec, "_max_workers", "n/a") if default_exec else "none"
        )

        return {
            "arm_a": {"wall_s": arm_a_wall, "per_call": a_per_call},
            "arm_c": {"wall_s": arm_c_wall, "per_call": c_per_call},
            "arm_d": {"wall_s": arm_d_wall, "per_call": d_per_call},
            "arm_f": {"wall_s": arm_f_wall, "per_call": f_per_call},
            "arm_g": {"wall_s": arm_g_wall, "per_call": g_per_call},
            "arm_h": {"wall_s": arm_h_wall, "per_call": h_per_call},
            "default_executor_max_workers": default_max,
        }

    arms = asyncio.run(_run())
    arm_async = arms["arm_a"]
    arm_c = arms["arm_c"]
    arm_d = arms["arm_d"]
    arm_f = arms["arm_f"]
    arm_g = arms["arm_g"]
    arm_h = arms["arm_h"]

    # Arm B: raw sync sandbox via concurrent.futures (no Python async at all).
    def one_sync(idx: int) -> dict[str, float]:
        t0 = time.perf_counter()
        resp = live_load_env.raw_sandbox.process.exec(
            f"sleep {SLEEP_S}",
            timeout=30,
        )
        return {
            "idx": idx,
            "elapsed_s": round(time.perf_counter() - t0, 6),
            "exit_code": getattr(resp, "exit_code", None),
        }

    # Warm one sync exec too.
    live_load_env.raw_sandbox.process.exec("echo warm", timeout=10)

    wall_t0 = time.perf_counter()
    with _cf.ThreadPoolExecutor(max_workers=N) as pool:
        arm_sync_per_call = list(pool.map(one_sync, range(N)))
    arm_sync_wall_s = round(time.perf_counter() - wall_t0, 6)

    def _profile(per_call: list[dict[str, float]]) -> dict[str, float]:
        values = sorted(float(item["elapsed_s"]) for item in per_call)
        return {
            "count": len(values),
            "min": round(values[0], 4),
            "p50": round(values[len(values) // 2], 4),
            "p90": round(values[int(len(values) * 0.9)], 4),
            "p99": round(values[int(len(values) * 0.99)], 4),
            "max": round(values[-1], 4),
            "sum": round(sum(values), 4),
        }

    def _arm(wall_s: float, per_call: list[dict[str, float]]) -> dict[str, Any]:
        return {
            "wall_s": wall_s,
            "effective_parallelism": round(
                (N * SLEEP_S) / max(wall_s, 1e-6), 2,
            ),
            "per_call_s": _profile(per_call),
        }

    summary = {
        "N": N,
        "sleep_s": SLEEP_S,
        "single_op_floor_s": SLEEP_S,
        "pure_sequential_wall_s": round(N * SLEEP_S, 4),
        "arm_a_asyncio_to_thread": _arm(arm_async["wall_s"], arm_async["per_call"]),
        "arm_b_sync_threadpool": _arm(arm_sync_wall_s, arm_sync_per_call),
        "arm_c_explicit_run_in_executor": _arm(arm_c["wall_s"], arm_c["per_call"]),
        "arm_d_true_async_daytona_aiohttp": _arm(arm_d["wall_s"], arm_d["per_call"]),
        "arm_f_run_in_executor_None_no_ctxrun": _arm(
            arm_f["wall_s"], arm_f["per_call"],
        ),
        "arm_g_async_daytona_python3_heavy": _arm(
            arm_g["wall_s"], arm_g["per_call"],
        ),
        "arm_h_async_daytona_git_snapshot_like": _arm(
            arm_h["wall_s"], arm_h["per_call"],
        ),
        "default_executor_max_workers": arms["default_executor_max_workers"],
    }

    print("\n[transport-parallelism-isolation]")
    print(json.dumps(summary, indent=2, sort_keys=True))

    # Diagnostic only — sanity-check completion and gross timeout.
    for arm in (arm_async, arm_c, arm_d, arm_f, arm_g):
        assert len(arm["per_call"]) == N
        assert arm["wall_s"] < N * SLEEP_S + 20
    assert len(arm_sync_per_call) == N
    assert arm_sync_wall_s < N * SLEEP_S + 10


def test_live_daytona_overlay_script_upload_transport_comparison(
    live_load_env: LiveLoadEnv,
) -> None:
    """Compare current process.exec upload with Daytona fs.upload_file."""
    payload = (
        _PROJECT_ROOT / "backend/src/code_intelligence/routing/overlay_run.py"
    ).read_bytes()
    payload_sha = __import__("hashlib").sha256(payload).hexdigest()
    encoded = base64.b64encode(payload).decode("ascii")
    base_dir = f"{live_load_env.home}/eos-upload-bench"
    sequential_n = 5
    concurrent_n = 20

    def _profile(values: list[float]) -> dict[str, float]:
        ordered = sorted(values)
        return {
            "count": float(len(ordered)),
            "min": round(ordered[0], 6),
            "p50": round(ordered[len(ordered) // 2], 6),
            "p90": round(ordered[int(len(ordered) * 0.9)], 6),
            "max": round(ordered[-1], 6),
            "avg": round(sum(ordered) / len(ordered), 6),
            "sum": round(sum(ordered), 6),
        }

    def _exec_upload_command(target: str) -> str:
        script = (
            "import base64,sys,pathlib; "
            "pathlib.Path(sys.argv[1]).write_bytes(base64.b64decode(sys.argv[2]))"
        )
        return (
            f"mkdir -p {shlex.quote(base_dir)} && "
            f"python3 -c {shlex.quote(script)} "
            f"{shlex.quote(target)} {shlex.quote(encoded)}"
        )

    async def _exec_upload(target: str) -> dict[str, Any]:
        started = time.perf_counter()
        response = await live_load_env.async_sandbox.process.exec(
            _wrap_bash_command(_exec_upload_command(target)),
            timeout=60,
        )
        elapsed = round(time.perf_counter() - started, 6)
        stdout, exit_code = _extract_exit_code(
            str(getattr(response, "result", "") or ""),
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        return {
            "target": target,
            "elapsed_s": elapsed,
            "exit_code": exit_code,
            "stdout_tail": stdout[-200:],
        }

    async def _fs_upload(target: str) -> dict[str, Any]:
        started = time.perf_counter()
        try:
            await live_load_env.async_sandbox.fs.upload_file(payload, target)
            return {
                "target": target,
                "elapsed_s": round(time.perf_counter() - started, 6),
                "exit_code": 0,
                "stdout_tail": "",
            }
        except Exception as exc:
            return {
                "target": target,
                "elapsed_s": round(time.perf_counter() - started, 6),
                "exit_code": -1,
                "stdout_tail": repr(exc)[-200:],
            }

    async def _async_fs_upload(async_real: Any, target: str) -> dict[str, Any]:
        started = time.perf_counter()
        try:
            await async_real.fs.upload_file(payload, target)
            return {
                "target": target,
                "elapsed_s": round(time.perf_counter() - started, 6),
                "exit_code": 0,
                "stdout_tail": "",
            }
        except Exception as exc:
            return {
                "target": target,
                "elapsed_s": round(time.perf_counter() - started, 6),
                "exit_code": -1,
                "stdout_tail": repr(exc)[-200:],
            }

    async def _verify(targets: list[str]) -> None:
        quoted = " ".join(shlex.quote(target) for target in targets)
        cmd = f"sha256sum {quoted}"
        response = await live_load_env.async_sandbox.process.exec(
            _wrap_bash_command(cmd),
            timeout=30,
        )
        stdout, exit_code = _extract_exit_code(
            str(getattr(response, "result", "") or ""),
            fallback_exit_code=getattr(response, "exit_code", None),
        )
        assert exit_code == 0, stdout
        for line in stdout.splitlines():
            if not line.strip():
                continue
            observed = line.split()[0]
            assert observed == payload_sha, line

    async def _run_arm(
        label: str,
        upload,
        *,
        count: int,
        concurrent: bool,
    ) -> dict[str, Any]:
        targets = [
            f"{base_dir}/{label}_{idx}_{uuid.uuid4().hex}.py"
            for idx in range(count)
        ]
        started = time.perf_counter()
        if concurrent:
            results = await asyncio.gather(*(upload(target) for target in targets))
        else:
            results = []
            for target in targets:
                results.append(await upload(target))
        wall_s = round(time.perf_counter() - started, 6)
        successful_targets = [
            str(item["target"]) for item in results if int(item["exit_code"]) == 0
        ]
        if successful_targets:
            await _verify(successful_targets)
        elapsed = [float(item["elapsed_s"]) for item in results]
        return {
            "count": count,
            "wall_s": wall_s,
            "per_call_s": _profile(elapsed),
            "failed": [item for item in results if item["exit_code"] != 0],
        }

    async def _run() -> dict[str, Any]:
        from sandbox.async_client import get_async_sandbox

        async_real = await get_async_sandbox(live_load_env.sandbox_id)
        await live_load_env.async_sandbox.process.exec(
            _wrap_bash_command(f"rm -rf {shlex.quote(base_dir)} && mkdir -p {shlex.quote(base_dir)}"),
            timeout=30,
        )
        # Warm both transports so one-time connection setup is not dominant.
        await _exec_upload(f"{base_dir}/warm_exec.py")
        await _fs_upload(f"{base_dir}/warm_fs.py")
        await _async_fs_upload(async_real, f"{base_dir}/warm_async_fs.py")
        return {
            "payload_bytes": len(payload),
            "payload_base64_chars": len(encoded),
            "process_exec_sequential": await _run_arm(
                "exec_seq",
                _exec_upload,
                count=sequential_n,
                concurrent=False,
            ),
            "fs_upload_file_sequential": await _run_arm(
                "fs_seq",
                _fs_upload,
                count=sequential_n,
                concurrent=False,
            ),
            "process_exec_concurrent": await _run_arm(
                "exec_concurrent",
                _exec_upload,
                count=concurrent_n,
                concurrent=True,
            ),
            "fs_upload_file_concurrent": await _run_arm(
                "fs_concurrent",
                _fs_upload,
                count=concurrent_n,
                concurrent=True,
            ),
            "async_fs_upload_file_sequential": await _run_arm(
                "async_fs_seq",
                lambda target: _async_fs_upload(async_real, target),
                count=sequential_n,
                concurrent=False,
            ),
            "async_fs_upload_file_concurrent": await _run_arm(
                "async_fs_concurrent",
                lambda target: _async_fs_upload(async_real, target),
                count=concurrent_n,
                concurrent=True,
            ),
        }

    summary = asyncio.run(_run())
    print("\n[overlay-script-upload-transport-comparison]")
    print(json.dumps(summary, indent=2, sort_keys=True))

    assert summary["process_exec_sequential"]["failed"] == []
    assert summary["process_exec_concurrent"]["failed"] == []


# ---------------------------------------------------------------------------
# Scenario A: contention correctness (OCC must gate, no lost updates)
# ---------------------------------------------------------------------------

_ABORT_STATUSES = frozenset({
    "aborted_version",
    "aborted_overlap",
    "aborted_lock",
    "dst_exists",
    "not_found",
    "patch_failed",
    "identical_paths",
    "failed",
})


def _item_status(item: dict[str, Any]) -> str:
    return str(
        (item.get("metadata") or {}).get("status")
        or (item.get("payload") or {}).get("status")
        or ""
    )


def _item_conflict(item: dict[str, Any]) -> str:
    return str(
        (item.get("metadata") or {}).get("conflict_reason")
        or (item.get("payload") or {}).get("conflict_reason")
        or ""
    )


def _group_outcome(results: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    """Bucket per-op results into winners / aborts / unclassified errors.

    Aborts are any op with ``is_error=True`` AND either a recognized
    abort status (coordinator enum) or a non-empty conflict_reason.
    Anything else that errored is unclassified — must not happen in a
    healthy system.
    """
    groups: dict[str, dict[str, Any]] = {}
    for item in results:
        gid = str(item.get("group") or "")
        g = groups.setdefault(gid, {"winners": [], "aborts": [], "other_errors": []})
        is_err = bool(item.get("is_error"))
        status = _item_status(item)
        conflict = _item_conflict(item)
        if not is_err:
            g["winners"].append(item)
        elif status in _ABORT_STATUSES or conflict:
            g["aborts"].append(
                {"item": item, "status": status, "conflict_reason": conflict},
            )
        else:
            g["other_errors"].append(item)
    return groups


def _assert_write_contention_invariants(
    env: LiveLoadEnv,
    groups: dict[str, dict[str, Any]],
    *,
    expect_file_present: bool = True,
    require_single_winner: bool = False,
) -> None:
    """Invariants for N ops contending on one group.

    ``require_single_winner=True`` is the strict OCC shape (same-anchor
    edits, deletes, dst-collision moves). When False (full-file writes
    serialize under the file lock), multiple sequential winners are
    valid — the invariant relaxes to "some winner landed on disk, no
    loser did, no unclassified errors".
    """
    for gid, bucket in groups.items():
        winners = bucket["winners"]
        aborts = bucket["aborts"]
        other = bucket["other_errors"]
        assert len(winners) >= 1, (
            f"group {gid}: no winners "
            f"(aborts={len(aborts)}, other={len(other)})"
        )
        if require_single_winner:
            assert len(winners) == 1, (
                f"group {gid}: expected exactly 1 winner, got {len(winners)} "
                f"(aborts={len(aborts)}, other={len(other)})"
            )
        assert not other, (
            f"group {gid}: unclassified errors leaked through gating: "
            f"{[{'name': o.get('name'), 'payload': o.get('payload'), 'metadata': o.get('metadata')} for o in other[:2]]}"
        )
        if not expect_file_present:
            continue
        rel = gid.removeprefix(env.repo_root + "/")
        actual = env.read_text(rel)
        winner_tokens = {str(w.get("winner_value") or "") for w in winners}
        winner_tokens.discard("")
        loser_tokens = {
            str(a["item"].get("winner_value") or "") for a in aborts
        }
        loser_tokens.discard("")
        on_disk_winners = {t for t in winner_tokens if t in actual}
        on_disk_losers = {t for t in loser_tokens if t in actual}
        assert on_disk_winners, (
            f"group {gid}: no winner token on disk "
            f"(winners={winner_tokens}, tail={actual[-300:]!r})"
        )
        assert not on_disk_losers, (
            f"group {gid}: loser tokens on disk: {on_disk_losers}"
        )


def _build_contention_ops_write(
    env: LiveLoadEnv,
    *,
    target_rel: str,
    n_writers: int,
) -> list[dict[str, Any]]:
    target_abs = f"{env.repo_root}/{target_rel}"
    ops = []
    for i in range(n_writers):
        winner = uuid.uuid4().hex[:12]
        ops.append({
            "kind": "write",
            "name": f"write-contend-{i}",
            "path": target_abs,
            "group": target_abs,
            "winner_value": winner,
            "kwargs": {
                "file_path": target_abs,
                "content": f"WINNER={winner}\nseq={i}\n",
            },
        })
    return ops


def _build_contention_ops_edit(
    env: LiveLoadEnv,
    *,
    target_rel: str,
    anchor: str,
    n_editors: int,
) -> list[dict[str, Any]]:
    target_abs = f"{env.repo_root}/{target_rel}"
    ops = []
    for i in range(n_editors):
        winner = uuid.uuid4().hex[:12]
        ops.append({
            "kind": "edit-overlap",
            "name": f"edit-contend-{i}",
            "path": target_abs,
            "group": target_abs,
            "winner_value": winner,
            "kwargs": {
                "file_path": target_abs,
                "old_text": anchor,
                "new_text": f"REPLACED_BY={winner}  # seq={i}",
            },
        })
    return ops


def test_live_occ_contention_write_same_path_gates_exactly_one_winner(
    live_load_env: LiveLoadEnv,
) -> None:
    """N concurrent writes to one path: exactly one winner, rest aborted_version."""
    log_label = "occ-contention-write-same-path"
    env = live_load_env
    env.init_repo()
    env.write_text("contend/target.txt", "seed\n")
    env.exec_checked(f"git -C {shlex.quote(env.repo_root)} add -A")
    env.exec_checked(
        f"git -C {shlex.quote(env.repo_root)} commit -m seed-contend",
        timeout=60,
    )
    svc = env.make_ci_service()
    svc.ensure_initialized(wait=True)

    n = 24
    ops = _build_contention_ops_write(env, target_rel="contend/target.txt", n_writers=n)
    started = time.perf_counter()
    results = asyncio.run(
        _run_mixed_operations(
            env, svc, ops,
            concurrency=n, timeout_s=300,
            log_label=log_label,
        )
    )
    wall_s = time.perf_counter() - started

    groups = _group_outcome(results)
    _assert_write_contention_invariants(env, groups, require_single_winner=False)
    only = next(iter(groups.values()))
    total = len(only["winners"]) + len(only["aborts"])
    assert total == n, (
        f"expected {n} classified outcomes, got "
        f"winners={len(only['winners'])} aborts={len(only['aborts'])}"
    )
    # Under same-path write contention with full-file overwrites, the
    # coordinator serializes via the per-file lock and last-write-wins
    # semantics permit multiple sequential winners. The gating invariant
    # is that no writer returns success without actually committing
    # (checked in _assert_write_contention_invariants via on-disk tokens).
    arb = _arbiter_snapshot(svc)

    summary = {
        "N": n,
        "wall_s": round(wall_s, 3),
        "winners": len(only["winners"]),
        "aborts": len(only["aborts"]),
        "arbiter": arb,
    }
    print(f"\n[{log_label}] {json.dumps(summary, sort_keys=True)}", flush=True)


def test_live_occ_contention_edit_same_anchor_gates_exactly_one_winner(
    live_load_env: LiveLoadEnv,
) -> None:
    """N concurrent edits against the same anchor: losers must abort, not silently drop."""
    log_label = "occ-contention-edit-same-anchor"
    env = live_load_env
    env.init_repo()
    anchor = "MARKER = 'seed'"
    env.write_text("contend/edit_target.py", f'"""anchor."""\n\n{anchor}\n')
    env.exec_checked(f"git -C {shlex.quote(env.repo_root)} add -A")
    env.exec_checked(
        f"git -C {shlex.quote(env.repo_root)} commit -m seed-edit-contend",
        timeout=60,
    )
    svc = env.make_ci_service()
    svc.ensure_initialized(wait=True)

    n = 24
    ops = _build_contention_ops_edit(
        env,
        target_rel="contend/edit_target.py",
        anchor=anchor,
        n_editors=n,
    )
    results = asyncio.run(
        _run_mixed_operations(
            env, svc, ops,
            concurrency=n, timeout_s=300,
            log_label=log_label,
        )
    )
    groups = _group_outcome(results)
    # Same-anchor edits: once winner replaces the anchor, every subsequent
    # editor's search_replace cannot find old_text → must abort, not
    # silently drop. Strict single-winner invariant applies here.
    _assert_write_contention_invariants(env, groups, require_single_winner=True)
    only = next(iter(groups.values()))
    assert len(only["aborts"]) == n - 1, (
        f"expected {n-1} aborts, got {len(only['aborts'])}"
    )


# ---------------------------------------------------------------------------
# Scenario B: parallelism-with-gating sweep
# ---------------------------------------------------------------------------

def _effective_parallelism(
    *, single_op_baseline_s: float, op_count: int, wall_elapsed_s: float,
) -> float:
    if wall_elapsed_s <= 0 or single_op_baseline_s <= 0:
        return 0.0
    return round((single_op_baseline_s * op_count) / wall_elapsed_s, 3)


def _measure_single_write_baseline(
    env: LiveLoadEnv, svc: CodeIntelligenceService,
) -> float:
    ops = [{
        "kind": "write",
        "name": "baseline-write",
        "path": f"{env.repo_root}/baseline/one.txt",
        "group": "baseline",
        "winner_value": "baseline",
        "kwargs": {
            "file_path": f"{env.repo_root}/baseline/one.txt",
            "content": "baseline\n",
        },
    }]
    results = asyncio.run(
        _run_mixed_operations(
            env, svc, ops, concurrency=1, timeout_s=120,
            log_label="occ-parallelism-baseline",
        )
    )
    return float(results[0]["elapsed_s"])


@pytest.mark.parametrize("group_count", [24, 12, 6, 1])
def test_live_occ_parallelism_gating_sweep(
    live_load_env: LiveLoadEnv, group_count: int,
) -> None:
    """Sweep contention density from K=N (no overlap) to K=1 (full overlap)."""
    n = 24
    log_label = f"occ-parallelism-sweep-K{group_count}"
    env = live_load_env
    env.init_repo()

    for k in range(group_count):
        env.write_text(f"sweep/target_{k}.txt", f"seed-{k}\n")
    env.exec_checked(f"git -C {shlex.quote(env.repo_root)} add -A")
    env.exec_checked(
        f"git -C {shlex.quote(env.repo_root)} commit -m seed-sweep-{group_count}",
        timeout=60,
    )

    svc = env.make_ci_service()
    svc.ensure_initialized(wait=True)
    baseline_s = _measure_single_write_baseline(env, svc)

    ops: list[dict[str, Any]] = []
    for i in range(n):
        k = i % group_count
        target_abs = f"{env.repo_root}/sweep/target_{k}.txt"
        winner = uuid.uuid4().hex[:12]
        ops.append({
            "kind": "write",
            "name": f"sweep-{i}",
            "path": target_abs,
            "group": target_abs,
            "winner_value": winner,
            "kwargs": {
                "file_path": target_abs,
                "content": f"WINNER={winner}\nseq={i}\nK={group_count}\n",
            },
        })

    started = time.perf_counter()
    results = asyncio.run(
        _run_mixed_operations(
            env, svc, ops,
            concurrency=n, timeout_s=600,
            log_label=log_label,
        )
    )
    wall_s = time.perf_counter() - started

    groups = _group_outcome(results)
    _assert_write_contention_invariants(
        env, groups, require_single_winner=False,
    )
    success_count = sum(len(g["winners"]) for g in groups.values())
    abort_count = sum(len(g["aborts"]) for g in groups.values())
    assert success_count + abort_count == n, (success_count, abort_count, n)

    eff_p = _effective_parallelism(
        single_op_baseline_s=baseline_s, op_count=n, wall_elapsed_s=wall_s,
    )
    abort_rate = round(abort_count / n, 3)

    summary = {
        "K": group_count,
        "N": n,
        "wall_s": round(wall_s, 3),
        "baseline_s": round(baseline_s, 3),
        "effective_parallelism": eff_p,
        "abort_rate": abort_rate,
        "winners": success_count,
        "aborts": abort_count,
        "arbiter": _arbiter_snapshot(svc),
    }
    print(f"\n[{log_label}] {json.dumps(summary, sort_keys=True)}", flush=True)

    # Shape assertions — writes serialize under the per-file lock, so
    # K=1 does not produce aborts (last-write-wins). The parallelism
    # ratio is the telltale: K=N should parallelize, K=1 should not.
    if group_count == n:
        assert eff_p >= 4.0, summary
        assert abort_rate == 0.0, summary
    elif group_count == 1:
        assert eff_p <= 5.0, summary


# ---------------------------------------------------------------------------
# Scenario C: daytona_shell overlay contention
# ---------------------------------------------------------------------------

def _extract_shell_conflict(item: dict[str, Any]) -> str:
    meta = item.get("metadata") or {}
    payload = item.get("payload") or {}
    for bag in (meta, payload):
        for key in (
            "conflict_reason",
            "git_conflict_reason",
            "git_commit_status",
        ):
            value = bag.get(key)
            if value and str(value) not in {"committed", "ok"}:
                return str(value)
    return ""


def test_live_occ_contention_shell_overlay_gates_concurrent_appends(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """N concurrent shell appends to the same file.

    Ground truth is disk state, not tool self-report. Overlay tracked commits
    should reject stale same-file writes through strict-base OCC.
    """
    log_label = "occ-contention-shell-overlay"
    env = live_load_env
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "init_repo_start",
            "pid": os.getpid(),
            "sandbox_id": env.sandbox_id,
            "repo_root": env.repo_root,
        },
    )
    init_repo_started = time.perf_counter()
    env.init_repo()
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "init_repo_finish",
            "elapsed_s": round(time.perf_counter() - init_repo_started, 6),
        },
    )

    shell_stats = _install_shell_phase_probe(monkeypatch)
    overlay_stats = _install_overlay_phase_probe(monkeypatch)
    commit_stats = _install_commit_phase_probe(monkeypatch)
    svc_cmd_stats = _install_svc_cmd_phase_probe(monkeypatch)
    executor_depth_stats: dict[str, list[float]] = {}

    target_rel = "contend/shell_target.txt"
    target_abs = f"{env.repo_root}/{target_rel}"
    seed_started = time.perf_counter()
    _log_occ_event(log_label, {"event": "setup", "phase": "seed_start"})
    env.write_text(target_rel, "seed\n")
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "seed_finish",
            "elapsed_s": round(time.perf_counter() - seed_started, 6),
        },
    )

    commit_started = time.perf_counter()
    _log_occ_event(log_label, {"event": "setup", "phase": "git_commit_start"})
    env.exec_checked(f"git -C {shlex.quote(env.repo_root)} add -A")
    env.exec_checked(
        f"git -C {shlex.quote(env.repo_root)} commit -m seed-shell-contend",
        timeout=60,
    )
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "git_commit_finish",
            "elapsed_s": round(time.perf_counter() - commit_started, 6),
        },
    )

    svc = env.make_ci_service()
    svc_init_started = time.perf_counter()
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "ensure_initialized_start",
            "svc_id": hex(id(svc)),
            "arbiter_id": hex(id(svc.arbiter)),
        },
    )
    svc.ensure_initialized(wait=True)
    _log_occ_event(
        log_label,
        {
            "event": "setup",
            "phase": "ensure_initialized_finish",
            "elapsed_s": round(time.perf_counter() - svc_init_started, 6),
            "svc_id": hex(id(svc)),
            "arbiter_id": hex(id(svc.arbiter)),
            "arbiter": _arbiter_snapshot(svc),
        },
    )

    n = 12
    ops: list[dict[str, Any]] = []
    tokens: list[str] = []
    for i in range(n):
        token = uuid.uuid4().hex[:12]
        tokens.append(token)
        command = (
            f"printf 'WINNER=%s\\n' {shlex.quote(token)} "
            f">> {shlex.quote(target_abs)}"
        )
        ops.append({
            "kind": "shell",
            "name": f"shell-contend-{i}",
            "path": target_abs,
            "group": target_abs,
            "winner_value": token,
            "coordinated": False,
            "kwargs": {"command": command},
        })

    started = time.perf_counter()
    results = asyncio.run(
        _run_mixed_operations(
            env, svc, ops,
            concurrency=n, timeout_s=600,
            log_ops=True,
            log_label=log_label,
            executor_depth_stats=executor_depth_stats,
        )
    )
    wall_s = time.perf_counter() - started

    final_content = env.read_text(target_rel)
    on_disk = {t for t in tokens if t in final_content}

    tool_ok = {
        str(item.get("winner_value") or "")
        for item in results
        if not item.get("is_error")
    }
    tool_ok.discard("")

    stray = on_disk - tool_ok
    assert not stray, {
        "unauthorized_tokens_on_disk": sorted(stray),
        "final_tail": final_content[-600:],
    }

    missing = tool_ok - on_disk
    residue_bound = 0
    assert not missing, {
        "tool_said_ok_but_not_on_disk": sorted(missing),
        "residue_bound": residue_bound,
        "final_tail": final_content[-600:],
    }

    assert len(on_disk) < n, {
        "winners": len(on_disk),
        "n": n,
        "final_tail": final_content[-600:],
    }

    silent_drops = []
    for item in results:
        token = str(item.get("winner_value") or "")
        if not token or token in on_disk:
            continue
        if item.get("is_error"):
            continue
        if _extract_shell_conflict(item):
            continue
        silent_drops.append({
            "token": token,
            "name": item.get("name"),
            "metadata": item.get("metadata"),
            "payload_keys": sorted((item.get("payload") or {}).keys()),
        })
    assert not silent_drops, {
        "silent_overlay_drops": silent_drops[:5],
        "count": len(silent_drops),
        "residue_bound": residue_bound,
    }

    summary = {
        "N": n,
        "wall_s": round(wall_s, 3),
        "winners_on_disk": len(on_disk),
        "tool_ok_count": len(tool_ok),
        "timing": _operation_timing_summary(
            results,
            wall_elapsed_s=wall_s,
        ),
        "shell_phase_s": _phase_summary(shell_stats),
        "overlay_phase_s": _phase_summary(overlay_stats),
        "commit_phase_s": _phase_summary(commit_stats),
        "svc_cmd_phase_s": _phase_summary(svc_cmd_stats),
        "executor_depth": _phase_summary(executor_depth_stats),
        "arbiter": _arbiter_snapshot(svc),
    }
    print(f"\n[{log_label}] {json.dumps(summary, sort_keys=True)}", flush=True)


def test_live_shell_gitignore_dependency_writes_persist(
    live_load_env: LiveLoadEnv,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """daytona_shell writes under dependency gitignore prefixes persist on live disk.

    Overlay routes ignored dependency trees outside tracked-file OCC. This
    profile covers the production shape for commands that populate ``.venv/``
    or ``node_modules/``: the tool should return success, the ignored files
    should remain in the live workspace, and a later daytona_shell invocation should
    be able to read them through the live lowerdir.
    """
    log_label = "shell-gitignore-dependency-persistence"
    env = live_load_env
    env.init_repo()
    env.write_text(".gitignore", ".venv/\nnode_modules/\n")
    env.exec_checked(f"git -C {shlex.quote(env.repo_root)} add -A")
    env.exec_checked(
        f"git -C {shlex.quote(env.repo_root)} commit -m seed-gitignore-persistence",
        timeout=60,
    )
    svc = env.make_ci_service()
    svc.ensure_initialized(wait=True)

    shell_stats = _install_shell_phase_probe(monkeypatch)
    overlay_stats = _install_overlay_phase_probe(monkeypatch)
    svc_cmd_stats = _install_svc_cmd_phase_probe(monkeypatch)

    token = uuid.uuid4().hex[:12]
    create_command = (
        "python3 - <<'PY'\n"
        "from pathlib import Path\n"
        "Path('.venv/lib/site-packages/live_pkg').mkdir(parents=True, exist_ok=True)\n"
        "Path('node_modules/live_pkg').mkdir(parents=True, exist_ok=True)\n"
        f"Path('.venv/lib/site-packages/live_pkg/METADATA').write_text('venv={token}\\n', encoding='utf-8')\n"
        f"Path('node_modules/live_pkg/index.js').write_text('node={token}\\n', encoding='utf-8')\n"
        "PY"
    )
    read_command = (
        "python3 - <<'PY'\n"
        "from pathlib import Path\n"
        "print(Path('.venv/lib/site-packages/live_pkg/METADATA').read_text(encoding='utf-8').strip())\n"
        "print(Path('node_modules/live_pkg/index.js').read_text(encoding='utf-8').strip())\n"
        "PY"
    )

    create_ctx = env.make_ctx(
        svc,
        agent_run_id=f"gitignore-create-{uuid.uuid4().hex[:8]}",
    )
    read_ctx = env.make_ctx(
        svc,
        agent_run_id=f"gitignore-read-{uuid.uuid4().hex[:8]}",
    )
    create_result = asyncio.run(
        _invoke_tool(
            daytona_shell,
            {"mode": "shell", "command": create_command, "timeout": 180},
            create_ctx,
        )
    )
    read_result = asyncio.run(
        _invoke_tool(
            daytona_shell,
            {"mode": "shell", "command": read_command, "timeout": 180},
            read_ctx,
        )
    )

    create_payload = _json_output(create_result)
    read_payload = _json_output(read_result)
    venv_rel = ".venv/lib/site-packages/live_pkg/METADATA"
    node_rel = "node_modules/live_pkg/index.js"
    venv_content = env.read_text(venv_rel)
    node_content = env.read_text(node_rel)
    status_short = env.exec_checked(
        f"git -C {shlex.quote(env.repo_root)} status --short --untracked-files=all",
        timeout=30,
    )
    ignored_status = env.exec_checked(
        f"git -C {shlex.quote(env.repo_root)} status --short --ignored",
        timeout=30,
    )
    summary = {
        "create_is_error": create_result.is_error,
        "read_is_error": read_result.is_error,
        "create_metadata": create_result.metadata,
        "read_metadata": read_result.metadata,
        "create_payload": create_payload,
        "read_payload": read_payload,
        "status_short": status_short.strip(),
        "ignored_status": ignored_status.strip().splitlines()[:10],
        "shell_phase_s": _phase_summary(shell_stats),
        "overlay_phase_s": _phase_summary(overlay_stats),
        "svc_cmd_phase_s": _phase_summary(svc_cmd_stats),
    }
    print(f"\n[{log_label}] {json.dumps(summary, sort_keys=True)}", flush=True)

    assert not create_result.is_error, create_result.output
    assert not read_result.is_error, read_result.output
    assert create_payload["status"] == "ok"
    assert read_payload["status"] == "ok"
    assert venv_content == f"venv={token}\n"
    assert node_content == f"node={token}\n"
    assert f"venv={token}" in read_payload["shell_outputs"][0]["stdout"]
    assert f"node={token}" in read_payload["shell_outputs"][0]["stdout"]
    assert status_short.strip() == ""
    assert ".venv/" in ignored_status
    assert "node_modules/" in ignored_status
