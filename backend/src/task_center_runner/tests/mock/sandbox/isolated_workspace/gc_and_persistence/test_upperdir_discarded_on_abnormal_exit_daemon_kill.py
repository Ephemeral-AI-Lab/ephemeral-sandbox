"""Abnormal daemon exit: ``startup_gc`` recurses into scratch_root.

Write 5 MB to upperdir; SIGKILL daemon (no graceful exit, no rmtree on
the way out); restart. After GC: no scratch dir for the dead handle, no
veth, no cgroup, no leaked IP allocation. The recurse-into-scratch_root
is the critical fix — without it the upperdir survives the crash.
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
    iws_scratch_root,
    list_host_eos_iws_resources,
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
@pytest.mark.timeout(360)
async def test_upperdir_discarded_on_abnormal_exit_daemon_kill(
    iws_clean_sandbox,
) -> None:
    sandbox_id = str(iws_clean_sandbox["sandbox_id"])
    enter = await _iws_rpc.enter(sandbox_id, "agent-A", layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)
    assert enter.get("success") is True, enter
    # Write 5 MB to upperdir before the crash.
    write = await _iws_rpc.shell(
        sandbox_id, "agent-A",
        "dd if=/dev/zero of=/testbed/scratch.bin bs=1M count=5 2>/dev/null",
    )
    assert write.get("success") is True, write

    scratch = await iws_scratch_root(sandbox_id)
    before = await raw_exec(
        sandbox_id,
        f"du -sb {scratch} 2>/dev/null | awk '{{print $1}}'",
        cwd="/",
        timeout=20,
    )
    before_bytes = int((getattr(before, "stdout", "") or "0").strip() or "0")
    assert before_bytes > 5 * 1024 * 1024, (before_bytes, before)

    # Abnormal exit + restart triggers startup_gc.
    await daemon_kill_and_respawn(sandbox_id, layer_stack_root=_iws_rpc.IWS_LAYER_STACK_ROOT)

    after_state = await list_host_eos_iws_resources(sandbox_id)
    assert after_state["veth"] == [], after_state
    assert after_state["cgroup"] == [], after_state

    after = await raw_exec(
        sandbox_id,
        f"find {scratch} -mindepth 1 -not -name manager.json "
        f"-not -path '*agent-restart*' 2>/dev/null || true",
        cwd="/",
        timeout=20,
    )
    leftover = (getattr(after, "stdout", "") or "").strip()
    assert leftover == "", (
        "startup_gc must rmtree pre-crash scratch dirs", leftover,
    )
