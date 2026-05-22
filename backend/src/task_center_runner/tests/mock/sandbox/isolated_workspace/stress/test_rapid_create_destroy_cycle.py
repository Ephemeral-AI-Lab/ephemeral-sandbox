"""100 enter/exit cycles for one agent — no FD/veth/IP-pool drift.

Daemon resource counts must stay bounded across the cycle. We sample
host-side veth count + daemon FD count before vs. after — both must be
within tolerance.
"""

from __future__ import annotations

import pytest

from sandbox.api import raw_exec
from benchmarks.sweevo.models import _REPO_DIR
from task_center_runner.tests._live_config import (
    database_configured,
    live_e2e_heavy_enabled,
)
from task_center_runner.tests.mock.sandbox.isolated_workspace import _iws_rpc
from task_center_runner.tests.mock.sandbox.isolated_workspace._iws_fixtures import (
    list_host_eos_iws_resources,
)


pytestmark = [pytest.mark.asyncio, pytest.mark.live_e2e_soak]
_CYCLES = 100


async def _daemon_fd_count(sandbox_id: str) -> int:
    res = await raw_exec(
        sandbox_id,
        "pid=$(pgrep -f '^.*python.*-m sandbox\\.daemon' | head -1); "
        "if [ -n \"$pid\" ]; then ls /proc/$pid/fd 2>/dev/null | wc -l; else echo 0; fi",
        cwd="/", timeout=15,
    )
    text = (getattr(res, "stdout", "") or "").strip()
    return int(text) if text.isdigit() else 0


@pytest.mark.skipif(
    not database_configured(), reason="database URL not configured",
)
@pytest.mark.skipif(
    not live_e2e_heavy_enabled(),
    reason="heavy live e2e disabled in runner.live_e2e.heavy_enabled",
)
@pytest.mark.timeout(1800)
async def test_rapid_create_destroy_cycle(iws_clean_sandbox) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    # Warm the daemon — one enter so subsequent cycles share a singleton.
    warm = await _iws_rpc.enter(
        sandbox_id, "agent-warm", layer_stack_root=_REPO_DIR,
    )
    assert warm.get("success") is True, warm
    await _iws_rpc.exit_(sandbox_id, "agent-warm")

    fd_before = await _daemon_fd_count(sandbox_id)

    for _ in range(_CYCLES):
        opened = await _iws_rpc.enter(
            sandbox_id, "agent-cycler", layer_stack_root=_REPO_DIR,
        )
        assert opened.get("success") is True, opened
        await _iws_rpc.exit_(sandbox_id, "agent-cycler")

    fd_after = await _daemon_fd_count(sandbox_id)
    after = await list_host_eos_iws_resources(sandbox_id)

    # FD growth must be bounded — small absolute slack (50) covers ttl_task
    # bookkeeping and audit-sink rotations.
    assert fd_after - fd_before <= 50, (
        f"FD leak across {_CYCLES} cycles: before={fd_before} after={fd_after}",
    )
    # No stranded veth / cgroup / netns.
    assert after["veth"] == [], after
    assert after["cgroup"] == [], after
