"""Phase 4c+f smoke — exercise ``run_pipeline`` against a fully-stubbed TaskCenter.

The mock-scenario suite under ``task_center_runner/tests/mock/`` is the
canonical end-to-end coverage for the shim → run_pipeline path, but it
requires PG. This smoke test stubs every dependency so the unit-test gate
catches contract drift in ``run_pipeline`` itself (e.g. the
``start_task_center_run(config=...)`` argument needing ``.cwd``, a
mistake the unit suite missed before Phase 4f).

What is verified:
- ``run_pipeline`` does not crash for a minimal ``RunConfig`` with a fake
  sandbox + ``runner_factory = lambda ctx: None`` + ``NoopLifecycle``.
- ``PipelineReport`` is returned and carries a non-None
  ``performance_report_task``; awaiting it returns the expected
  ``performance_report.json`` path.
- ``SandboxProvisioner.release`` is called exactly once.
- The default ``run_dir`` matches ``audit_dir/<run_label>/<utc>_<self_id>``.
"""

from __future__ import annotations

import re
import types
from pathlib import Path
from typing import Any

import pytest

from task_center_runner.audit.events import EventType
from task_center_runner.core.config import RunConfig
from task_center_runner.core.lifecycle import NoopLifecycle
from task_center_runner.core.sandbox import SandboxLease


class _StubProvisioner:
    """``SandboxProvisioner`` that hands back a fixed lease and counts releases."""

    def __init__(self) -> None:
        self.released: list[SandboxLease] = []

    async def provision(self, ctx: Any) -> SandboxLease:
        return SandboxLease(sandbox_id="stub-sandbox", metadata={})

    async def release(self, lease: SandboxLease) -> None:
        self.released.append(lease)


class _StubLauncher:
    def __init__(self) -> None:
        self._pending: tuple = ()

    async def wait_for_idle(self) -> None:
        return None


class _StubHandle:
    def __init__(self) -> None:
        self.task_center_run_id = "stub-tcrid"
        self.request_id = "stub-request"
        self.launcher = _StubLauncher()


class _StubStores:
    def __init__(self) -> None:
        self.task_store = self
        self.workflow_store = self
        self.iteration_store = self
        self.attempt_store = self
        self.workflow_store = self
        self.iteration_store = self
        self.attempt_store = self
        self.context_packet_store = self

    def get_run(self, *_args: Any, **_kwargs: Any) -> dict:
        return {"status": "done"}

    def list_tasks_for_run(self, *_args: Any, **_kwargs: Any) -> list[dict]:
        return [
            {"status": "done"},
            {"status": "done"},
            {"status": "failed"},
        ]

    def close(self) -> None:
        return None


@pytest.fixture
def stubbed_engine(monkeypatch: pytest.MonkeyPatch) -> None:
    """Patch ``start_task_center_run`` + the AuditRecorder to be no-ops."""
    from task_center_runner.core import engine as engine_module

    def _stub_start(**_kwargs: Any) -> _StubHandle:  # noqa: ANN001
        return _StubHandle()

    monkeypatch.setattr(engine_module, "start_task_center_run", _stub_start)

    class _StubRecorder:
        def __init__(self, *_args: Any, **_kwargs: Any) -> None:
            self._run_dir = _kwargs.get("run_dir") or (_args[0] if _args else Path("."))
            self.metrics = types.SimpleNamespace(
                snapshot=lambda: {"counters": {}},
                performance_snapshot=lambda: {"snapshot": "stub"},
            )

        def start(self) -> None:
            return None

        def bind_task_center_run_id(self, _tcrid: str) -> None:
            return None

        def dispose(self) -> None:
            return None

        def message_recorder_for_agent_run(self, _id: str) -> None:
            return None

        def message_recorder_for_task(self, _id: str) -> None:
            return None

    monkeypatch.setattr(engine_module, "AuditRecorder", _StubRecorder)

    async def _stub_write_perf_report(
        run_dir: Path, _snapshot: Any, **_kwargs: Any
    ) -> Path:
        return run_dir / "performance_report.json"

    monkeypatch.setattr(
        engine_module, "_write_perf_report_safe", _stub_write_perf_report
    )

    def _stub_create_stores() -> _StubStores:
        return _StubStores()

    monkeypatch.setattr(engine_module, "create_per_test_task_center_stores", _stub_create_stores)


@pytest.mark.asyncio
async def test_run_pipeline_smoke(stubbed_engine: None, tmp_path: Path) -> None:
    from task_center_runner.core.engine import run_pipeline

    provisioner = _StubProvisioner()
    config = RunConfig(
        entry_prompt="hello world",
        repo_dir="/tmp/repo",
        sandbox=provisioner,
        runner_factory=lambda ctx: None,
        lifecycle=NoopLifecycle(),
        audit_dir=tmp_path,
        run_label="smoke",
        instance_id="stub-instance",
    )

    report = await run_pipeline(config)

    assert report.status == "completed"
    assert report.task_center_run_id == "stub-tcrid"
    assert report.request_id == "stub-request"
    assert report.sandbox_id == "stub-sandbox"
    assert report.instance_id == "stub-instance"
    assert report.task_count == 3
    assert report.tasks_completed == 2
    assert report.tasks_failed == 1
    assert report.aborted_by_timeout is False

    # Default run_dir scheme: audit_dir/<run_label>/<utc>_<self_id>
    assert report.run_dir.parent.parent == tmp_path
    assert report.run_dir.parent.name == "smoke"
    assert re.match(r"^\d{8}T\d{6}Z_[0-9a-f]{12}$", report.run_dir.name)

    assert report.performance_report_task is not None
    perf_path = await report.performance_report_task
    assert perf_path == report.run_dir / "performance_report.json"

    assert len(provisioner.released) == 1


@pytest.mark.asyncio
async def test_run_pipeline_lifecycle_hooks_fire(
    stubbed_engine: None, tmp_path: Path
) -> None:
    """before_run / on_event / after_run / on_aborted all dispatch through lifecycle."""
    from task_center_runner.core.engine import run_pipeline

    class _RecordingLifecycle(NoopLifecycle):
        def __init__(self) -> None:
            self.before_run_called = 0
            self.after_run_called = 0
            self.events: list[EventType] = []

        async def before_run(self, ctx: Any) -> None:
            self.before_run_called += 1

        def on_event(self, event: Any) -> None:
            self.events.append(event.type)

        async def after_run(self, ctx: Any, report: Any) -> None:
            self.after_run_called += 1

    lifecycle = _RecordingLifecycle()
    config = RunConfig(
        entry_prompt="hello",
        repo_dir="/tmp/repo",
        sandbox=_StubProvisioner(),
        runner_factory=lambda ctx: None,
        lifecycle=lifecycle,
        audit_dir=tmp_path,
        run_label="smoke-hooks",
    )

    report = await run_pipeline(config)

    assert lifecycle.before_run_called == 1
    assert lifecycle.after_run_called == 1
    # At minimum, the engine publishes RUN_STARTED and RUN_COMPLETED.
    assert EventType.RUN_STARTED in lifecycle.events
    assert EventType.RUN_COMPLETED in lifecycle.events

    if report.performance_report_task is not None:
        await report.performance_report_task
