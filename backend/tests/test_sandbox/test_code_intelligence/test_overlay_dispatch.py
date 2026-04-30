"""Verify that ``svc.cmd`` uses the overlay auditor directly."""

from __future__ import annotations

from types import SimpleNamespace

import pytest

from sandbox.code_intelligence.overlay.command_executor import AuditedCommandExecutor
from sandbox.code_intelligence.overlay.auditor import OverlayAuditor
from sandbox.code_intelligence.service import (
    CodeIntelligenceService,
    dispose_all_code_intelligence,
)


@pytest.fixture(autouse=True)
def _registry() -> None:
    dispose_all_code_intelligence()
    yield
    dispose_all_code_intelligence()


@pytest.mark.asyncio
async def test_executor_builds_overlay_auditor_by_default(tmp_path) -> None:
    svc = CodeIntelligenceService(
        sandbox_id=f"dispatch-overlay-{tmp_path.name}",
        workspace_root=str(tmp_path),
    )
    executor: AuditedCommandExecutor = svc._command_executor  # type: ignore[attr-defined]

    auditor = await executor._ensure_overlay_auditor()

    assert isinstance(auditor, OverlayAuditor)


@pytest.mark.asyncio
async def test_cmd_delegates_to_overlay_auditor_with_stdin(tmp_path) -> None:
    sandbox = SimpleNamespace()
    svc = CodeIntelligenceService(
        sandbox_id=f"dispatch-cmd-{tmp_path.name}",
        workspace_root=str(tmp_path),
        sandbox=sandbox,
    )
    executor: AuditedCommandExecutor = svc._command_executor  # type: ignore[attr-defined]
    calls: list[dict[str, object]] = []

    class _FakeOverlayAuditor:
        async def execute(self, sandbox_arg, command: str, **kwargs):
            calls.append(
                {
                    "sandbox": sandbox_arg,
                    "command": command,
                    "stdin": kwargs.get("stdin"),
                }
            )
            return SimpleNamespace(result="ok", exit_code=0)

    async def _fake_ensure_overlay_auditor():
        return _FakeOverlayAuditor()

    executor._ensure_overlay_auditor = _fake_ensure_overlay_auditor  # type: ignore[method-assign]

    result = await svc.cmd(sandbox, "cat", stdin="payload")

    assert result.result == "ok"
    assert calls == [{"sandbox": sandbox, "command": "cat", "stdin": "payload"}]
