"""Tests for ``CodeIntelligenceService.cmd`` fail-closed overlay semantics."""

from __future__ import annotations

import asyncio
import subprocess
from types import SimpleNamespace

import pytest

from sandbox.overlay.types import OverlayRunError
from sandbox.runtime.service import (
    CodeIntelligenceService,
)
from sandbox.runtime.registry import (
    dispose_all_code_intelligence,
)


@pytest.fixture(autouse=True)
def _clear_registry() -> None:
    dispose_all_code_intelligence()
    yield
    dispose_all_code_intelligence()


@pytest.mark.asyncio
async def test_cmd_raises_when_overlay_runtime_fails(tmp_path) -> None:
    sandbox = SimpleNamespace(process=_AsyncLocalProcess())
    svc = CodeIntelligenceService(
        sandbox_id=f"sandbox-cmd-overlay-fail-{tmp_path.name}",
        workspace_root=str(tmp_path),
        sandbox=sandbox,
    )

    with pytest.raises(OverlayRunError) as excinfo:
        await svc.cmd(sandbox, "echo hi")

    assert "overlay diff.ndjson missing" in str(excinfo.value)


class _AsyncLocalProcess:
    async def exec(self, command: str, timeout: int | None = None):
        completed = await asyncio.to_thread(
            subprocess.run,
            command,
            shell=True,
            text=True,
            capture_output=True,
            timeout=timeout,
            check=False,
        )
        return SimpleNamespace(
            result=completed.stdout + completed.stderr,
            exit_code=completed.returncode,
        )
