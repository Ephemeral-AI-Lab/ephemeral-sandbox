"""Symlinked resolv.conf — detection must follow it INSIDE the new mntns.

If the lowerdir ships ``/etc/resolv.conf`` as a symlink to
``/run/systemd/resolve/stub-resolv.conf`` (or another path whose
content is mntns-specific), the daemon must resolve the symlink AFTER
setns(CLONE_NEWNS) — otherwise it inspects the daemon-host file by
mistake and skips the fallback.
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
async def test_dns_symlinked_resolv_conf(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    # Stage a stub-style symlink target reachable inside the new mntns.
    await raw_exec(
        sandbox_id,
        "cp /etc/resolv.conf /etc/resolv.conf.backup-iws-test 2>/dev/null; "
        "mkdir -p /run/systemd/resolve && "
        "echo 'nameserver 127.0.0.53' > /run/systemd/resolve/stub-resolv.conf && "
        "ln -sf /run/systemd/resolve/stub-resolv.conf /etc/resolv.conf",
        cwd="/", timeout=15,
    )
    try:
        await _iws_rpc.enter(sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
        try:
            shown = await _iws_rpc.shell(
                sandbox_id, "agent-A", "cat /etc/resolv.conf",
            )
            content = shown.get("stdout", "") or ""
            assert "127.0.0.53" not in content, (
                "symlink-followed detection must trigger fallback", content,
            )
        finally:
            await _iws_rpc.exit_(sandbox_id, "agent-A")
    finally:
        await raw_exec(
            sandbox_id,
            "rm -f /etc/resolv.conf && "
            "mv /etc/resolv.conf.backup-iws-test /etc/resolv.conf 2>/dev/null || true",
            cwd="/", timeout=10,
        )
