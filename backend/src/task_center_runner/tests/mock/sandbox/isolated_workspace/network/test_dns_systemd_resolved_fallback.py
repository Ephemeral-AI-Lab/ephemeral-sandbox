"""Lowerdir resolv.conf points at 127.0.0.53 → daemon swaps in fallback.

The systemd-resolved stub at ``127.0.0.53`` is unreachable from inside
the workspace mntns (lo is up but the host's resolver process is not).
The daemon detects this and writes the configured ``fallback_dns``
(default ``1.1.1.1``) into the workspace's ``/etc/resolv.conf``.
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
async def test_dns_systemd_resolved_fallback(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    # Stage the systemd-resolved-style resolv.conf on the lowerdir.
    await raw_exec(
        sandbox_id,
        "cp /etc/resolv.conf /etc/resolv.conf.backup-iws-test 2>/dev/null; "
        "echo 'nameserver 127.0.0.53' > /etc/resolv.conf",
        cwd="/", timeout=10,
    )
    try:
        enter = await _iws_rpc.enter(
            sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
        )
        assert enter.get("success") is True, enter
        try:
            shown = await _iws_rpc.shell(
                sandbox_id, "agent-A", "cat /etc/resolv.conf",
            )
            content = shown.get("stdout", "") or ""
            assert "127.0.0.53" not in content, (
                "fallback must replace the stub", content,
            )
            assert "1.1.1.1" in content or "8.8.8.8" in content, content
        finally:
            await _iws_rpc.exit_(sandbox_id, "agent-A")
    finally:
        await raw_exec(
            sandbox_id,
            "mv /etc/resolv.conf.backup-iws-test /etc/resolv.conf 2>/dev/null || true",
            cwd="/", timeout=10,
        )
