"""End-to-end network + package: pip install then http GET via httpx.

Whole stack: DNS + MASQUERADE + bridge + outbound HTTPS + cross-tool-call
package availability. Requires the public internet.
"""

from __future__ import annotations

import pytest

from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc


pytestmark = [pytest.mark.asyncio, pytest.mark.live_e2e_soak]


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(600)
async def test_pip_install_then_run_e2e(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    opened = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert opened.get("success") is True, opened
    try:
        install = await _iws_rpc.shell(
            sandbox_id, "agent-A",
            "pip install --target /tmp/pkg --quiet httpx 2>&1 | tail -5",
        )
        assert install.get("success") is True, install

        run = await _iws_rpc.shell(
            sandbox_id, "agent-A",
            "PYTHONPATH=/tmp/pkg python3 -c "
            "\"import httpx; print(httpx.get('https://httpbin.org/get', timeout=15).status_code)\"",
        )
        assert run.get("success") is True, run
        assert "200" in (run.get("stdout", "") or ""), run
    finally:
        await _iws_rpc.exit_(sandbox_id, "agent-A")
