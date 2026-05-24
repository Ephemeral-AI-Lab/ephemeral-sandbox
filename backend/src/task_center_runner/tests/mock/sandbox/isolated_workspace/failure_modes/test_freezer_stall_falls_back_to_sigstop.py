"""R11: cgroup.freeze EACCES → per-PID SIGSTOP fallback + ``freezer_degraded``.

We chmod ``cgroup.freeze`` to ``000`` after enter; the next tool call's
freeze must fall back to walking ``cgroup.procs`` and SIGSTOPping each
PID. The handle's ``freezer_degraded`` flag flips to True (visible via
``status``).
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
@pytest.mark.timeout(240)
async def test_freezer_stall_falls_back_to_sigstop(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter
    try:
        # Shadow cgroup.freeze with /dev/null so writes silently succeed
        # but read-back returns "" (≠ "1"). chmod 000 doesn't work for the
        # daemon: it runs as root with CAP_DAC_OVERRIDE which bypasses DAC
        # checks, so the chmod is invisible and the freezer actually does
        # transition. Bind-mounting /dev/null over the file is the
        # production-shaped scenario the R11 read-back check is designed
        # to detect.
        await raw_exec(
            sandbox_id,
            "for f in /sys/fs/cgroup/eos-iws-*/cgroup.freeze; do "
            "mount --bind /dev/null \"$f\" 2>/dev/null || true; done",
            cwd="/", timeout=10,
        )

        # A tool call now exercises freeze → fallback path.
        result = await _iws_rpc.shell(sandbox_id, "agent-A", "true")
        assert result.get("success") is True, result

        status = await _iws_rpc.status(sandbox_id, "agent-A")
        assert status.get("freezer_degraded") is True, (
            "R11: freezer_degraded must be set after SIGSTOP fallback", status,
        )
    finally:
        # Unmount the bind so cleanup paths can rmdir the cgroup. Iterate
        # umount in case mount --bind layered multiple times across a
        # retried test.
        await raw_exec(
            sandbox_id,
            "for f in /sys/fs/cgroup/eos-iws-*/cgroup.freeze; do "
            "while mountpoint -q \"$f\" 2>/dev/null; do "
            "umount \"$f\" 2>/dev/null || break; done; done",
            cwd="/", timeout=10,
        )
        await _iws_rpc.exit_(sandbox_id, "agent-A")
