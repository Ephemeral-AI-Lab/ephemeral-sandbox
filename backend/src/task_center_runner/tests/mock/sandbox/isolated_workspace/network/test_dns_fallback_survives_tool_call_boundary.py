"""The DNS fallback persists across the mntns's lifetime — not just one call.

The bind-mount/write that configure_dns applies must outlast a single
tool call. The fallback content must still be visible from a second
``read /etc/resolv.conf`` after an unrelated intervening tool call.
"""

from __future__ import annotations

import pytest

from sandbox.api import raw_exec
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(180)
async def test_dns_fallback_survives_tool_call_boundary(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    await raw_exec(
        sandbox_id,
        "cp /etc/resolv.conf /etc/resolv.conf.backup-iws-test 2>/dev/null; "
        "echo 'nameserver 127.0.0.53' > /etc/resolv.conf",
        cwd="/", timeout=10,
    )
    try:
        await _iws_rpc.enter(sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
        try:
            first = await _iws_rpc.shell(
                sandbox_id, "agent-A", "cat /etc/resolv.conf",
            )
            await _iws_rpc.shell(sandbox_id, "agent-A", "true")
            second = await _iws_rpc.shell(
                sandbox_id, "agent-A", "cat /etc/resolv.conf",
            )
            assert first.get("stdout") == second.get("stdout"), (first, second)
            assert "127.0.0.53" not in (second.get("stdout") or ""), second
        finally:
            await _iws_rpc.exit_(sandbox_id, "agent-A")
    finally:
        await raw_exec(
            sandbox_id,
            "mv /etc/resolv.conf.backup-iws-test /etc/resolv.conf 2>/dev/null || true",
            cwd="/", timeout=10,
        )
