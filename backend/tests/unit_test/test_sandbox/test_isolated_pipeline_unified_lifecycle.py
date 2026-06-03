"""Unit contracts for isolated workspace lifecycle and per-call routing."""

from __future__ import annotations

import asyncio
import json
import time
from pathlib import Path
from typing import Any

import pytest

from sandbox._shared.models import Intent, ToolCallRequest
from sandbox.isolated_workspace._control_plane.types import (
    IsolatedWorkspaceError,
    IsolatedWorkspaceHandle,
    _PipelineConfig,
)
from sandbox.isolated_workspace.pipeline import IsolatedPipeline


class _Snapshot:
    lease_id = "lease-1"
    manifest_version = 1
    root_hash = "root"
    layer_paths = ("/layers/L1",)


class _LayerStack:
    def __init__(self) -> None:
        self.released: list[str] = []

    def acquire_snapshot(self, *, request_id: str) -> _Snapshot:
        assert request_id.startswith("isolated-")
        return _Snapshot()

    def release_lease(self, *, lease_id: str) -> bool:
        self.released.append(lease_id)
        return True


class _Network:
    initialized = False

    def install_veth(self, *, workspace_handle_id: str, holder_pid: int) -> None:
        del workspace_handle_id, holder_pid
        return None

    def teardown_veth(self, _veth) -> None:
        return None


class _FakeNamespaceRuntime:
    def __init__(self) -> None:
        self.active_calls = 0
        self.max_active_calls = 0

    def spawn_ns_holder(self, handle: IsolatedWorkspaceHandle, *, setup_timeout_s: float) -> int:
        del handle, setup_timeout_s
        return 1234

    def open_ns_fds(self, holder_pid: int) -> dict[str, int]:
        assert holder_pid == 1234
        return {}

    async def mount_overlay(
        self,
        handle: IsolatedWorkspaceHandle,
        *,
        layer_paths: tuple[str, ...],
    ) -> None:
        del handle, layer_paths

    async def configure_dns(
        self,
        handle: IsolatedWorkspaceHandle,
        *,
        fallback_dns: str,
    ) -> bool:
        del handle, fallback_dns
        return True

    def signal_net_ready(
        self,
        handle: IsolatedWorkspaceHandle,
        *,
        setup_timeout_s: float,
    ) -> None:
        del handle, setup_timeout_s

    def create_cgroup(self, handle: IsolatedWorkspaceHandle) -> Path:
        path = handle.scratch_dir / "cgroup"
        path.mkdir(parents=True, exist_ok=True)
        return path

    def kill_holder(self, holder_pid: int, *, grace_s: float) -> None:
        del holder_pid, grace_s

    def run_in_handle(
        self,
        handle: IsolatedWorkspaceHandle,
        *,
        argv: list[str],
        stdin: bytes | None = None,
        timeout_s: float | None = None,
    ) -> tuple[int, bytes, bytes]:
        del handle, argv, stdin, timeout_s
        self.active_calls += 1
        self.max_active_calls = max(self.max_active_calls, self.active_calls)
        try:
            time.sleep(0.05)
            return 0, b'{"success":true}', b""
        finally:
            self.active_calls -= 1


def _config(**overrides: Any) -> _PipelineConfig:
    values = {
        "enabled": True,
        "ttl_s": 0.0,
        "total_cap": 5,
        "upperdir_bytes": 1024,
        "memavail_fraction": 0.5,
        "setup_timeout_s": 1.0,
        "exit_grace_s": 0.25,
        "rfc1918_egress": "allow",
        "fallback_dns": "1.1.1.1",
    }
    values.update(overrides)
    return _PipelineConfig(**values)


def _pipeline(
    tmp_path: Path,
    *,
    config: _PipelineConfig | None = None,
    runtime: _FakeNamespaceRuntime | None = None,
    meminfo_reader=None,
) -> IsolatedPipeline:
    return IsolatedPipeline(
        scratch_root=tmp_path,
        layer_stack=_LayerStack(),
        config=config or _config(),
        network=_Network(),
        runtime=runtime or _FakeNamespaceRuntime(),
        meminfo_reader=meminfo_reader,
    )


def test_pipeline_config_exit_grace_defaults_to_short_escalation_window() -> None:
    config = _PipelineConfig.from_env({"EOS_ISOLATED_WORKSPACE_ENABLED": "true"})
    assert config.exit_grace_s == pytest.approx(0.25)


def test_pipeline_config_exit_grace_env_override() -> None:
    config = _PipelineConfig.from_env(
        {
            "EOS_ISOLATED_WORKSPACE_ENABLED": "true",
            "EOS_ISOLATED_WORKSPACE_EXIT_GRACE_S": "1.5",
        }
    )
    assert config.exit_grace_s == pytest.approx(1.5)


@pytest.mark.asyncio
async def test_enter_exit_error_kinds(tmp_path: Path) -> None:
    pipeline = _pipeline(tmp_path)
    await pipeline.enter("agent-a")
    with pytest.raises(IsolatedWorkspaceError) as already_open:
        await pipeline.enter("agent-a")
    assert already_open.value.kind == "already_open"
    await pipeline.exit("agent-a")
    with pytest.raises(IsolatedWorkspaceError) as not_open:
        await pipeline.exit("agent-a")
    assert not_open.value.kind == "not_open"

    capped = _pipeline(tmp_path / "cap", config=_config(total_cap=1))
    await capped.enter("agent-a")
    with pytest.raises(IsolatedWorkspaceError) as quota:
        await capped.enter("agent-b")
    assert quota.value.kind == "quota_exceeded"
    await capped.exit("agent-a")

    pressure = _pipeline(
        tmp_path / "pressure",
        config=_config(upperdir_bytes=1024, memavail_fraction=0.5),
        meminfo_reader=lambda: 1,
    )
    with pytest.raises(IsolatedWorkspaceError) as host_ram:
        await pressure.enter("agent-a")
    assert host_ram.value.kind == "host_ram_pressure"


@pytest.mark.asyncio
async def test_test_reset_rewrites_invalid_manager_json(tmp_path: Path) -> None:
    pipeline = _pipeline(tmp_path)
    manager = pipeline.persisted_handles_path
    manager.parent.mkdir(parents=True)
    manager.write_text(
        json.dumps(
            {
                "schema_version": 999,
                "handles": [{"workspace_handle_id": "ghost"}],
            }
        ),
        encoding="utf-8",
    )

    await pipeline.test_reset()

    assert json.loads(manager.read_text(encoding="utf-8")) == {
        "schema_version": 1,
        "handles": [],
    }


@pytest.mark.asyncio
async def test_same_session_tool_calls_do_not_share_per_call_lock(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    runtime = _FakeNamespaceRuntime()
    pipeline = _pipeline(tmp_path, runtime=runtime)
    handle = IsolatedWorkspaceHandle(
        workspace_handle_id="h1",
        agent_id="agent-a",
        lease_id="lease-iws",
        manifest_version=1,
        manifest_root_hash="root",
        workspace_root="/testbed",
        scratch_dir=tmp_path / "scratch",
        upperdir=tmp_path / "scratch" / "upper",
        workdir=tmp_path / "scratch" / "work",
        holder_pid=1234,
    )
    handle.upperdir.mkdir(parents=True)
    handle.workdir.mkdir(parents=True)
    pipeline._handles[handle.workspace_handle_id] = handle
    pipeline._by_agent[handle.agent_id] = handle.workspace_handle_id

    async def fake_run_in_namespace(_handle, req, *, isolated_runner):
        response = await isolated_runner(["tool"], None, None)
        return {"success": response["success"], "status": "ok", "timings": {}}

    monkeypatch.setattr(
        "sandbox.isolated_workspace.pipeline.run_in_namespace",
        fake_run_in_namespace,
    )

    async def call(invocation_id: str) -> dict[str, Any]:
        return await pipeline.run_tool_call(
            ToolCallRequest(
                invocation_id=invocation_id,
                agent_id="agent-a",
                verb="read_file",
                intent=Intent.READ_ONLY,
                args={"path": "a.py"},
            )
        )

    first, second = await asyncio.gather(call("req-1"), call("req-2"))

    assert first["success"] is True
    assert second["success"] is True
    assert runtime.max_active_calls == 2


@pytest.mark.asyncio
async def test_shutdown_suppresses_ttl_task_cancellation(tmp_path: Path) -> None:
    pipeline = _pipeline(tmp_path)
    pipeline._ttl_task = asyncio.create_task(asyncio.sleep(60))

    await pipeline.shutdown()

    assert pipeline._ttl_task.cancelled()


def test_no_freeze_contract_left() -> None:
    root = Path(__file__).resolve().parents[3] / "src"
    production_files = list((root / "sandbox" / "isolated_workspace").rglob("*.py"))
    production_files += list((root / "tools" / "isolated_workspace").rglob("*.py"))
    text = "\n".join(path.read_text(encoding="utf-8") for path in production_files)

    assert "freezer_degraded" not in text
    assert "freeze(" not in text
    assert "unfreeze(" not in text
