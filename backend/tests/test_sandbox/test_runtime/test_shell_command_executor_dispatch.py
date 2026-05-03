"""Verify that ``svc.cmd`` uses the overlay engine directly."""

from __future__ import annotations

from types import SimpleNamespace

import pytest

from sandbox.runtime.shell_command_executor import AuditedCommandExecutor
from sandbox.overlay.engine import LocalOverlayEngine
from sandbox.overlay.types import OverlayRunOutcome
from sandbox.runtime.service import (
    CodeIntelligenceService,
)
from sandbox.runtime.registry import (
    dispose_all_code_intelligence,
)


@pytest.fixture(autouse=True)
def _registry() -> None:
    dispose_all_code_intelligence()
    yield
    dispose_all_code_intelligence()


@pytest.mark.asyncio
async def test_executor_builds_overlay_engine_by_default(tmp_path) -> None:
    svc = CodeIntelligenceService(
        sandbox_id=f"dispatch-overlay-{tmp_path.name}",
        workspace_root=str(tmp_path),
    )
    executor: AuditedCommandExecutor = svc._command_executor  # type: ignore[attr-defined]

    overlay_engine = await executor._ensure_overlay_engine()

    assert isinstance(overlay_engine, LocalOverlayEngine)


@pytest.mark.asyncio
async def test_cmd_delegates_to_overlay_engine_with_stdin(tmp_path) -> None:
    sandbox = SimpleNamespace()
    svc = CodeIntelligenceService(
        sandbox_id=f"dispatch-cmd-{tmp_path.name}",
        workspace_root=str(tmp_path),
        sandbox=sandbox,
    )
    executor: AuditedCommandExecutor = svc._command_executor  # type: ignore[attr-defined]
    calls: list[dict[str, object]] = []

    class _FakeOverlayEngine:
        async def execute(self, command: str, **kwargs):
            calls.append(
                {
                    "sandbox": kwargs.get("sandbox"),
                    "command": command,
                    "stdin": kwargs.get("stdin"),
                }
            )
            return OverlayRunOutcome(
                exit_code=0,
                stdout="ok",
                upper_changes=(),
                overlay_rejected=False,
                conflict=None,
            )

    async def _fake_ensure_overlay_engine():
        return _FakeOverlayEngine()

    executor._ensure_overlay_engine = _fake_ensure_overlay_engine  # type: ignore[method-assign]

    result = await svc.cmd(sandbox, "cat", stdin="payload")

    assert result.result == "ok"
    assert calls == [{"sandbox": sandbox, "command": "cat", "stdin": "payload"}]


@pytest.mark.asyncio
async def test_executor_can_run_local_process_without_sandbox(tmp_path) -> None:
    svc = CodeIntelligenceService(
        sandbox_id=f"dispatch-local-{tmp_path.name}",
        workspace_root=str(tmp_path),
    )
    executor: AuditedCommandExecutor = svc._command_executor  # type: ignore[attr-defined]

    result = await executor._exec_sandbox_process(None, "printf local", timeout=5)

    assert result.result == "local"
    assert result.exit_code == 0
