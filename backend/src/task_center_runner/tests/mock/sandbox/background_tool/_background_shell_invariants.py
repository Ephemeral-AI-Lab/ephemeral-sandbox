"""Shared invariant helper for background-shell live tests."""

from __future__ import annotations

import asyncio
import json
import subprocess
import warnings
from pathlib import Path
from typing import Any

import sandbox.api as sandbox_api
from task_center_runner.benchmarks.sweevo.models import SWEEvoInstance, _REPO_DIR
from sandbox.shared.models import ReadFileRequest, SandboxCaller
from sandbox.daemon.paths import DAEMON_PID_PATH, DAEMON_SOCKET_PATH
from sandbox.host.daemon_client import call_daemon_api
from task_center_runner.agent.mock.background_shell_probe import (
    BACKGROUND_IWS_LAYER_STACK_ROOT,
)
from task_center_runner.core.runner import RunReport
from task_center_runner.core.stores import TaskCenterStoreBundle
from task_center_runner.environments.sweevo_image.fixtures import (
    run_scenario_on_sweevo_image,
)
from task_center_runner.scenarios import SCENARIO_REGISTRY
from task_center_runner.tests.mock._layer_stack_occ_overlay_assertions import (
    assert_o1_workspace_resource_snapshots,
    assert_resource_key_max,
    assert_timing_keys_present,
    load_performance_report,
    mapping,
)

_ERROR_NEEDLES = (
    "internal_error",
    "stale lowerdir",
    "mount_failed",
    "manifest references missing layer",
)
_DELETED_SHELL_RPC_NEEDLES = (
    "api.v1.shell.launch",
    "api.v1.shell.reap",
    "api.v1.shell.poll",
    "api.v1.shell.cancel",
    "ShellJob",
    "shell_job",
)
_OVERLAY_TIMING_KEYS = (
    "command_exec.mount_workspace_s",
    "command_exec.run_command_s",
    "command_exec.capture_upperdir_s",
    "api.shell.total_s",
)
_IWS_APT_CACHE_DIR = (
    Path(__file__).resolve().parents[6]
    / "tests"
    / "_assets"
    / "iws_apt_cache"
    / "jammy-amd64"
)
_IWS_TEST_UPPERDIR_BYTES = 67_108_864


async def run_background_shell_scenario(
    *,
    scenario_name: str,
    summary_path: str,
    sweevo_image_instance: SWEEvoInstance,
    workspace: dict[str, object],
    audit_dir: Path,
    stores: TaskCenterStoreBundle,
    preserve_inflight_ttl: bool = False,
) -> tuple[RunReport, dict[str, Any]]:
    scenario = SCENARIO_REGISTRY[scenario_name]()
    sandbox_id = str(workspace["sandbox_id"])
    if not preserve_inflight_ttl:
        await configure_default_inflight_ttl(sandbox_id)
    report = await run_scenario_on_sweevo_image(
        scenario,
        instance=sweevo_image_instance,
        sandbox_id=sandbox_id,
        audit_dir=audit_dir,
        stores=stores,
    )
    assert report.task_center_status == "done", report.metrics
    assert report.passed_prompt_inspections, [
        item for item in report.prompt_inspections if not item.passed
    ]
    assert report.passed_sandbox_checks, [
        item for item in report.sandbox_checks if not item.passed
    ]
    if report.performance_report_task is not None:
        await report.performance_report_task
    return report, await read_json_summary(sandbox_id, summary_path)


async def read_json_summary(sandbox_id: str, path: str) -> dict[str, Any]:
    read = await sandbox_api.read_file(
        sandbox_id,
        ReadFileRequest(
            path=path,
            caller=SandboxCaller(agent_id="test.background_shell.summary"),
        ),
    )
    assert read.success and read.exists, read
    summary = json.loads(read.content or "{}")
    assert isinstance(summary, dict), summary
    return summary


async def configure_short_inflight_ttl(sandbox_id: str) -> None:
    command = "\n".join(
        [
            "set -eu",
            "sed -i '/^EOS_INFLIGHT_TTL_S=/d; /^EOS_INFLIGHT_REAPER_INTERVAL_S=/d' /etc/environment 2>/dev/null || true",
            "printf '\\nEOS_INFLIGHT_TTL_S=1\\nEOS_INFLIGHT_REAPER_INTERVAL_S=0.2\\n' >> /etc/environment",
            f"if [ -f {DAEMON_PID_PATH} ]; then kill -TERM \"$(cat {DAEMON_PID_PATH})\" 2>/dev/null || true; fi",
            f"rm -f {DAEMON_SOCKET_PATH} {DAEMON_PID_PATH}",
        ]
    )
    result = await sandbox_api.raw_exec(sandbox_id, command, timeout=30)
    assert result.exit_code == 0, result
    readiness = await call_daemon_api(
        sandbox_id,
        "api.runtime.ready",
        {},
        timeout=30,
    )
    assert readiness.get("success") is True and readiness.get("ready") is True, readiness


async def configure_default_inflight_ttl(sandbox_id: str) -> None:
    command = "\n".join(
        [
            "set -eu",
            "sed -i '/^EOS_INFLIGHT_TTL_S=/d; /^EOS_INFLIGHT_REAPER_INTERVAL_S=/d' /etc/environment 2>/dev/null || true",
            f"if [ -f {DAEMON_PID_PATH} ]; then kill -TERM \"$(cat {DAEMON_PID_PATH})\" 2>/dev/null || true; fi",
            f"rm -f {DAEMON_SOCKET_PATH} {DAEMON_PID_PATH}",
        ]
    )
    result = await sandbox_api.raw_exec(sandbox_id, command, timeout=30)
    assert result.exit_code == 0, result
    readiness = await call_daemon_api(
        sandbox_id,
        "api.runtime.ready",
        {},
        timeout=30,
    )
    assert readiness.get("success") is True and readiness.get("ready") is True, readiness


async def configure_isolated_workspace_for_background(sandbox_id: str) -> None:
    """Prepare the live daemon for the background/IWS interaction probe."""
    installed_via_cache = await _try_install_iws_deps_from_cache(sandbox_id)
    if not installed_via_cache:
        try:
            await sandbox_api.raw_exec(
                sandbox_id,
                (
                    "command -v ip >/dev/null 2>&1 && "
                    "command -v nft >/dev/null 2>&1 "
                    "|| (apt-get update -qq && "
                    "DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "
                    "iproute2 nftables) >/dev/null 2>&1 || true"
                ),
                cwd="/",
                timeout=300,
            )
        except (TimeoutError, asyncio.TimeoutError):
            warnings.warn(
                "background IWS setup: iproute2+nftables install timed out; "
                "the live probe may fail while opening the isolated workspace.",
                stacklevel=2,
            )

    command = "\n".join(
        [
            "set -eu",
            "grep -q '^EOS_ISOLATED_WORKSPACE_ENABLED=' /etc/environment 2>/dev/null || echo 'EOS_ISOLATED_WORKSPACE_ENABLED=true' >> /etc/environment",
            "grep -q '^EOS_ISOLATED_WORKSPACE_TEST_HARNESS=' /etc/environment 2>/dev/null || echo 'EOS_ISOLATED_WORKSPACE_TEST_HARNESS=true' >> /etc/environment",
            "sed -i '/^EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES=/d' /etc/environment 2>/dev/null || true",
            f"echo 'EOS_ISOLATED_WORKSPACE_UPPERDIR_BYTES={_IWS_TEST_UPPERDIR_BYTES}' >> /etc/environment",
            "mount -o remount,rw /sys/fs/cgroup 2>/dev/null || true",
            "pkill -f '^.*python.*-m sandbox\\.daemon' || true",
        ]
    )
    result = await sandbox_api.raw_exec(sandbox_id, command, cwd="/", timeout=30)
    assert result.exit_code == 0, result
    await call_daemon_api(
        sandbox_id,
        "api.ensure_workspace_base",
        {"workspace_root": _REPO_DIR},
        layer_stack_root=BACKGROUND_IWS_LAYER_STACK_ROOT,
        timeout=180,
    )


async def _try_install_iws_deps_from_cache(sandbox_id: str) -> bool:
    if not _IWS_APT_CACHE_DIR.is_dir():
        return False
    if not any(_IWS_APT_CACHE_DIR.glob("*.deb")):
        return False
    staged = await sandbox_api.raw_exec(
        sandbox_id,
        "rm -rf /tmp/iws-debs && mkdir -p /tmp/iws-debs",
        cwd="/",
        timeout=10,
    )
    if staged.exit_code != 0:
        return False
    cp_cmd = [
        "docker",
        "cp",
        f"{_IWS_APT_CACHE_DIR}/.",
        f"{sandbox_id}:/tmp/iws-debs/",
    ]
    try:
        copied = await asyncio.to_thread(
            subprocess.run,
            cp_cmd,
            check=False,
            capture_output=True,
            timeout=60,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError):
        return False
    if copied.returncode != 0:
        return False
    installed = await sandbox_api.raw_exec(
        sandbox_id,
        (
            "command -v ip >/dev/null 2>&1 && "
            "command -v nft >/dev/null 2>&1 && "
            "command -v ping >/dev/null 2>&1 && "
            "command -v host >/dev/null 2>&1 "
            "|| DEBIAN_FRONTEND=noninteractive "
            "dpkg -i /tmp/iws-debs/*.deb 2>&1 | tail -3"
        ),
        cwd="/",
        timeout=60,
    )
    if installed.exit_code != 0:
        return False
    verified = await sandbox_api.raw_exec(
        sandbox_id,
        "command -v ip >/dev/null 2>&1 && command -v nft >/dev/null 2>&1",
        cwd="/",
        timeout=10,
    )
    return verified.exit_code == 0


def assert_background_performance_artifacts(report: RunReport) -> dict[str, Any]:
    events_path = report.run_dir / "sandbox_events.jsonl"
    assert_shell_audit_invariants(events_path)
    assert_o1_workspace_resource_snapshots(events_path)
    perf = dict(load_performance_report(report.run_dir))
    assert_timing_keys_present(perf, _OVERLAY_TIMING_KEYS)
    assert_resource_key_max(perf, "resource.command_exec.workspace_tree_bytes", 0.0)
    assert_resource_key_max(perf, "resource.command_exec.workspace_tree_exists", 0.0)
    resources = mapping(mapping(perf["sandbox"])["resource_keys"])
    for key in (
        "resource.command_exec.run_dir_tree_truncated",
        "resource.command_exec.upperdir_tree_truncated",
        "resource.command_exec.workspace_tree_truncated",
    ):
        assert float(mapping(resources[key])["max"]) == 0.0
    return perf


def tool_p95_ms(perf: dict[str, Any], tool_name: str) -> float:
    per_tool = mapping(mapping(mapping(perf["tools"])["per_tool"]))
    if tool_name not in per_tool:
        return 0.0
    return float(mapping(per_tool[tool_name]).get("p95_ms") or 0.0)


def _read_rows(jsonl_path: Path) -> list[dict[str, object]]:
    if not jsonl_path.exists():
        return []
    rows: list[dict[str, object]] = []
    raw = jsonl_path.read_text(encoding="utf-8", errors="replace")
    for line in raw.splitlines():
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            # Truncated JSON at the engine-kill cut point is expected in T4.
            continue
    return rows


def assert_shell_audit_invariants(
    jsonl_path: Path,
    *,
    expect_truncated: bool = False,
) -> None:
    """Assert background-shell runs did not emit known sandbox failure text."""
    del expect_truncated
    _read_rows(jsonl_path)
    if jsonl_path.exists():
        raw_text = jsonl_path.read_text(encoding="utf-8", errors="replace")
        for needle in _ERROR_NEEDLES:
            assert needle not in raw_text, (
                f"AC-11 violation: '{needle}' appears in {jsonl_path}"
            )
        for needle in _DELETED_SHELL_RPC_NEEDLES:
            assert needle not in raw_text, (
                f"deleted shell RPC surface '{needle}' appears in {jsonl_path}"
            )


__all__ = [
    "assert_background_performance_artifacts",
    "configure_default_inflight_ttl",
    "configure_isolated_workspace_for_background",
    "assert_shell_audit_invariants",
    "configure_short_inflight_ttl",
    "read_json_summary",
    "run_background_shell_scenario",
    "tool_p95_ms",
]
