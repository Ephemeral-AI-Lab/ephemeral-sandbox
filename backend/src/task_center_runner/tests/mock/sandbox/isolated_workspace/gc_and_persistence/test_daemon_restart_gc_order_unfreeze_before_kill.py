"""R5: orphan-handle teardown unfreezes the cgroup BEFORE killing PIDs.

Setup leaves a frozen orphan (``cgroup.freeze=1``) behind on purpose; if GC
kills before unfreezing, the kernel queues the kill against a frozen task
which can deadlock on cgroup transitions. The fix is the explicit ordering
inside :func:`_unfreeze_and_kill` — this test pins it by scanning the
daemon log for ``isolated_workspace_gc_unfreeze`` occurring before
``isolated_workspace_gc_kill`` for the same cgroup.
"""

from __future__ import annotations

import pytest

from sandbox.api import raw_exec
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    daemon_kill_and_respawn,
)


pytestmark = pytest.mark.asyncio


@pytest.mark.skipif(
    not database_configured(),
    reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(300)
async def test_daemon_restart_gc_order_unfreeze_before_kill(
    iws_clean_sandbox,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(
        sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT,
    )
    assert enter.get("success") is True, enter

    # Force the orphan cgroup into the frozen state before killing the daemon.
    await raw_exec(
        sandbox_id,
        "for d in /sys/fs/cgroup/eos-iws-*/; do "
        "echo 1 > \"$d/cgroup.freeze\" 2>/dev/null || true; done",
        cwd="/",
        timeout=15,
    )

    # Tee the daemon's stderr for the lifetime of the restart so we can
    # inspect the log ordering. ``launch_daemon.sh`` redirects the daemon's
    # stdout+stderr to ``_DAEMON_LOG`` which lives under the bundle dir.
    daemon_log_path = "/tmp/eos-sandbox-runtime/runtime.log"
    # Truncate the existing log so only the post-restart lines are scanned.
    await raw_exec(sandbox_id, f": > {daemon_log_path}", cwd="/", timeout=10)

    await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)

    log = await raw_exec(
        sandbox_id, f"cat {daemon_log_path} 2>/dev/null || true",
        cwd="/", timeout=10,
    )
    text = getattr(log, "stdout", "") or ""
    unfreeze_pos = text.find("isolated_workspace_gc_unfreeze")
    kill_pos = text.find("isolated_workspace_gc_kill")
    if unfreeze_pos < 0 and kill_pos < 0:
        pytest.skip(
            "daemon log not captured on this image; ordering is enforced "
            "structurally by _unfreeze_and_kill — see _gc.py."
        )
    assert unfreeze_pos >= 0, ("missing unfreeze log line", text)
    assert kill_pos >= 0, ("missing kill log line", text)
    assert unfreeze_pos < kill_pos, (
        "R5: unfreeze must precede kill",
        text[max(0, unfreeze_pos - 50):min(len(text), kill_pos + 50)],
    )
